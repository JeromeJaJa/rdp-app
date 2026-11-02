//! Layered connection settings: the app's settings, overridden by the connection's group,
//! overridden by the connection itself. Groups and connections store only the settings
//! they override, so changing an app-wide default reaches everything that doesn't.

use anyhow::bail;
use serde_json::{Map, Value};

/// Settings a group or a connection can override.
pub const OVERRIDABLE: &[&str] = &[
    "audio",
    "microphone",
    "clipboardSync",
    "driveRedirection",
    "drivePaths",
    "printerRedirection",
    "smartcardRedirection",
    "autoReconnect",
    "codec",
    "h264Quality",
    "videoDecoder",
    "remoteWallpaper",
    "companion",
    "hostStatsInterval",
    "directTransfers",
    "remoteCommands",
];

/// A group's or connection's overrides (only overridable keys).
pub type Overrides = Map<String, Value>;

/// Validates overrides: only overridable keys, with the same values the app setting
/// accepts. `null` entries mean "inherit" and are dropped.
pub fn clean(mut overrides: Overrides) -> anyhow::Result<Overrides> {
    overrides.retain(|_, v| !v.is_null());
    if let Some(key) = overrides.keys().find(|k| !OVERRIDABLE.contains(&k.as_str())) {
        bail!("{key} can't be set per group or connection");
    }
    crate::core::validate_settings(&overrides)?;
    Ok(overrides)
}

/// The app settings with each layer's overrides applied in order (group, then connection).
pub fn resolve(app: &Map<String, Value>, layers: &[&Overrides]) -> Map<String, Value> {
    let mut out = app.clone();
    for layer in layers {
        for (key, value) in layer.iter() {
            if OVERRIDABLE.contains(&key.as_str()) {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn connection_beats_group_beats_app() {
        let app = map(json!({"audio": "active", "microphone": false, "theme": "dark"}));
        let group = map(json!({"audio": "off", "microphone": true}));
        let conn = map(json!({"audio": "all"}));
        let out = resolve(&app, &[&group, &conn]);
        assert_eq!(out["audio"], "all");
        assert_eq!(out["microphone"], true, "from the group");
        assert_eq!(out["theme"], "dark", "untouched app setting");
    }

    #[test]
    fn overrides_are_validated() {
        assert!(clean(map(json!({"theme": "light"}))).is_err(), "not overridable");
        assert!(clean(map(json!({"audio": "loud"}))).is_err(), "bad value");
        let ok = clean(map(
            json!({"audio": "off", "microphone": null, "drivePaths": ["D:\\Work"]}),
        ))
        .unwrap();
        assert_eq!(ok.len(), 2, "null means inherit");
    }
}
