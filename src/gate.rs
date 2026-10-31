//! A Claude Code Stop hook that sends back a turn ending in a prose question ("say ship", "Want me to…?"): the step
//! needs no decision, so Claude takes it; a real decision goes through vjuu instead.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::{config, platform};

/// The command registered in settings.json; also how install recognises the entry as vjuu's.
const COMMAND: &str = "vjuu hook gate";

/// Returns what the gate tells Claude when it sends a turn back; `worktrees` is the worktree folder setting.
fn reason(worktrees: &str) -> String {
    format!(
        "Do not end the turn with a question or a request for permission in prose. If the next step needs no \
         decision, take it now instead of asking: in a worktree under `{worktrees}/` with finished commits, run /ship \
         (or /sync when the PR or MR exists) and end the turn with its link and the obvious next step, if any. If it \
         is a real decision (destructive, costs money, posts as the user, or a choice between real alternatives), put \
         it in a vjuu review whose options spell out what each one does, as the vjuu skill says."
    )
}

/// Openings of a closing line that asks for permission without a question mark, lowercase.
const ASKING: &[&str] = &["want me to", "shall i", "should i", "do you want", "let me know if", "let me know when"];

/// Claude Code's Stop hook: blocks the stop once when the last message ends in a prose question; a second stop
/// in the same turn ends it, and is logged.
pub fn hook() -> ! {
    let config = config::load();
    if !config.hooks.gate {
        std::process::exit(0)
    }
    let event = crate::hook_event();
    let text = event["last_assistant_message"].as_str().map(String::from).or_else(|| {
        let transcript = fs::read_to_string(event["transcript_path"].as_str()?).ok()?;
        crate::last_assistant_text(&transcript)
    });
    match judge(event["stop_hook_active"] == true, text.as_deref().unwrap_or_default()) {
        Stop::Pass => {}
        Stop::Block => println!("{}", json!({ "decision": "block", "reason": reason(&config.worktrees) })),
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

/// Imperatives that open a request for the user's go-ahead (`Reply "yes" and I'll…`), lowercase.
const PROMPTS: &[&str] = &["say", "reply", "tell me", "type", "approve", "confirm"];

/// Phrases anywhere in a closing line that wait on the user's go-ahead, lowercase.
const WAITING: &[&str] =
    &["waiting on your go", "waiting for your go", "once you say so", "when you're ready", "tell me to "];

/// Returns whether `plain`, a lowercase closing line, asks for the user's go-ahead as a statement: a clause that
/// opens with a prompt and quotes the word to send or promises `and I'll…`, or a phrase that waits on the user.
/// A report that only quotes something (`The commit says "Refs #188"`) does not count.
fn requests_go(plain: &str) -> bool {
    // `**yes**`, `` `go` `` and curly quotes all quote the word to send back.
    let line = plain.replace("**", "\"").replace(['`', '“', '”'], "\"").replace('’', "'");
    let prompted = |clause: &str| {
        PROMPTS.iter().filter_map(|p| clause.strip_prefix(p)).any(|rest| {
            rest.starts_with(" \"")
                || (rest.starts_with(' ') && [" and i'll ", " and i will "].iter().any(|a| rest.contains(a)))
        })
    };
    WAITING.iter().any(|w| line.contains(w))
        || line.split(['.', ':', ';', ',', '!', '?', '(', '—']).any(|clause| prompted(clause.trim_start()))
}

/// Returns the closing line of `text` when it asks the user something in prose or asks for their go-ahead; None for
/// a statement or a code block. The gate does not judge whether the question is real: Claude asks real ones
/// through vjuu.
fn closing_question(text: &str) -> Option<&str> {
    let line = text.trim_end().lines().next_back()?.trim();
    // The last line of a fenced block is its fence: the message ends in code, not prose.
    if line.starts_with("```") {
        return None;
    }
    let plain = line.trim_matches(|c: char| matches!(c, '*' | '_' | '`' | '"' | ' ' | ')' | '.' | '!')).to_lowercase();
    // A quoted word (`say "go"`) is a prompt that requests_go finds; this catches the unquoted `say ship`.
    let asks = plain.ends_with('?')
        || ["go", "ship", "yes", "the word"].iter().any(|w| plain.contains(&format!("say {w}")))
        || requests_go(&plain)
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

/// Registers the gate in `settings` (Claude Code's settings.json) for `vjuu setup`, and returns whether it changed
/// anything. A missing file is empty settings; one that is not a JSON object is refused rather than overwritten.
/// The `gate` setting switches it off; installed, it stays, so turning it back on needs no reinstall.
pub fn install(settings: &Path) -> Result<bool, String> {
    let mut value = match fs::read_to_string(settings) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("{} is not valid JSON, so it was left alone: {e}", settings.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("Cannot read {}: {e}", settings.display())),
    };
    if !value.is_object() {
        return Err(format!("{} is not a JSON object, so it was left alone.", settings.display()));
    }
    if !add(&mut value) {
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

/// Adds the gate under `hooks.Stop`, unless it is there already or the vjuu plugin, which registers it, is enabled.
fn add(settings: &mut Value) -> bool {
    // Claude Code keys enabled plugins as `name@marketplace`.
    let plugin = settings["enabledPlugins"]
        .as_object()
        .is_some_and(|p| p.iter().any(|(k, on)| k.starts_with("vjuu@") && *on == true));
    let present = plugin
        || settings["hooks"]["Stop"]
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
            // Requests for a go-ahead phrased as statements (#92), each answered with a bare go-ahead.
            "Next: say \"ship\" to push it.",
            "Next: reply \"go\" and I'll write the sampler and start it on the VM.",
            "Reply **yes** and I'll deactivate the Express finding in Vanta.",
            "Next: tell me \"ship it\" and I'll commit, push … and open the MR.",
            "The #685 changes are still uncommitted, waiting on your go.",
            "To push, tell me to commit, or commit it yourself and run /sync.",
            "Type `merge` to land it.",
            "Next: reply “go” and I’ll start it.",
            "Approve it and I'll merge.",
            "The branch is ready to push once you say so.",
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
            // A finished result the user may still choose to ship is a report, not a request (#92).
            "The commit says \"Refs #188\" rather than \"Closes\", so the issue stays open.",
            "Renamed the type \"Gate\" to \"Stop\" and I'll note it in the changelog.",
            "Pushed; CI is green and the PR is open.",
            "The essay \"go\" is in docs/.",
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
        assert!(reason(".wt").contains("a worktree under `.wt/`"), "names the worktree folder setting");

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
    fn installs_once_beside_other_hooks() {
        // The plugin registers the gate itself: a second entry would block every question twice.
        for enabled in [json!({ "vjuu@vjuu": true }), json!({ "vjuu@fork": true, "other@x": false })] {
            let mut s = settings();
            s["enabledPlugins"] = enabled;
            let before = s.clone();
            assert!(!add(&mut s));
            assert_eq!(s, before);
        }
        let mut s = settings();
        assert!(add(&mut s));
        assert!(!add(&mut s), "already there");
        let mut expected = settings();
        expected["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "hooks": [{ "type": "command", "command": COMMAND, "timeout": 10 }] }));
        assert_eq!(s, expected);
        // Key order survives, so the file reads as the user wrote it: the gate goes last, other Stop hooks first.
        assert_eq!(serde_json::to_string(&s).unwrap(), serde_json::to_string(&expected).unwrap());
        // A disabled plugin does not register it.
        let mut off = json!({ "enabledPlugins": { "vjuu@vjuu": false } });
        assert!(add(&mut off));
        assert_eq!(off["hooks"]["Stop"][0]["hooks"][0]["command"], COMMAND);
    }

    #[test]
    fn installs_into_the_file() {
        let dir = std::env::temp_dir().join(format!("vjuu-gate-{}", std::process::id()));
        let path = dir.join("settings.json");
        let original = serde_json::to_string_pretty(&settings()).unwrap() + "\n";
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, &original).unwrap();
        assert_eq!(install(&path), Ok(true));
        let installed = fs::read_to_string(&path).unwrap();
        assert_eq!(install(&path), Ok(false));
        assert_eq!(fs::read_to_string(&path).unwrap(), installed, "unchanged the second time");
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
