//! Where lD Mixer keeps its files: `$XDG_CONFIG_HOME/ld-mixer`.
//!
//! - `<model>.json`: the live mixer state, restored on the next start.
//! - `presets/<model>/<name>.json`: named snapshots of the whole mixer.
//!
//! - `settings.json`: app preferences that are not part of a mix.
//!
//! `ld-mixer uninstall` removes this directory.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::mixer::MixerState;
use crate::protocol::Profile;

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("ld-mixer")
}

/// App preferences that are not part of a mix (not stored in presets).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// Offer the hardware loopback to other apps as "iD24 Loopback".
    #[serde(default)]
    pub loopback_source: bool,
}

pub fn load_settings() -> Settings {
    fs::read_to_string(config_dir().join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(s: &Settings) -> Result<()> {
    write_atomic(&config_dir().join("settings.json"), &serde_json::to_string_pretty(s)?)
}

fn state_path(profile: &Profile) -> PathBuf {
    config_dir().join(format!("{}.json", profile.name))
}

fn preset_dir(profile: &Profile) -> PathBuf {
    config_dir().join("presets").join(profile.name)
}

/// Write `data` to `path` atomically, so a crash never leaves half a file.
fn write_atomic(path: &PathBuf, data: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Saved state for this model, if any.
pub fn load_state(profile: &Profile) -> Option<MixerState> {
    let text = fs::read_to_string(state_path(profile)).ok()?;
    match serde_json::from_str::<MixerState>(&text) {
        Ok(s) => Some(s.sanitize(profile)),
        Err(e) => {
            log::warn!("ignoring unreadable state file: {e}");
            None
        }
    }
}

pub fn save_state(profile: &Profile, state: &MixerState) -> Result<()> {
    write_atomic(&state_path(profile), &serde_json::to_string_pretty(state)?)
}

/// Preset names for this model, sorted.
pub fn list_presets(profile: &Profile) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(preset_dir(profile))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension()? != "json" {
                return None;
            }
            Some(p.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Turn a user-typed preset name into a safe file name.
pub fn sanitize_preset_name(name: &str) -> String {
    name.trim()
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

pub fn save_preset(profile: &Profile, name: &str, state: &MixerState) -> Result<String> {
    let name = sanitize_preset_name(name);
    anyhow::ensure!(!name.is_empty(), "preset name is empty");
    let path = preset_dir(profile).join(format!("{name}.json"));
    write_atomic(&path, &serde_json::to_string_pretty(state)?)?;
    Ok(name)
}

pub fn load_preset(profile: &Profile, name: &str) -> Result<MixerState> {
    let path = preset_dir(profile).join(format!("{}.json", sanitize_preset_name(name)));
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(serde_json::from_str::<MixerState>(&text)?.sanitize(profile))
}

pub fn delete_preset(profile: &Profile, name: &str) -> Result<()> {
    let path = preset_dir(profile).join(format!("{}.json", sanitize_preset_name(name)));
    fs::remove_file(path)?;
    Ok(())
}
