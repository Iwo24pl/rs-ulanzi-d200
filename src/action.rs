use async_trait::async_trait;
use log::info;
use openaction::*;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default)]
pub struct CycleStatusWindowSettings {
    pub current: u8,
}

pub struct CycleStatusWindow {
    pub cycle_tx: mpsc::Sender<()>,
}

#[async_trait]
impl Action for CycleStatusWindow {
    const UUID: ActionUuid = "io.github.Iwo24pl.rs-ulanzi-d200.cycle";
    type Settings = CycleStatusWindowSettings;

    async fn key_up(
        &self,
        _instance: &Instance,
        _settings: &Self::Settings,
    ) -> OpenActionResult<()> {
        info!("Cycling display mode");
        crate::tlog!("ACTION cycle key_up received");
        let _ = self.cycle_tx.send(()).await;
        Ok(())
    }

    async fn will_appear(&self, _: &Instance, _: &Self::Settings) -> OpenActionResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// OpenDeck resolves actions by UUID, so the runtime UUID, the manifest
    /// entry and the plugin id must be renamed together.
    #[test]
    fn action_uuid_matches_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("manifest.json")).expect("valid manifest");
        let plugin_uuid = manifest["PluginUUID"].as_str().expect("PluginUUID");
        let action_uuid = <CycleStatusWindow as Action>::UUID;

        assert_eq!(plugin_uuid, "io.github.Iwo24pl.rs-ulanzi-d200");
        assert_eq!(action_uuid, format!("{plugin_uuid}.cycle"));
        assert_eq!(
            manifest["Actions"][0]["UUID"]
                .as_str()
                .expect("action UUID"),
            action_uuid
        );
    }
}
