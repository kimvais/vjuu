use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};

use crate::platform;

/// The settings file's first line, for whoever opens it by hand.
const HEADER: &str = "# vjuu settings: change them with `vjuu config`, or the gear in the title bar.\n";

/// What `config.toml` holds; a missing key keeps its default.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The tray icon, and a notification when a review opens behind other windows. First: TOML puts plain keys
    /// before tables.
    pub tray: bool,
    pub hooks: Hooks,
}

impl Default for Config {
    fn default() -> Self {
        Self { tray: true, hooks: Hooks::default() }
    }
}

/// The Claude Code hooks, each switched on or off.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hooks {
    pub plan: bool,
    pub ask: bool,
    pub turn: bool,
}

impl Default for Hooks {
    fn default() -> Self {
        Self { plan: true, ask: true, turn: true }
    }
}

impl Config {
    /// Switches one setting by its `vjuu config` name.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let on = match value {
            "on" | "true" => true,
            "off" | "false" => false,
            _ => return Err(format!("Unknown value `{value}`: use on or off.")),
        };
        match key {
            "plan" => self.hooks.plan = on,
            "ask" => self.hooks.ask = on,
            "turn" => self.hooks.turn = on,
            "tray" => self.tray = on,
            _ => return Err(format!("Unknown setting `{key}`: use plan, ask, turn or tray.")),
        }
        Ok(())
    }

    pub fn to_toml(&self) -> String {
        HEADER.to_string() + &toml::to_string(self).expect("Config serialises")
    }
}

pub fn path() -> PathBuf {
    platform::config_dir().join("config.toml")
}

/// Returns the settings; a missing or broken file gives the defaults, so a typo never stops a review.
pub fn load() -> Config {
    let Ok(text) = fs::read_to_string(path()) else { return Config::default() };
    toml::from_str(&text).unwrap_or_else(|e| {
        eprintln!("Ignoring {}: {e}", path().display());
        Config::default()
    })
}

pub fn save(config: &Config) -> io::Result<()> {
    let path = path();
    fs::create_dir_all(path.parent().expect("config path has a folder"))?;
    fs::write(path, config.to_toml())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_stay_on() {
        let config: Config = toml::from_str("[hooks]\nturn = false\n").unwrap();
        assert_eq!(config, Config { tray: true, hooks: Hooks { plan: true, ask: true, turn: false } });
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }

    #[test]
    fn round_trips_with_a_header() {
        let mut config = Config::default();
        config.set("ask", "off").unwrap();
        config.set("tray", "off").unwrap();
        let text = config.to_toml();
        assert_eq!(text, format!("{HEADER}tray = false\n\n[hooks]\nplan = true\nask = false\nturn = true\n"));
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn rejects_unknown_settings_and_values() {
        let mut config = Config::default();
        for (key, value) in [("theme", "on"), ("plan", "yes"), ("", "")] {
            assert!(config.set(key, value).is_err(), "{key} {value}");
        }
        assert_eq!(config, Config::default());
    }
}
