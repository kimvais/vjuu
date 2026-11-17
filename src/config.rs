use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};

use crate::{platform, theme};

/// The settings file's first line, for whoever opens it by hand.
const HEADER: &str = "# vjuu settings: change them with `vjuu config`, or the gear in the title bar.\n";

/// What `config.toml` holds; a missing key keeps its default.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The tray icon, and a notification when a review opens behind other windows. First: TOML puts plain keys
    /// before tables.
    pub tray: bool,
    /// Where a new review opens.
    pub focus: Focus,
    /// What happens to vjuu's window on another virtual desktop when it comes to the front.
    pub desktop: Desktop,
    /// Where the agent's git worktrees live, relative to the repository root; the gate and the skills name it.
    pub worktrees: String,
    pub hooks: Hooks,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tray: true,
            focus: Focus::Front,
            desktop: Desktop::Follow,
            // Where Claude Code's own `--worktree` puts them.
            worktrees: ".claude/worktrees".into(),
            hooks: Hooks::default(),
            theme: Theme::default(),
        }
    }
}

/// Where a new review opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", from = "CompatFocus")]
pub enum Focus {
    /// In front, with keyboard focus.
    Front,
    /// Behind the window in use, with a notification.
    Behind,
    /// No window: only the notification and the tray dot.
    Hidden,
}

impl Focus {
    /// Returns where a new review opens in a vjuu that has a tray icon or not: `hidden` without one would leave
    /// nothing pointing at the review, so it opens `behind`.
    pub fn with_tray(self, tray: bool) -> Focus {
        match self {
            Focus::Hidden if !tray => Focus::Behind,
            focus => focus,
        }
    }
}

/// Reads `focus` as it was before it had three values: `true` / `false`. Scheduled for deletion.
#[derive(Deserialize)]
#[serde(untagged)]
enum CompatFocus {
    Bool(bool),
    Mode(#[serde(with = "FocusMode")] Focus),
}

/// `Focus` by name, for `CompatFocus`, which cannot use `Focus`'s own `Deserialize` without recursing.
#[derive(Deserialize)]
#[serde(remote = "Focus", rename_all = "lowercase")]
enum FocusMode {
    Front,
    Behind,
    Hidden,
}

impl From<CompatFocus> for Focus {
    fn from(value: CompatFocus) -> Self {
        match value {
            CompatFocus::Bool(true) => Focus::Front,
            CompatFocus::Bool(false) => Focus::Behind,
            CompatFocus::Mode(focus) => focus,
        }
    }
}

/// What happens to vjuu's window on another virtual desktop when it comes to the front.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Desktop {
    /// Move the window to the desktop in use.
    Follow,
    /// Switch to the window's desktop, as Windows does by itself.
    Switch,
}

/// Which themes (files in the themes folder, without `.css`) colour the windows.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// A theme, or "auto" to follow the OS between `light` and `dark`.
    pub name: String,
    pub light: String,
    pub dark: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self { name: theme::AUTO.into(), light: "light".into(), dark: "graphite".into() }
    }
}

/// The Claude Code hooks, each switched on or off.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hooks {
    pub plan: bool,
    pub ask: bool,
    pub turn: bool,
    pub permission: bool,
    /// The Stop hook that sends back a turn ending in a prose question.
    pub gate: bool,
}

impl Default for Hooks {
    fn default() -> Self {
        Self { plan: true, ask: true, turn: true, permission: true, gate: true }
    }
}

impl Config {
    /// Switches one setting by its `vjuu config` name.
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let on = || match value {
            "on" | "true" => Ok(true),
            "off" | "false" => Ok(false),
            _ => Err(format!("Unknown value `{value}`: use on or off.")),
        };
        let theme = |auto_ok: bool| {
            let themes: Vec<String> = theme::list().into_iter().map(|t| t.name).collect();
            if (auto_ok && value == theme::AUTO) || themes.iter().any(|t| t == value) {
                Ok(value.to_string())
            } else {
                Err(format!("Unknown theme `{value}`: use {}.", themes.join(", ")))
            }
        };
        match key {
            "plan" => self.hooks.plan = on()?,
            "ask" => self.hooks.ask = on()?,
            "turn" => self.hooks.turn = on()?,
            "permission" => self.hooks.permission = on()?,
            "gate" => self.hooks.gate = on()?,
            "worktrees" => {
                let folder = value.trim().trim_end_matches(['/', '\\']);
                if folder.is_empty()
                    || std::path::Path::new(folder).is_absolute()
                    // Not absolute to Windows, but outside the repository all the same.
                    || folder.starts_with(['/', '\\'])
                    || folder.split(['/', '\\']).any(|p| p == "..")
                {
                    return Err(format!(
                        "Unknown worktree folder `{value}`: use a folder inside the repository, such as .wt."
                    ));
                }
                self.worktrees = folder.to_string();
            }
            "tray" => self.tray = on()?,
            "focus" => {
                self.focus = match value {
                    "front" => Focus::Front,
                    "behind" => Focus::Behind,
                    "hidden" => Focus::Hidden,
                    _ => return Err(format!("Unknown value `{value}`: use front, behind or hidden.")),
                }
            }
            "desktop" => {
                self.desktop = match value {
                    "follow" => Desktop::Follow,
                    "switch" => Desktop::Switch,
                    _ => return Err(format!("Unknown value `{value}`: use follow or switch.")),
                }
            }
            "theme" => self.theme.name = theme(true)?,
            "theme.light" => self.theme.light = theme(false)?,
            "theme.dark" => self.theme.dark = theme(false)?,
            _ => {
                return Err(format!(
                    "Unknown setting `{key}`: use plan, ask, turn, permission, gate, tray, focus, desktop, worktrees, theme, theme.light or theme.dark."
                ));
            }
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
        assert_eq!(
            config,
            Config {
                tray: true,
                focus: Focus::Front,
                desktop: Desktop::Follow,
                worktrees: ".claude/worktrees".into(),
                hooks: Hooks { plan: true, ask: true, turn: false, permission: true, gate: true },
                theme: Theme::default()
            }
        );
        assert_eq!(toml::from_str::<Config>("").unwrap(), Config::default());
    }

    #[test]
    fn round_trips_with_a_header() {
        let mut config = Config::default();
        config.set("ask", "off").unwrap();
        config.set("tray", "off").unwrap();
        config.set("gate", "off").unwrap();
        config.set("worktrees", ".wt/").unwrap();
        config.set("focus", "hidden").unwrap();
        config.set("desktop", "switch").unwrap();
        let text = config.to_toml();
        assert_eq!(
            text,
            format!(
                "{HEADER}tray = false\nfocus = \"hidden\"\ndesktop = \"switch\"\nworktrees = \".wt\"\n\n[hooks]\nplan = true\nask = false\nturn = true\npermission = true\ngate = false\n\n\
                 [theme]\nname = \"auto\"\nlight = \"light\"\ndark = \"graphite\"\n"
            )
        );
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
    }

    #[test]
    fn reads_old_focus_bools() {
        assert_eq!(toml::from_str::<Config>("focus = true\n").unwrap().focus, Focus::Front);
        assert_eq!(toml::from_str::<Config>("focus = false\n").unwrap().focus, Focus::Behind);
    }

    #[test]
    fn hidden_needs_a_tray() {
        // A vjuu with no tray (a one-off window with no resident instance, a desktop without tray icons, the tray
        // off) would leave a hidden review with nothing pointing at it.
        assert_eq!(Focus::Hidden.with_tray(true), Focus::Hidden);
        assert_eq!(Focus::Hidden.with_tray(false), Focus::Behind);
        assert_eq!(Focus::Front.with_tray(false), Focus::Front);
        assert_eq!(Focus::Behind.with_tray(false), Focus::Behind);
    }

    #[test]
    fn rejects_unknown_settings_and_values() {
        let mut config = Config::default();
        // A theme needs its file; "auto" is a pick for `theme` only.
        for (key, value) in [
            ("theme", "on"),
            ("theme.dark", "auto"),
            ("theme.light", "../x"),
            ("plan", "yes"),
            ("gate", "1"),
            ("focus", "on"),
            ("focus", "yes"),
            ("desktop", "here"),
            ("worktrees", ""),
            ("worktrees", "../elsewhere"),
            ("worktrees", "/abs"),
            ("", ""),
        ] {
            assert!(config.set(key, value).is_err(), "{key} {value}");
        }
        assert_eq!(config, Config::default());
    }
}
