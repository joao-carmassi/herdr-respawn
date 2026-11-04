//! services.json: the saved services, in the plugin config dir.
use crate::herdr::R;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Service {
    pub name: String,
    pub command: String,
    pub cwd: String,
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub workspace_label: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Store {
    pub version: u32,
    pub services: Vec<Service>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            version: 1,
            services: vec![],
            extra: Map::new(),
        }
    }
}

pub fn config_dir() -> R<PathBuf> {
    if let Ok(d) = std::env::var("HERDR_PLUGIN_CONFIG_DIR") {
        return Ok(d.into());
    }
    let bin = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into());
    let out = Command::new(bin)
        .args(["plugin", "config-dir", "herdr-respawn"])
        .output()?;
    if !out.status.success() {
        return Err("cannot find the config dir (is herdr on PATH?)".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().into())
}

/// Outside herdr there is no state dir; the config dir does for the lock file.
pub fn state_dir() -> R<PathBuf> {
    match std::env::var("HERDR_PLUGIN_STATE_DIR") {
        Ok(d) => Ok(d.into()),
        Err(_) => config_dir(),
    }
}

pub fn path() -> R<PathBuf> {
    Ok(config_dir()?.join("services.json"))
}

/// A missing file is an empty store; a file that does not parse is an error (and is never rewritten).
pub fn load_from(p: &Path) -> R<Store> {
    match std::fs::read_to_string(p) {
        Ok(s) => Ok(serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display()))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(e.into()),
    }
}

/// Atomic: write a temp file, then rename over the old one.
pub fn save_to(p: &Path, s: &Store) -> R<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(s)? + "\n")?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_unknown_fields() {
        let src = r#"{"version":1,"note":"x","services":[{"name":"a","command":"c","cwd":"d","color":"red"}]}"#;
        let s: Store = serde_json::from_str(src).unwrap();
        assert!(s.services[0].enabled);
        let back = serde_json::to_string(&s).unwrap();
        assert!(back.contains(r#""note":"x""#) && back.contains(r#""color":"red""#));
    }

    #[test]
    fn missing_file_is_empty_and_bad_json_errors() {
        let dir = std::env::temp_dir().join(format!("respawn-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("services.json");
        assert!(load_from(&p).unwrap().services.is_empty());
        std::fs::write(&p, "{nope").unwrap();
        assert!(load_from(&p).is_err());
        save_to(&p, &Store::default()).unwrap();
        assert!(load_from(&p).unwrap().services.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
