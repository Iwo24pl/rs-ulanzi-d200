use anyhow::Result;
use async_hid::AsyncHidRead;
use log::{debug, error, info, warn};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::signal;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep_until};

use crate::config::WINDOW_MODE_STATUS;
use crate::config::WINDOW_MODE_CLOCK;
use crate::config::WINDOW_MODE_CLEAR;
use crate::config::Config;
use crate::device::{ButtonEvent, UlanziDevice, NUM_BUTTONS};
use crate::openaction_client::BridgeEvent;
use crate::system_monitor::SystemMonitor;

#[derive(Debug, Clone)]
pub enum HardwareEvent {
    KeyDown { device_id: String, key_index: u8 },
    KeyUp { device_id: String, key_index: u8 },
    DeviceConnected { device_id: String },
}

pub struct UlanziDaemon {
    devices: HashMap<String, UlanziDevice>,
    config: Config,
    system_monitor: SystemMonitor,
    cpu_usage: u8,
    mem_usage: u8,
    gpu_usage: u8,
    plugin_cmd_rx: Option<mpsc::Receiver<BridgeEvent>>,
    hw_event_tx: Option<mpsc::Sender<HardwareEvent>>,
    device_input_rx: mpsc::Receiver<(String, ButtonEvent)>,
    device_input_tx: mpsc::Sender<(String, ButtonEvent)>,
    // Debouncing & rate limiting
    flush_deadline: Option<Instant>,
    last_flush_time: Option<Instant>,
    debounce_delay: Duration,
    min_flush_interval: Duration,
    // Cycle command channel
    cycle_rx: mpsc::Receiver<()>,
    // Last small-window frame sent (mode, cpu, mem, time, gpu).
    // The D200 flickers if it receives redundant frames too fast
    // (upstream 0.6.1), so the keep-alive skips unchanged frames.
    last_window: Option<(u8, u8, u8, String, u8)>,
    // Last forwarded state per button â€” suppresses duplicate press/release
    // reports (phantom re-triggers) without affecting genuine taps.
    pressed_state: [Option<bool>; NUM_BUTTONS],
    // True when staged button images changed and still need a ZIP upload.
    images_dirty: bool,
}

impl UlanziDaemon {
    pub async fn new(
        config: Config,
        plugin_cmd_rx: Option<mpsc::Receiver<BridgeEvent>>,
        hw_event_tx: Option<mpsc::Sender<HardwareEvent>>,
        cycle_rx: mpsc::Receiver<()>,
    ) -> Result<Self> {
        let (device_input_tx, device_input_rx) = mpsc::channel(100);
        let mut devices = HashMap::new();

        // Try to connect to the first available device
        match UlanziDevice::connect().await {
            Ok(device) => {
                devices.insert(device.get_id().to_string(), device);
            }
            Err(e) => {
                warn!("No devices found at startup: {}", e);
            }
        }

        let system_monitor = SystemMonitor::new();

        Ok(Self {
            devices,
            config,
            system_monitor,
            cpu_usage: 0,
            mem_usage: 0,
            gpu_usage: 0,
            plugin_cmd_rx,
            hw_event_tx,
            device_input_rx,
            device_input_tx,
            flush_deadline: None,
            last_flush_time: None,
            debounce_delay: Duration::from_millis(50),
            min_flush_interval: Duration::from_millis(20),
            cycle_rx,
            last_window: None,
            pressed_state: [None; NUM_BUTTONS],
            images_dirty: false,
        })
    }

    pub async fn run(mut self) -> Result<()> {
        info!("Ulanzi Daemon started (debounced flush)");

        // --- Initial device setup for all connected devices ---
        for device in self.devices.values_mut() {
            // 1. Clear the screen (all 14 buttons empty)
            if let Err(e) = device.clear_all_images().await {
                error!("Failed to clear buttons for {}: {}", device.get_id(), e);
            }

            // 2. Apply brightness and label style from config
            if let Err(e) = device.set_brightness(self.config.brightness).await {
                error!("Failed to set brightness for {}: {}", device.get_id(), e);
            }
            if let Ok(label_style) = serde_json::to_value(&self.config.label_style) {
                let _ = device.set_label_style(&label_style).await;
            }

            // 3. Start the small window with the current time
            let startup_time = chrono::Local::now().format("%H:%M:%S").to_string();
            let _ = device
                .set_small_window_data(self.config.display_mode, 0, 0, &startup_time, 0)
                .await;

            // 4. Notify plugins that a device is connected
            if let Some(ref tx) = self.hw_event_tx {
                let _ = tx
                    .send(HardwareEvent::DeviceConnected {
                        device_id: device.get_id().to_string(),
                    })
                    .await;
            }

            // 5. Spawn reader task for button events
            if let Some(mut reader) = device.take_reader() {
                let tx = self.device_input_tx.clone();
                let device_id = device.get_id().to_string();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    loop {
                        match reader.read_input_report(&mut buf).await {
                            Ok(len) if len > 0 => {
                                let hex: String = buf[..len.min(24)]
                                    .iter()
                                    .map(|b| format!("{:02x} ", b))
                                    .collect();
                                match UlanziDevice::parse_report(&buf[..len]) {
                                    Some(event) => {
                                        crate::tlog!(
                                            "RAW {} {}| idx={} pressed={}",
                                            device_id,
                                            hex.trim_end(),
                                            event.index,
                                            event.pressed
                                        );
                                        if tx.send((device_id.clone(), event)).await.is_err() {
                                            break;
                                        }
                                    }
                                    None => {
                                        crate::tlog!(
                                            "RAW {} {}| (unhandled)",
                                            device_id,
                                            hex.trim_end()
                                        );
                                    }
                                }
                            }
                            Ok(_) => continue,
                            Err(e) => {
                                error!("Device {} read error: {}", device_id, e);
                                break;
                            }
                        }
                    }
                    info!("Reader task finished for {}", device_id);
                });
            }
        }

        // --- Drain any initial plugin commands that arrived before the main loop ---
        let mut initial_commands = Vec::new();
        if let Some(rx) = &mut self.plugin_cmd_rx {
            while let Ok(cmd) = rx.try_recv() {
                initial_commands.push(cmd);
            }
        }
        if !initial_commands.is_empty() {
            info!(
                "Processing {} initial plugin commands",
                initial_commands.len()
            );
            for cmd in initial_commands {
                self.handle_plugin_command(cmd).await;
            }
            // Schedule a flush after the initial batch
            self.schedule_flush();
        }

        // --- Timers and shutdown signal ---
        let mut keep_alive_interval = interval(Duration::from_millis(100));
        let mut system_monitor_interval =
            interval(Duration::from_millis(self.config.stats_interval_ms));

        let shutdown = async {
            #[cfg(unix)]
            {
                let mut sigint =
                    signal::unix::signal(signal::unix::SignalKind::interrupt()).unwrap();
                let mut sigterm =
                    signal::unix::signal(signal::unix::SignalKind::terminate()).unwrap();
                tokio::select! {
                    _ = sigint.recv() => info!("Received SIGINT, shutting down..."),
                    _ = sigterm.recv() => info!("Received SIGTERM, shutting down..."),
                }
            }
            #[cfg(windows)]
            {
                let _ = signal::ctrl_c().await;
                info!("Received Ctrl-C, shutting down...");
            }
        };
        tokio::pin!(shutdown);

        // --- Main event loop ---
        loop {
            // Copy the current deadline (if any) so the future doesn't borrow self.
            let deadline = self.flush_deadline;

            tokio::select! {
                _ = &mut shutdown => break,

                // Handle WebSocket commands from OpenDeck plugin
                Some(cmd) = async {
                    if let Some(rx) = &mut self.plugin_cmd_rx {
                        rx.recv().await
                    } else {
                        std::future::pending::<Option<BridgeEvent>>().await
                    }
                } => {
                    // Collect all pending commands
                    let mut commands = vec![cmd];
                    if let Some(rx) = &mut self.plugin_cmd_rx {
                        while let Ok(c) = rx.try_recv() {
                            commands.push(c);
                        }
                    }

                    for cmd in commands {
                        self.handle_plugin_command(cmd).await;
                    }
                    self.schedule_flush();
                }

                // Flush deadline reached (debounced)
                _ = async move {
                    if let Some(d) = deadline {
                        sleep_until(d.into()).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if deadline.is_some() => {
                    self.perform_flush().await;
                }

                // Keepâ€‘alive: update small window with current stats.
                // Send only when the frame actually changed (the clock ticks
                // ~1/s, stats ~1/s) â€” redundant 10/s writes make the
                // firmware display flicker.
                _ = keep_alive_interval.tick() => {
                    use chrono::Local;
                    let now = Local::now();
                    let time_str = now.format("%H:%M:%S").to_string();
                    let frame = (
                        self.config.display_mode,
                        self.cpu_usage,
                        self.mem_usage,
                        time_str,
                        self.gpu_usage,
                    );
                    if self.last_window.as_ref() == Some(&frame) {
                        continue;
                    }
                    let (mode, cpu, mem, time_s, gpu) = &frame;
                    for device in self.devices.values() {
                        if let Err(e) = device.set_small_window_data(
                            *mode,
                            *cpu,
                            *mem,
                            time_s,
                            *gpu,
                        ).await {
                            debug!("Failed to send keep-alive to {}: {}", device.get_id(), e);
                        }
                    }
                    self.last_window = Some(frame);
                }

                // Forward hardware button events to plugins
                Some((device_id, event)) = self.device_input_rx.recv() => {
                    self.handle_device_event(&device_id, event).await;
                }

                // Update system_monitor every `stats_interval_ms`
                _ = system_monitor_interval.tick() => {
                    let (cpu, mem, gpu) = self.system_monitor.get_metrics();
                    self.cpu_usage = cpu;
                    self.mem_usage = mem;
                    self.gpu_usage = gpu;
                }

                // Handle cycle command from action
                Some(()) = self.cycle_rx.recv() => {
                    self.cycle_small_window().await;
                }
            }
        }

        info!("Shutdown complete.");
        Ok(())
    }

    /// Schedule a flush after the debounce delay, respecting the minimum interval
    /// since the last flush.
    fn schedule_flush(&mut self) {
        let now = Instant::now();
        let mut deadline = now + self.debounce_delay;
        if let Some(last) = self.last_flush_time {
            let earliest = last + self.min_flush_interval;
            if deadline < earliest {
                deadline = earliest;
            }
        }
        self.flush_deadline = Some(deadline);
    }

    /// Perform the actual flush (send all staged button images to the device).
    /// Uses a timeout to avoid hanging, and logs errors without crashing.
    async fn perform_flush(&mut self) {
        self.flush_deadline = None;

        // Skip the upload entirely when no image actually changed: a full
        // ZIP re-send makes the firmware reload every icon and repaint the
        // display, which briefly shows the *previous* small-window content
        // (the "two apps are fighting for the screen" flash).
        if !self.images_dirty {
            crate::tlog!("FLUSH skipped (no image changed)");
            return;
        }
        crate::tlog!("FLUSH begin");

        // Wrap the flush operation in a timeout.
        let flush_future = async {
            for device in self.devices.values() {
                if let Err(e) = device.flush().await {
                    info!("Failed to flush device {}: {}", device.get_id(), e);
                    // Continue with other devices (if any) â€“ don't break.
                }
            }
            self.last_flush_time = Some(Instant::now());
            debug!("Flush completed (or attempted)");
        };

        match tokio::time::timeout(Duration::from_secs(2), flush_future).await {
            Ok(_) => {
                // Flush finished within timeout (success or logged error).
            }
            Err(_) => {
                info!("Flush timed out after 2 seconds â€“ device may be stuck");
                // Still update last_flush_time to avoid immediate retry storms.
                self.last_flush_time = Some(Instant::now());
            }
        }
        self.images_dirty = false;
        // The repaint can clobber the small window â€” force the keep-alive
        // to re-send the current frame within 100 ms.
        self.last_window = None;
        crate::tlog!("FLUSH done");
    }

    async fn handle_device_event(&mut self, device_id: &str, event: ButtonEvent) {
        debug!("Button event from {}: {:?}", device_id, event);
        // State dedup: the same edge twice in a row is a duplicate report
        // (real presses always alternate press/release).
        if self.pressed_state[event.index] == Some(event.pressed) {
            crate::tlog!(
                "DEDUP {} idx={} (already {})",
                device_id,
                event.index,
                if event.pressed { "pressed" } else { "released" }
            );
            return;
        }
        self.pressed_state[event.index] = Some(event.pressed);
        crate::tlog!(
            "HW {} idx={} {}",
            device_id,
            event.index,
            if event.pressed { "DOWN" } else { "UP" }
        );
        if let Some(ref tx) = self.hw_event_tx {
            let outbound = if event.pressed {
                HardwareEvent::KeyDown {
                    device_id: device_id.to_string(),
                    key_index: event.index as u8,
                }
            } else {
                HardwareEvent::KeyUp {
                    device_id: device_id.to_string(),
                    key_index: event.index as u8,
                }
            };
            if let Err(e) = tx.send(outbound).await {
                warn!("Failed to broadcast hardware event: {}", e);
            }
        }
    }

    async fn handle_plugin_command(&mut self, cmd: BridgeEvent) {
        match cmd {
            BridgeEvent::SetImage {
                device_id,
                position,
                image_base64,
            } => {
                let dev = if let Some(d) = self.devices.get_mut(&device_id) {
                    Some(d)
                } else {
                    self.devices.values_mut().next()
                };
                if let Some(dev) = dev {
                    let index = position as usize;
                    debug!("Setting image for button {} on {}", index, dev.get_id());
                    match dev.set_button_image(index, &image_base64).await {
                        Ok(true) => {
                            self.images_dirty = true;
                            crate::tlog!(
                                "CMD SetImage device={} position={} len={} (staged)",
                                dev.get_id(),
                                index,
                                image_base64.len()
                            );
                            debug!(
                                "SetImage: device={} position={} image_len={} (staged)",
                                dev.get_id(),
                                index,
                                image_base64.len()
                            );
                        }
                        Ok(false) => {
                            crate::tlog!(
                                "CMD SetImage position={} (unchanged, no flush)",
                                index
                            );
                            debug!("Image unchanged for button {}, skipping", index);
                        }
                        Err(e) => error!("Failed to set image: {}", e),
                    }
                } else {
                    warn!("SetImage: No target device found for {}", device_id);
                }
            }
            BridgeEvent::ClearImage {
                device_id,
                position,
            } => {
                let dev = if let Some(d) = self.devices.get_mut(&device_id) {
                    Some(d)
                } else {
                    self.devices.values_mut().next()
                };
                if let Some(dev) = dev {
                    let index = position as usize;
                    self.images_dirty = true;
                    crate::tlog!(
                        "CMD ClearImage device={} position={}",
                        dev.get_id(),
                        index
                    );
                    debug!(
                        "ClearImage: device={} position={} (staged)",
                        dev.get_id(),
                        index
                    );
                    dev.clear_button_image(index);
                } else {
                    warn!("ClearImage: No target device found for {}", device_id);
                }
            }
            BridgeEvent::SetBrightness {
                device_id,
                brightness,
            } => {
                let dev = if let Some(d) = self.devices.get_mut(&device_id) {
                    Some(d)
                } else {
                    self.devices.values_mut().next()
                };
                if let Some(dev) = dev {
                    crate::tlog!("CMD SetBrightness={}", brightness);
                    if let Err(e) = dev.set_brightness(brightness).await {
                        error!("Failed to set brightness: {}", e);
                    }
                } else {
                    warn!("SetBrightness: No target device found for {}", device_id);
                }
            }
            BridgeEvent::DeviceConnected(_) | BridgeEvent::DeviceDisconnected(_) => {}
        }
    }

    async fn cycle_small_window(&mut self) {
        let current = self.config.display_mode;
        let new_mode = match current {
            WINDOW_MODE_STATUS => WINDOW_MODE_CLOCK, // Status -> Clock
            WINDOW_MODE_CLOCK => WINDOW_MODE_CLEAR, // Clock -> Clear
            WINDOW_MODE_CLEAR => WINDOW_MODE_STATUS, // Clear -> Status
            _ => WINDOW_MODE_STATUS,
        };
        self.config.display_mode = new_mode;
        if let Err(e) = self.config.save() {
            warn!("Failed to save config: {}", e);
        }
        info!("Smallâ€‘window mode cycled: {:?} -> {:?}", current, new_mode);
        crate::tlog!("CYCLE mode {} -> {}", current, new_mode);

        // Convert to u8 for device command
        let mode_byte = new_mode as u8;
        let now = chrono::Local::now();
        let time_str = now.format("%H:%M:%S").to_string();
        let frame = (
            mode_byte,
            self.cpu_usage,
            self.mem_usage,
            time_str,
            self.gpu_usage,
        );
        for device in self.devices.values() {
            if let Err(e) = device
                .set_small_window_data(
                    frame.0,
                    frame.1,
                    frame.2,
                    &frame.3,
                    frame.4,
                )
                .await
            {
                warn!("Failed to update small window after mode cycle: {}", e);
            }
        }
        // The firmware renders the small window one frame behind the last
        // received payload: a single mode-change frame still paints the
        // *previous* display until the next frame arrives (up to 1 s later
        // via keep-alive), which reads as a flash. Send the frame once more
        // after a short delay so the switch is applied within ~60 ms.
        tokio::time::sleep(Duration::from_millis(60)).await;
        for device in self.devices.values() {
            if let Err(e) = device
                .set_small_window_data(
                    frame.0,
                    frame.1,
                    frame.2,
                    &frame.3,
                    frame.4,
                )
                .await
            {
                warn!("Failed to re-send small window frame: {}", e);
            }
        }
        // Remember what we just sent so the keep-alive doesn't
        // immediately re-send the identical frame.
        self.last_window = Some(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_handle_device_event_keydown() {
        let (hw_event_tx, mut hw_event_rx) = mpsc::channel(1);
        let config = Config::default();
        let (_device_input_tx, device_input_rx) = mpsc::channel(1);
        let (device_input_tx_internal, _device_input_rx_internal) = mpsc::channel(1);
        let (_cycle_tx, cycle_rx) = mpsc::channel(1);

        let mut daemon = UlanziDaemon {
            devices: HashMap::new(),
            config,
            system_monitor: SystemMonitor::new(),
            cpu_usage: 0,
            mem_usage: 0,
            gpu_usage: 0,
            plugin_cmd_rx: None,
            hw_event_tx: Some(hw_event_tx),
            device_input_rx,
            device_input_tx: device_input_tx_internal,
            flush_deadline: None,
            last_flush_time: None,
            debounce_delay: Duration::from_millis(50),
            min_flush_interval: Duration::from_millis(20),
            cycle_rx,
            last_window: None,
            pressed_state: [None; NUM_BUTTONS],
            images_dirty: false,
        };

        let event = ButtonEvent {
            index: 5,
            pressed: true,
            state: 1,
        };
        daemon.handle_device_event("test_device", event).await;

        let received = hw_event_rx.recv().await.unwrap();
        match received {
            HardwareEvent::KeyDown { device_id, key_index } => {
                assert_eq!(device_id, "test_device");
                assert_eq!(key_index, 5);
            }
            _ => panic!("Expected KeyDown event"),
        }
    }

    #[tokio::test]
    async fn test_handle_device_event_keyup() {
        let (hw_event_tx, mut hw_event_rx) = mpsc::channel(1);
        let config = Config::default();
        let (_device_input_tx, device_input_rx) = mpsc::channel(1);
        let (device_input_tx_internal, _device_input_rx_internal) = mpsc::channel(1);
        let (_cycle_tx, cycle_rx) = mpsc::channel(1);

        let mut daemon = UlanziDaemon {
            devices: HashMap::new(),
            config,
            system_monitor: SystemMonitor::new(),
            cpu_usage: 0,
            mem_usage: 0,
            gpu_usage: 0,
            plugin_cmd_rx: None,
            hw_event_tx: Some(hw_event_tx),
            device_input_rx,
            device_input_tx: device_input_tx_internal,
            flush_deadline: None,
            last_flush_time: None,
            debounce_delay: Duration::from_millis(50),
            min_flush_interval: Duration::from_millis(20),
            cycle_rx,
            last_window: None,
            pressed_state: [None; NUM_BUTTONS],
            images_dirty: false,
        };

        let event = ButtonEvent {
            index: 3,
            pressed: false,
            state: 0,
        };
        daemon.handle_device_event("test_device", event).await;

        let received = hw_event_rx.recv().await.unwrap();
        match received {
            HardwareEvent::KeyUp { device_id, key_index } => {
                assert_eq!(device_id, "test_device");
                assert_eq!(key_index, 3);
            }
            _ => panic!("Expected KeyUp event"),
        }
    }
}