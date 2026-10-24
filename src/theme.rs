//! Themes: CSS files of custom properties in the `themes` folder next to the settings. Apply compiles the chosen ones
//! into one `theme.css`, so opening a window reads that single file (or the resident instance's copy of it) and never
//! scans the folder.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;
use std::{fs, io};

use serde::Serialize;

use crate::{config, platform};

/// Built in, and copied into the themes folder whenever one is missing; a user's edited copy is never overwritten.
const SHIPPED: [(&str, &str); 2] =
    [("light", include_str!("../themes/light.css")), ("graphite", include_str!("../themes/graphite.css"))];

/// The theme name that follows the OS between the light and the dark pick.
pub const AUTO: &str = "auto";

#[derive(Debug, PartialEq, Serialize)]
pub struct Theme {
    pub name: String,
    /// "light" or "dark", from the theme's `color-scheme`.
    pub scheme: String,
}

pub fn dir() -> PathBuf {
    platform::config_dir().join("themes")
}

fn compiled_path() -> PathBuf {
    platform::data_dir().join("theme.css")
}

/// Copies the shipped themes that are missing from the themes folder.
pub fn ensure_shipped() -> io::Result<()> {
    fs::create_dir_all(dir())?;
    for (name, css) in SHIPPED {
        let path = dir().join(format!("{name}.css"));
        if !path.exists() {
            fs::write(path, css)?;
        }
    }
    Ok(())
}

/// Returns the themes in the folder, by name.
pub fn list() -> Vec<Theme> {
    let _ = ensure_shipped();
    let mut themes: Vec<Theme> = fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension()? == "css").then_some(())?;
            let name = path.file_stem()?.to_string_lossy().into_owned();
            let scheme = scheme(&fs::read_to_string(&path).ok()?);
            Some(Theme { name, scheme })
        })
        .collect();
    themes.sort_by(|a, b| a.name.cmp(&b.name));
    themes
}

/// Returns a theme's declarations, read from its file, or the shipped copy when the file is gone.
fn declarations(name: &str) -> Option<Vec<(String, String)>> {
    let shipped = || SHIPPED.iter().find(|(n, _)| *n == name).map(|(_, css)| css.to_string());
    // A name is a file in the themes folder, never a path out of it.
    if name.contains(['/', '\\', '.']) {
        return None;
    }
    let css = fs::read_to_string(dir().join(format!("{name}.css"))).ok().or_else(shipped)?;
    Some(parse(&css))
}

/// Returns the `color-scheme` and `--custom-property` declarations of a stylesheet, in order; everything else (other
/// properties, selectors, at-rules) is dropped, so a theme can recolour vjuu but not change its layout.
fn parse(css: &str) -> Vec<(String, String)> {
    let mut text = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        text += &rest[..start];
        rest = rest[start + 2..].split_once("*/").map_or("", |(_, after)| after);
    }
    text += rest;
    text.split(['{', '}', ';'])
        .filter_map(|declaration| {
            let (key, value) = declaration.split_once(':')?;
            let (key, value) = (key.trim(), value.trim());
            // `<` could close the style element the compiled theme ends up in.
            let ok = (key.starts_with("--") || key == "color-scheme") && !value.is_empty() && !value.contains('<');
            ok.then(|| (key.to_string(), value.to_string()))
        })
        .collect()
}

fn scheme(css: &str) -> String {
    let declared = parse(css).into_iter().find(|(k, _)| k == "color-scheme").map(|(_, v)| v);
    if declared.is_some_and(|v| v.contains("dark")) { "dark" } else { "light" }.to_string()
}

fn block(declarations: &[(String, String)]) -> String {
    declarations.iter().map(|(k, v)| format!("  {k}: {v};\n")).collect()
}

/// Returns one theme's declarations as a `:root` rule; the settings panel previews themes with this.
pub fn preview(name: &str) -> Option<String> {
    Some(format!(":root {{\n{}}}\n", block(&declarations(name)?)))
}

/// Returns the stylesheet for the settings' theme; a theme that cannot be read falls back to the shipped default.
pub fn compile(theme: &config::Theme) -> String {
    let pick = |name: &str, fallback: &str| declarations(name).or_else(|| declarations(fallback)).unwrap_or_default();
    let header = "/* Compiled by vjuu from its themes folder; change the theme in the settings, not here. */\n";
    if theme.name == AUTO {
        let (light, dark) = (pick(&theme.light, "light"), pick(&theme.dark, "graphite"));
        format!(
            "{header}:root {{\n{}}}\n@media (prefers-color-scheme: dark) {{\n:root {{\n{}}}\n}}\n",
            block(&light),
            block(&dark)
        )
    } else {
        format!("{header}:root {{\n{}}}\n", block(&pick(&theme.name, "light")))
    }
}

/// Compiles the settings' theme into the file every window loads.
pub fn write_compiled(theme: &config::Theme) -> io::Result<()> {
    let _ = ensure_shipped();
    let path = compiled_path();
    fs::create_dir_all(path.parent().expect("compiled theme has a folder"))?;
    fs::write(path, compile(theme))
}

/// The compiled theme and the modification time it was read at: a resident instance reads the file once, and again
/// only after an Apply (here or from `vjuu config` in another process) has rewritten it.
static CACHE: Mutex<Option<(SystemTime, Vec<u8>)>> = Mutex::new(None);

/// Returns the compiled theme for a window; compiles it first if this install has none yet.
pub fn compiled() -> Vec<u8> {
    let path = compiled_path();
    let modified = || fs::metadata(&path).and_then(|m| m.modified()).ok();
    if modified().is_none() {
        let _ = write_compiled(&config::load().theme);
    }
    let Some(stamp) = modified() else { return compile(&config::load().theme).into_bytes() };
    let mut cache = CACHE.lock().expect("theme cache lock");
    if let Some((cached, bytes)) = cache.as_ref()
        && *cached == stamp
    {
        return bytes.clone();
    }
    let bytes = fs::read(&path).unwrap_or_default();
    *cache = Some((stamp, bytes.clone()));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_custom_properties_and_the_scheme() {
        let css = "/* a: b; */ :root { color-scheme: dark; --bg: #000; /* --gone: 1; */ font-size: 40px; }\n\
                   body { --fg: rgb(1, 2, 3); display: none } @import url(x.css); --bad: </style>;";
        assert_eq!(
            parse(css),
            [("color-scheme", "dark"), ("--bg", "#000"), ("--fg", "rgb(1, 2, 3)")]
                .map(|(k, v)| (k.to_string(), v.to_string()))
        );
        assert_eq!(scheme(css), "dark");
        assert_eq!(scheme(":root { --bg: #fff; }"), "light", "light unless it says dark");
    }

    #[test]
    fn shipped_themes_parse_whole() {
        for (name, css) in SHIPPED {
            // Every declaration in a shipped theme is kept: one per line that ends in a semicolon.
            let lines = css.lines().filter(|l| l.trim_end().ends_with(';') || l.contains("; /*")).count();
            assert_eq!(parse(css).len(), lines, "{name}");
        }
        assert_eq!(scheme(SHIPPED[0].1), "light");
        assert_eq!(scheme(SHIPPED[1].1), "dark");
    }

    #[test]
    fn names_never_leave_the_themes_folder() {
        for name in ["../config", "a/b", "a\\b", "light.css", ""] {
            assert_eq!(preview(name), None, "{name}");
        }
    }

    #[test]
    fn auto_compiles_both_picks_behind_a_media_query() {
        let theme = config::Theme { name: AUTO.into(), light: "light".into(), dark: "graphite".into() };
        let css = compile(&theme);
        let (light, dark) = css.split_once("@media (prefers-color-scheme: dark)").unwrap();
        assert!(light.contains("--bg: #ffffff;") && dark.contains("--bg: #1e1e1e;"), "{css}");
        // A fixed pick, and a missing one falling back to the shipped default.
        let fixed = compile(&config::Theme { name: "graphite".into(), ..theme });
        assert!(fixed.contains("--bg: #1e1e1e;") && !fixed.contains("@media"), "{fixed}");
        let missing = compile(&config::Theme { name: "nope".into(), light: String::new(), dark: String::new() });
        assert!(missing.contains("--bg: #ffffff;"), "{missing}");
    }
}
