//! A Claude Code Stop hook that sends back a turn ending in a prose question ("say ship", "Want me to…?"): the step
//! needs no decision, so Claude takes it; a real decision goes through vjuu instead.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::platform;

/// The command registered in settings.json; also how install and uninstall recognise the entry as vjuu's.
const COMMAND: &str = "vjuu hook gate";

const REASON: &str = "Do not end the turn with a question or a request for permission in prose. If the next step \
    needs no decision, take it now instead of asking: in a `.wt/` worktree with finished commits, run /ship (or /sync \
    when the MR exists) and end the turn with the MR link and the obvious next step, if any. If it is a real decision \
    (destructive, costs money, posts as the user, or a choice between real alternatives), put it in a vjuu review \
    whose options spell out what each one does, as the vjuu skill says.";

/// Openings of a closing line that asks for permission without a question mark, lowercase.
const ASKING: &[&str] = &["want me to", "shall i", "should i", "do you want", "let me know if", "let me know when"];

/// Claude Code's Stop hook: blocks the stop once when the last message ends in a prose question; a second stop
/// in the same turn ends it, and is logged.
pub fn hook() -> ! {
    let event = crate::hook_event();
    let text = event["last_assistant_message"].as_str().map(String::from).or_else(|| {
        let transcript = fs::read_to_string(event["transcript_path"].as_str()?).ok()?;
        crate::last_assistant_text(&transcript)
    });
    match judge(event["stop_hook_active"] == true, text.as_deref().unwrap_or_default()) {
        Stop::Pass => {}
        Stop::Block => println!("{}", json!({ "decision": "block", "reason": REASON })),
        Stop::Escape(line) => log_escape(&platform::data_dir().join("gate.log"), &event, line),
    }
    std::process::exit(0)
}

#[derive(Debug, PartialEq)]
enum Stop<'a> {
    /// The turn ends in a statement.
    Pass,
    /// The turn ends in a prose question: sent back once.
    Block,
    /// Sent back already this turn and still asking: it ends, and the closing line is logged.
    Escape(&'a str),
}

/// Returns what to do with a stop whose last message is `text`; `again` is Claude Code's `stop_hook_active`, set
/// when this turn was sent back already, so the gate never holds a turn twice.
fn judge(again: bool, text: &str) -> Stop<'_> {
    match closing_question(text) {
        None => Stop::Pass,
        Some(line) if again => Stop::Escape(line),
        Some(_) => Stop::Block,
    }
}

/// Returns the closing line of `text` when it asks the user something in prose; None for a statement or a code block.
/// The gate does not judge whether the question is real: Claude asks real ones through vjuu.
fn closing_question(text: &str) -> Option<&str> {
    let line = text.trim_end().lines().next_back()?.trim();
    // The last line of a fenced block is its fence: the message ends in code, not prose.
    if line.starts_with("```") {
        return None;
    }
    let plain = line.trim_matches(|c: char| matches!(c, '*' | '_' | '`' | '"' | ' ' | ')' | '.' | '!')).to_lowercase();
    let says = |word: &str| plain.contains(&format!("say {word}")) || plain.contains(&format!("say \"{word}"));
    let asks = plain.ends_with('?')
        || ["go", "ship", "yes", "the word"].iter().any(|w| says(w))
        || ASKING.iter().any(|a| plain.trim_start_matches(|c: char| !c.is_alphabetic()).starts_with(a));
    asks.then_some(line)
}

/// Appends a stop the gate let through to the log at `path`, one JSON line each, for counting how often asking
/// still slips out.
fn log_escape(path: &Path, event: &Value, line: &str) {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let entry = json!({ "ts": ts, "session": event["session_id"], "cwd": event["cwd"], "line": line });
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::OpenOptions::new().create(true).append(true).open(path))
        .and_then(|mut f| writeln!(f, "{entry}"));
    if let Err(e) = written {
        eprintln!("Could not log to {}: {e}", path.display());
    }
}

/// Registers the gate in `settings` (Claude Code's settings.json); returns whether it changed anything.
pub fn install(settings: &Path) -> Result<bool, String> {
    edit(settings, add)
}

/// Removes the gate from `settings`; returns whether it changed anything.
pub fn uninstall(settings: &Path) -> Result<bool, String> {
    edit(settings, remove)
}

/// Applies `change` to the settings file, writing it back only when something changed. A missing file is empty
/// settings; one that is not a JSON object is refused rather than overwritten.
fn edit(settings: &Path, change: fn(&mut Value) -> bool) -> Result<bool, String> {
    let mut value = match fs::read_to_string(settings) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON, so it was left alone: {e}", settings.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("Cannot read {}: {e}", settings.display())),
    };
    if !value.is_object() {
        return Err(format!("{} is not a JSON object, so it was left alone.", settings.display()));
    }
    if !change(&mut value) {
        return Ok(false);
    }
    let text = serde_json::to_string_pretty(&value).expect("settings serialise") + "\n";
    if let Some(dir) = settings.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    }
    fs::write(settings, text).map_err(|e| format!("Cannot write {}: {e}", settings.display()))?;
    Ok(true)
}

fn is_gate(hook: &Value) -> bool {
    hook["command"] == COMMAND
}

/// Adds the gate under `hooks.Stop`, unless it is there already.
fn add(settings: &mut Value) -> bool {
    let present = settings["hooks"]["Stop"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|entry| entry["hooks"].as_array().into_iter().flatten().any(is_gate));
    if present {
        return false;
    }
    let root = settings.as_object_mut().expect("checked by edit()");
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let Some(hooks) = hooks.as_object_mut() else { return false };
    let stop = hooks.entry("Stop").or_insert_with(|| json!([]));
    let Some(stop) = stop.as_array_mut() else { return false };
    stop.push(json!({ "hooks": [{ "type": "command", "command": COMMAND, "timeout": 10 }] }));
    true
}

/// Removes the gate, and the entry, `Stop` list and `hooks` object it leaves empty, so the file is as it was.
fn remove(settings: &mut Value) -> bool {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else { return false };
    let Some(stop) = hooks.get_mut("Stop").and_then(Value::as_array_mut) else { return false };
    let before = stop.len();
    let mut removed = false;
    for entry in stop.iter_mut() {
        if let Some(list) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
            let n = list.len();
            list.retain(|h| !is_gate(h));
            removed |= list.len() != n;
        }
    }
    stop.retain(|entry| entry["hooks"].as_array().is_none_or(|l| !l.is_empty()));
    removed |= stop.len() != before;
    if stop.is_empty() {
        hooks.remove("Stop");
    }
    if hooks.is_empty() {
        settings.as_object_mut().expect("has hooks").remove("hooks");
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_questions_are_caught() {
        for text in [
            "Pushed the branch.\n\nSay ship when you're ready.",
            "All green. Say \"go\" and I'll merge.",
            "Done.\n\nWant me to open the PR?",
            "Shall I clean up the worktrees?",
            "**Should I push it now**",
            "Tests pass. Ready to merge?",
            "1. Done\n2. Let me know if you want the tag pushed.",
            "Say the word and I'll release it.",
        ] {
            assert!(closing_question(text).is_some(), "{text:?}");
        }
    }

    #[test]
    fn statements_pass() {
        for text in [
            "",
            "PR #86 is open: https://github.com/kimvais/vjuu/pull/86",
            "Why did it fail? The lease was stale.\n\nFixed and pushed.",
            "Run this:\n\n```\nvjuu --help?\n```",
            "Next: run `cargo test` and paste the first failing line.",
            "Nobody wants me to go there.",
        ] {
            assert_eq!(closing_question(text), None, "{text:?}");
        }
        assert_eq!(closing_question("Done.\nWant me to push?\n"), Some("Want me to push?"));
    }

    #[test]
    fn blocks_once_per_turn_and_logs_the_escape() {
        let asking = "Done.\nWant me to push?";
        assert_eq!(judge(false, asking), Stop::Block);
        assert_eq!(judge(true, asking), Stop::Escape("Want me to push?"));
        assert_eq!(judge(false, "Pushed."), Stop::Pass);
        assert_eq!(judge(true, "Pushed."), Stop::Pass);

        let path = std::env::temp_dir().join(format!("vjuu-gate-log-{}", std::process::id())).join("gate.log");
        let event = json!({ "session_id": "s1", "cwd": "C:/p/vjuu", "stop_hook_active": true });
        log_escape(&path, &event, "Want me to push?");
        log_escape(&path, &event, "Shall I?");
        let lines: Vec<Value> =
            fs::read_to_string(&path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0]["ts"].as_u64().unwrap() > 0);
        assert_eq!(
            (&lines[0]["session"], &lines[0]["cwd"], &lines[1]["line"]),
            (&json!("s1"), &json!("C:/p/vjuu"), &json!("Shall I?"))
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    fn settings() -> Value {
        json!({
            "model": "opus",
            "hooks": {
                "Stop": [{ "hooks": [{ "type": "command", "command": "other stop" }] }],
                "PreToolUse": [{ "matcher": "Bash", "hooks": [{ "type": "command", "command": "check" }] }],
            },
            "env": { "A": "1" },
        })
    }

    #[test]
    fn install_twice_is_one_entry_and_uninstall_restores() {
        let mut s = settings();
        assert!(add(&mut s));
        assert!(!add(&mut s), "already there");
        let gates = s["hooks"]["Stop"].as_array().unwrap().iter().filter(|e| is_gate(&e["hooks"][0])).count();
        assert_eq!(gates, 1);
        assert_eq!(s["hooks"]["Stop"][0], settings()["hooks"]["Stop"][0], "other Stop hooks stay first");
        assert!(remove(&mut s));
        assert!(!remove(&mut s), "already gone");
        assert_eq!(s, settings());
        // Key order survives: the file reads as the user wrote it.
        assert_eq!(serde_json::to_string(&s).unwrap(), serde_json::to_string(&settings()).unwrap());

        // Settings without hooks get them, and lose them again.
        let mut bare = json!({ "model": "opus" });
        assert!(add(&mut bare));
        assert_eq!(bare["hooks"]["Stop"][0]["hooks"][0]["command"], COMMAND);
        assert!(remove(&mut bare));
        assert_eq!(bare, json!({ "model": "opus" }));
    }

    #[test]
    fn install_and_uninstall_the_file() {
        let dir = std::env::temp_dir().join(format!("vjuu-gate-{}", std::process::id()));
        let path = dir.join("settings.json");
        let original = serde_json::to_string_pretty(&settings()).unwrap() + "\n";
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, &original).unwrap();
        assert_eq!(install(&path), Ok(true));
        assert_eq!(install(&path), Ok(false));
        assert_eq!(uninstall(&path), Ok(true));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        // Broken settings are never overwritten.
        for broken in ["{ not json", "[1, 2]"] {
            fs::write(&path, broken).unwrap();
            assert!(install(&path).is_err(), "{broken}");
            assert_eq!(fs::read_to_string(&path).unwrap(), broken);
        }
        // No settings yet: created.
        fs::remove_file(&path).unwrap();
        assert_eq!(install(&path), Ok(true));
        assert!(fs::read_to_string(&path).unwrap().contains(COMMAND));
        fs::remove_dir_all(&dir).unwrap();
    }
}
