mod config;
mod daemon;
mod device;
mod openaction_client;
mod system_monitor;
mod action;

use anyhow::Result;
use clap::Parser;
use log::{error, info, warn};
use std::path::PathBuf;
use openaction::*;
use tokio::sync::mpsc;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the configuration YAML file
    #[arg(short, long, default_value = "config.yaml")]
    config: PathBuf,

    /// Log level (info, debug, trace)
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Run as a daemon
    #[arg(short, long)]
    daemon: bool,

    /// Enable WebSocket server for plugin communication
    #[arg(long)]
    websocket: bool,

    /// WebSocket server port
    #[arg(long, short = 'p', default_value_t = 57116)]
    port: u16,

    /// Stream Deck / OpenDeck Registration: Plugin UUID
    #[arg(long = "pluginUUID", hide = true)]
    plugin_uuid: Option<String>,

    /// Stream Deck / OpenDeck Registration: Register Event
    #[arg(long = "registerEvent", hide = true)]
    register_event: Option<String>,

    /// Stream Deck / OpenDeck Registration: Info
    #[arg(long = "info", hide = true)]
    info: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Preprocess arguments to handle Stream Deck's single-dash parameters
    let args_iter = std::env::args().map(|arg| match arg.as_str() {
        "-port" => "--port".to_string(),
        "-pluginUUID" => "--pluginUUID".to_string(),
        "-registerEvent" => "--registerEvent".to_string(),
        "-info" => "--info".to_string(),
        _ => arg,
    });

    let args = Args::parse_from(args_iter);

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&args.log_level))
        .init();

    info!("Starting Ulanzi D200 Rust Driver (MQ-RUST-002)");

    // 1. Load configuration
    let config = match config::Config::load(&args.config) {
        Ok(c) => c,
        Err(e) => {
            if args.plugin_uuid.is_some() || args.daemon {
                warn!(
                    "Configuration file not found or invalid ({}), using defaults: {}",
                    args.config.display(),
                    e
                );
                config::Config::default()
            } else {
                error!("Failed to load configuration: {}", e);
                return Err(e);
            }
        }
    };

    // Create the cycle command channel
    let (cycle_tx, cycle_rx) = mpsc::channel(1);

    // 2. Determine Communication Mode
    let (plugin_cmd_rx, hw_event_tx, openaction_handle) = if let Some(ref uuid) = args.plugin_uuid {
        // PLUGIN MODE (Client)
        let port = args.port;
        let event = args
            .register_event
            .clone()
            .unwrap_or_else(|| "register".to_string());
        let info_str = args.info.clone().unwrap_or_else(|| "{}".to_string());

        info!("Starting in Plugin Mode (UUID: {}, Port: {})", uuid, port);

        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(100);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);

        // Shared with OpenActionBridge so `plugin_ready` can re-register
        // devices that connected before the websocket was ready.
        let known_devices: crate::openaction_client::KnownDevices =
            std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let bridge = crate::openaction_client::OpenActionBridge::new(
            cmd_tx,
            known_devices.clone(),
        );
        bridge.register();

        let exe_name = std::env::current_exe()
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "rs-ulanzi-d200-linux".to_string());

        let oa_args = vec![
            exe_name,
            "-port".to_string(),
            port.to_string(),
            "-pluginUUID".to_string(),
            uuid.clone(),
            "-registerEvent".to_string(),
            event,
            "-info".to_string(),
            info_str,
        ];

        // Keep handle so we can detect when OpenDeck disconnects
        let oa_handle = tokio::spawn(async move {
            info!("Running OpenAction Runtime...");
            if let Err(e) = openaction::run(oa_args).await {
                error!("OpenAction Runtime Error: {}", e);
            }
            info!("OpenAction Runtime exited");
        });

        // Register the action with the cycle sender
        let cycle_action = action::CycleStatusWindow { cycle_tx: cycle_tx.clone() };
        register_action(cycle_action).await;

        // Outbound events forwarder: HID -> OpenDeck.
        // NOTE: `register_device` is a silent no-op until `openaction::run`
        // has connected (outbound manager not yet set). We therefore remember
        // the ID first; `plugin_ready` re-registers from `known_devices`.
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    daemon::HardwareEvent::KeyDown {
                        device_id,
                        key_index,
                    } => {
                        if let Err(e) =
                            openaction::device_plugin::key_down(device_id.clone(), key_index).await
                        {
                            warn!("key_down forward failed for {}: {}", device_id, e);
                        }
                    }
                    daemon::HardwareEvent::KeyUp {
                        device_id,
                        key_index,
                    } => {
                        if let Err(e) =
                            openaction::device_plugin::key_up(device_id.clone(), key_index).await
                        {
                            warn!("key_up forward failed for {}: {}", device_id, e);
                        }
                    }
                    daemon::HardwareEvent::DeviceConnected { device_id } => {
                        crate::openaction_client::remember_device(&known_devices, &device_id).await;
                        info!("Registering device {} with OpenDeck", device_id);
                        if let Err(e) = openaction::device_plugin::register_device(
                            device_id.clone(),
                            crate::openaction_client::DEVICE_NAME.to_string(),
                            crate::openaction_client::DEVICE_ROWS,
                            crate::openaction_client::DEVICE_COLS,
                            0,
                            0,
                        )
                        .await
                        {
                            warn!("register_device failed for {}: {}", device_id, e);
                        }
                    }
                }
            }
        });

        (Some(cmd_rx), Some(event_tx), Some(oa_handle))
    } else if args.daemon && args.websocket {
        warn!("Server Mode is currently disabled/unsupported in this version.");
        (None, None, None)
    } else {
        (None, None, None)
    };

    // 3. Start Daemon or One-Shot
    if args.daemon || args.plugin_uuid.is_some() {
        let daemon = daemon::UlanziDaemon::new(config, plugin_cmd_rx, hw_event_tx, cycle_rx).await?;

        if let Some(oa_handle) = openaction_handle {
            // Plugin mode: exit when EITHER the daemon or the OpenAction runtime finishes
            tokio::select! {
                res = daemon.run() => {
                    info!("Daemon exited");
                    res?;
                }
                _ = oa_handle => {
                    info!("OpenAction runtime ended; shutting down driver");
                }
            }
        } else {
            // Standalone daemon mode: just run until daemon exits (Ctrl-C, etc)
            daemon.run().await?;
        }
    } else {
        info!("No mode specified. Use --daemon or --pluginUUID to start.");
    }
    Ok(())
}