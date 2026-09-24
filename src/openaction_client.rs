use log::info;
use openaction::async_trait;
use openaction::global_events::{
    set_global_event_handler, DeviceDidConnectEvent, DeviceDidDisconnectEvent, GlobalEventHandler,
    SetBrightnessEvent, SetImageEvent,
};
use openaction::OpenActionResult;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// Device registration parameters shared between the forwarder and `plugin_ready`.
pub const DEVICE_NAME: &str = "Ulanzi D200";
pub const DEVICE_ROWS: u8 = 3;
pub const DEVICE_COLS: u8 = 5;

/// IDs of locally-attached devices, shared so `plugin_ready` can
/// (re-)register devices whose initial `registerDevice` was sent before
/// the OpenAction websocket was ready (silently dropped by `openaction`).
pub type KnownDevices = Arc<Mutex<Vec<String>>>;

pub async fn remember_device(devices: &KnownDevices, device_id: &str) {
    let mut guard = devices.lock().await;
    if !guard.iter().any(|d| d == device_id) {
        guard.push(device_id.to_string());
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum BridgeEvent {
    SetImage {
        device_id: String,
        position: u8,
        image_base64: String,
    },
    ClearImage {
        device_id: String,
        position: u8,
    },
    SetBrightness {
        device_id: String,
        brightness: u8,
    },
    #[allow(dead_code)]
    DeviceConnected(String),
    DeviceDisconnected(String),
}

pub struct OpenActionBridge {
    pub tx: mpsc::Sender<BridgeEvent>,
    pub known_devices: KnownDevices,
}

impl OpenActionBridge {
    pub fn new(tx: mpsc::Sender<BridgeEvent>, known_devices: KnownDevices) -> Self {
        Self { tx, known_devices }
    }

    pub fn register(self) {
        let leaked = Box::leak(Box::new(self));
        set_global_event_handler(leaked);
    }
}

#[async_trait]
impl GlobalEventHandler for OpenActionBridge {
    async fn plugin_ready(&self) -> OpenActionResult<()> {
        info!("OpenAction Bridge: Plugin Ready");
        // Re-register any devices that connected before the websocket was
        // ready. The initial registerDevice in that window is a no-op inside
        // `openaction` (outbound manager not yet set), which otherwise leaves
        // OpenDeck stuck on "no device connected".
        let devices = self.known_devices.lock().await.clone();
        for device_id in devices {
            info!("(Re-)registering device {} on plugin_ready", device_id);
            if let Err(e) = openaction::device_plugin::register_device(
                device_id.clone(),
                DEVICE_NAME.to_string(),
                DEVICE_ROWS,
                DEVICE_COLS,
                0,
                0,
            )
            .await
            {
                info!("Failed to (re-)register device {}: {}", device_id, e);
            }
        }
        Ok(())
    }

    async fn device_plugin_set_image(&self, event: SetImageEvent) -> OpenActionResult<()> {
        if let Some(pos) = event.position {
            let device_id = event.device.clone();
            if let Some(img) = event.image {
                let _ = self
                    .tx
                    .send(BridgeEvent::SetImage {
                        device_id,
                        position: pos,
                        image_base64: img,
                    })
                    .await;
            } else {
                let _ = self
                    .tx
                    .send(BridgeEvent::ClearImage {
                        device_id,
                        position: pos,
                    })
                    .await;
            }
        }
        Ok(())
    }

    async fn device_plugin_set_brightness(
        &self,
        event: SetBrightnessEvent,
    ) -> OpenActionResult<()> {
        let _ = self
            .tx
            .send(BridgeEvent::SetBrightness {
                device_id: event.device.clone(),
                brightness: event.brightness,
            })
            .await;
        Ok(())
    }

    async fn device_did_connect(&self, event: DeviceDidConnectEvent) -> OpenActionResult<()> {
        info!("Device Connected: {}", event.device);
        let _ = self
            .tx
            .send(BridgeEvent::DeviceConnected(event.device))
            .await;
        Ok(())
    }

    async fn device_did_disconnect(&self, event: DeviceDidDisconnectEvent) -> OpenActionResult<()> {
        info!("Device Disconnected: {}", event.device);
        let _ = self
            .tx
            .send(BridgeEvent::DeviceDisconnected(event.device))
            .await;
        Ok(())
    }
}
