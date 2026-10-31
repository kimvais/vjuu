mod config;
mod gate;
mod host;
mod platform;
mod review;
mod theme;
mod tray;

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, fs, thread};

use interprocess::local_socket::{ListenerOptions, Stream, prelude::*};
use serde::{Deserialize, Serialize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{ResizeDirection, Window};
use wry::WebContext;
use wry::http::Response;

use host::{Host, Tab, TabId};
use platform::{bring_to_front, pipe_name};
use review::{Review, Verdict};

const HELP: &str = "\
vjuu: a review window for agent handoffs. Comment on lines, then approve (Ctrl+S), ask for a redo (Ctrl+R) or discard
(Ctrl+Z); the review comes back on stdout, its first line the verdict: approved, redo or discard.

Usage:
  vjuu <file>...                 Review files in one window: markdown, a diff or patch, an HTML page
                                 (or file:// URL), or any source file
  vjuu -                         Review stdin: a diff or markdown
  vjuu --git [<git diff args>]   Review `git diff` in the current directory
  vjuu --previous <old> <file>   Review a file as its changes since an earlier copy; d shows it in full
  vjuu --comments <json> ...     Show the comments kept in <json> by the last review, and keep this one's there
  vjuu --session <name> ...      Name the asking session in the title bar (automatic under Claude Code)
  vjuu last                      Print the most recent review again

  vjuu config                    Open the settings
  vjuu config list               Print the settings and where they are saved
  vjuu config <setting> <value>  Change one: plan, ask, turn, permission, tray or focus (on / off),
                                 theme (a theme or auto), theme.light or theme.dark (a theme)
  vjuu setup                     Install the vjuu, ship and sync skills for Claude Code and Codex
  vjuu start                     Start the resident instance in the tray (also: double-click vjuu.exe)
  vjuu stop                      Stop the resident instance (before upgrading)
  vjuu hook <name>               Claude Code hooks: plan, ask, turn, permission or gate

  vjuu -h, --help                Show this
  vjuu -v, --version             Show the version

More at https://vjuu.it";
/// The skill that tells agents to run vjuu; built in, so it always matches this version.
const SKILL: &str = include_str!("../skills/vjuu/SKILL.md");
/// Wait before warming the next spare window, so it does not compete with the one just shown.
const SPARE_DELAY: Duration = Duration::from_millis(500);
/// How a hook tells the vjuu it runs what it asks; an environment variable, so no argument can be mistaken for it.
const KIND_VAR: &str = "VJUU_KIND";

#[derive(Serialize, Deserialize)]
struct Doc {
    name: String,
    kind: String,
    text: String,
    /// Directory the document's relative images resolve against.
    base: Option<PathBuf>,
    /// The changes since an earlier version, as a unified diff; shown instead of the document until toggled.
    #[serde(default)]
    changes: Option<String>,
}

/// The documents one window shows; also what a client sends the resident instance.
#[derive(Serialize, Deserialize)]
struct Request {
    docs: Vec<Doc>,
    autoclose: bool,
    /// The agent session that asked for the review, shown in the title bar.
    #[serde(default)]
    session: Option<String>,
    /// Where vjuu was run: its git checkout goes in the title bar.
    #[serde(default)]
    cwd: Option<PathBuf>,
    /// Opens on the settings panel: once after a fresh install, and for `vjuu config`.
    #[serde(default)]
    settings: bool,
    /// The documents are only a sample behind the settings panel: closing the panel closes the window, unreviewed.
    #[serde(default)]
    sample: bool,
    /// Where this review's comments are kept for the next round, as JSON.
    #[serde(default)]
    comments: Option<PathBuf>,
    /// The comments kept there by the round before, shown beside this one's but not sent again.
    #[serde(default)]
    earlier: Vec<review::Comment>,
    /// What a Claude Code hook asks (plan, question, permission, turn), for the tray's list; None for documents.
    #[serde(default)]
    kind: Option<String>,
}

impl Request {
    fn name(&self) -> String {
        if self.sample {
            return "settings".into();
        }
        self.docs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")
    }

    fn title(&self) -> String {
        match &self.session {
            Some(session) => format!("vjuu: {session} · {}", self.name()),
            None => format!("vjuu: {}", self.name()),
        }
    }
}

/// Messages posted by ui/app.js via `window.ipc.postMessage`.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Msg {
    State {
        review: Review,
    },
    /// Approve, Redo or Discard: the review is finished with that verdict.
    Done {
        verdict: Verdict,
    },
    Rendered,
    /// The page has drawn its documents. Finishing WebView2's start-up can hand focus back to the previous
    /// window (via Explorer's ForegroundStaging), so the window is brought to the front again here.
    Ready,
    // The window is frameless; the page draws the title bar and asks for these.
    Drag,
    Resize {
        dir: String,
    },
    Minimize,
    Maximize,
    /// The settings panel's Apply.
    Settings {
        config: config::Config,
    },
    /// The settings panel's "Open themes folder".
    Themes,
    /// A tab picked in the tab strip, or with Ctrl+Tab or Ctrl+1 to Ctrl+9.
    Show {
        tab: TabId,
    },
    /// A tab's close button: its review is discarded.
    Close {
        tab: TabId,
    },
    /// The window's close button: every tab's review is discarded.
    Quit,
}

enum AppEvent {
    /// A review to show; the stream is the client waiting for it, None when vjuu opened it itself.
    Open(Request, Option<Stream>),
    Ui(TabId, Msg),
    Tray(tray::Action),
    /// A tab's git context, looked up after it opened.
    Context(TabId, String),
}

/// Where a finished review goes.
enum Reply {
    Stdout,
    Client(Stream),
}

type Slot = Arc<Mutex<Option<Request>>>;

fn main() {
    let started = Instant::now();
    platform::keep_stdio_private();
    let mut args: Vec<String> = env::args().skip(1).collect();
    // Only as the first argument, so it never eats a `--git` argument.
    let session = (args.len() > 1 && args[0] == "--session").then(|| args.drain(..2).nth(1).expect("checked length"));
    // Next, likewise.
    let comments = (args.len() > 1 && args[0] == "--comments")
        .then(|| PathBuf::from(args.drain(..2).nth(1).expect("checked length")));
    let mut request = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["-h" | "--help"] => {
            println!("{HELP}");
            process::exit(0)
        }
        ["-v" | "--version"] => {
            println!("vjuu {}", env!("CARGO_PKG_VERSION"));
            process::exit(0)
        }
        // Double-clicked: run as an app in the tray. The winget validator also launches the exe so.
        [] if platform::own_console() => start(),
        // Nothing to review: the help, but as an error, since a script calling vjuu so has a bug.
        [] => {
            eprintln!("{HELP}");
            process::exit(2)
        }
        ["last"] => print_last(),
        ["start"] => start(),
        ["stop"] => stop(),
        ["setup"] => setup(),
        ["config"] => settings_request(),
        ["config", "list"] => print_config(),
        ["config", key, value] => set_config(key, value),
        ["hook", "plan"] => hook_plan(),
        ["hook", "turn"] => hook_turn(),
        ["hook", "ask"] => hook_ask(),
        ["hook", "permission"] => hook_permission(),
        ["hook", "gate"] => gate::hook(),
        ["--serve"] => run(None, started),
        ["--git", ref rest @ ..] => git_diff(rest),
        ["-"] => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap_or_else(|e| fail(&format!("Cannot read stdin: {e}")));
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let doc = Doc {
                name: "stdin".into(),
                kind: kind_of("", &text).into(),
                text,
                base: env::current_dir().ok(),
                changes: None,
            };
            request(vec![doc])
        }
        ["--previous", previous, path] => request(vec![revised_doc(previous, path)]),
        // A command with the wrong arguments is not a file to open.
        ref paths @ [first, ..]
            if first != "config" && first != "hook" && paths.iter().all(|p| !p.starts_with('-')) =>
        {
            request(paths.iter().map(|p| file_doc(p)).collect())
        }
        ref other => {
            // The first option vjuu does not know, or the command's first wrong argument.
            let wrong = other.iter().find(|a| a.starts_with('-')).or(other.get(1)).copied().unwrap_or(other[0]);
            eprintln!(
                "vjuu: cannot use `{wrong}` here (in `vjuu {}`).\nRun `vjuu --help` for usage.",
                other.join(" ")
            );
            process::exit(2)
        }
    };
    request.session = session.or_else(claude_session_title);
    if let Some(path) = comments {
        // No file yet, or one vjuu cannot read, is a first round: no earlier comments.
        request.earlier =
            fs::read(&path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        // The resident instance writes it, from its own directory.
        request.comments = Some(std::path::absolute(&path).unwrap_or(path));
    }
    // A fresh install opens on the settings panel once; later windows pay only for checking that the file exists.
    if !request.settings && !config::path().exists() && config::save(&config::Config::default()).is_ok() {
        request.settings = true;
    }
    if env::var_os("VJUU_COLD").is_none() {
        match Stream::connect(pipe_name()) {
            Ok(conn) => client(conn, &request, started),
            // Nobody listening: show this one ourselves and start a resident instance for next time.
            Err(_) => spawn_server(),
        }
    }
    run(Some(request), started)
}

/// Returns the name given with `/rename` to the Claude Code session running us.
fn claude_session_title() -> Option<String> {
    session_title(&env::var("CLAUDE_CODE_SESSION_ID").ok()?)
}

/// Returns Claude Code's config folder: `CLAUDE_CONFIG_DIR`, or `~/.claude`.
fn claude_dir() -> Option<PathBuf> {
    env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| {
        env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")).map(|home| PathBuf::from(home).join(".claude"))
    })
}

/// Returns the `/rename` name of the Claude Code session with this id.
fn session_title(id: &str) -> Option<String> {
    let root = claude_dir()?;
    // The project folder is the session's start directory, mangled; finding the id is simpler than mangling.
    let file = fs::read_dir(root.join("projects"))
        .ok()?
        .flatten()
        .map(|d| d.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())?;
    transcript_title(&fs::read_to_string(file).ok()?)
}

/// shortcut: Claude Code's transcript format is undocumented; anything unexpected just means no title.
fn transcript_title(transcript: &str) -> Option<String> {
    // Not `aiTitle`: Claude Code's own summary of the first prompt reads as a task description, not a name.
    // Titles are appended again as they change; the last one wins. Quoted text elsewhere is not top level.
    transcript.lines().rev().filter(|l| l.contains("customTitle")).find_map(|l| {
        serde_json::from_str::<serde_json::Value>(l).ok()?.get("customTitle")?.as_str().map(String::from)
    })
}

/// Claude Code's PreToolUse hook on ExitPlanMode: opens the plan in vjuu. Approved exits plan mode, redo sends the
/// plan back for changes; discarded decides nothing, so Claude Code's own approval prompt follows.
fn hook_plan() -> ! {
    if !config::load().hooks.plan {
        process::exit(0)
    }
    let event = hook_event();
    // Claude Code injects the plan file's path; without one there is nothing to anchor comments to.
    let Some(plan) = event["tool_input"]["planFilePath"].as_str().filter(|p| Path::new(p).is_file()) else {
        process::exit(0)
    };
    let dir = session_dir(event["session_id"].as_str().unwrap_or_default());
    let comments = dir.join("comments.json");
    let mut args = vec![OsStr::new("--comments"), comments.as_os_str()];
    let previous = previous_plan(Path::new(plan), &dir);
    if let Some(previous) = &previous {
        args.extend([OsStr::new("--previous"), previous.as_os_str()]);
    }
    args.push(OsStr::new(plan));
    if let Some(decision) = plan_decision(&review_in_child("plan", &event, &args)) {
        println!("{decision}");
    }
    process::exit(0)
}

/// Returns where vjuu keeps a Claude Code session's plan and comments between rounds of review.
fn session_dir(session: &str) -> PathBuf {
    // The id comes from the hook's input: it must not climb out of the folder.
    let id: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    platform::data_dir().join("sessions").join("claude").join(if id.is_empty() { "none" } else { &id })
}

/// Keeps a copy of the plan as the session last saw it, and returns a copy of that earlier version when the plan has
/// changed since, so a revision opens as a diff against it.
fn previous_plan(plan: &Path, dir: &Path) -> Option<PathBuf> {
    let shown = dir.join("plan.md");
    let previous = dir.join("plan.previous.md");
    fs::create_dir_all(dir).ok()?;
    let current = fs::read(plan).ok()?;
    let before = fs::read(&shown).ok().filter(|before| *before != current);
    let _ = fs::write(&shown, &current);
    fs::write(&previous, before?).ok()?;
    Some(previous)
}

/// Claude Code's Stop hook: shows the diff of the files Claude edited this turn; approved and redo go back to Claude.
/// Without edits, or discarded, it lets the turn end.
fn hook_turn() -> ! {
    if !config::load().hooks.turn {
        process::exit(0)
    }
    let event = hook_event();
    let Some(cwd) = event["cwd"].as_str() else { process::exit(0) };
    let transcript = event["transcript_path"].as_str().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
    let since = turn_started(&transcript);
    let mut files = edited_this_turn(&transcript);
    // Shell commands name no files: what they edited is what changed on disk since the turn started.
    let commands = shell_commands(&transcript);
    if let Some(since) = since.filter(|_| !commands.is_empty()) {
        files.extend(changed_since(Path::new(cwd), &commands, since));
    }
    let diff = turn_diff(Path::new(cwd), &files, since);
    // One file per session, so parallel sessions do not overwrite each other's diff.
    let path = env::temp_dir().join(format!("vjuu-turn-{}.diff", event["session_id"].as_str().unwrap_or("none")));
    // Hook feedback starts no new turn, so a reply to it that edits nothing would show the same diff again.
    let reviewed = fs::read_to_string(&path).is_ok_and(|shown| shown == diff);
    if diff.is_empty() || reviewed || fs::write(&path, diff).is_err() {
        process::exit(0)
    }
    if let Some(feedback) = turn_feedback(&review_in_child("turn", &event, &[path.as_os_str()])) {
        println!("{feedback}");
    }
    process::exit(0)
}

/// Claude Code's PreToolUse hook on AskUserQuestion: asks the questions in vjuu, under Claude's last message.
/// Picks alone answer the tool; comments or unanswered questions go back to Claude as the whole review instead.
/// Discarding (or closing the window) leaves the questions to Claude Code's own picker.
fn hook_ask() -> ! {
    if !config::load().hooks.ask {
        process::exit(0)
    }
    let event = hook_event();
    let questions = event["tool_input"]["questions"].as_array().cloned().unwrap_or_default();
    // shortcut: a choice block picks one option, so multi-select questions stay in Claude Code's picker.
    if questions.is_empty() || questions.iter().any(|q| q["multiSelect"] == true) {
        process::exit(0)
    }
    let transcript = event["transcript_path"].as_str().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
    let md = ask_markdown(last_assistant_text(&transcript).as_deref(), &questions);
    let path = env::temp_dir().join(format!("vjuu-ask-{}.md", event["session_id"].as_str().unwrap_or("none")));
    if fs::write(&path, md).is_err() {
        process::exit(0)
    }
    if let Some(decision) =
        ask_decision(&event["tool_input"], &review_in_child("question", &event, &[path.as_os_str()]))
    {
        println!("{decision}");
    }
    process::exit(0)
}

/// Returns the text of Claude's latest message in this turn, the context its questions refer to.
/// shortcut: Claude Code's transcript format is undocumented; lines it does not recognise are skipped.
fn last_assistant_text(transcript: &str) -> Option<String> {
    for line in transcript.lines().rev() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if entry["type"] == "user" && entry["origin"]["kind"] == "human" {
            return None;
        }
        if entry["type"] != "assistant" {
            continue;
        }
        let blocks = entry["message"]["content"].as_array().into_iter().flatten();
        let text: Vec<&str> = blocks.filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
        if !text.is_empty() {
            return Some(text.join("\n\n"));
        }
    }
    None
}

/// Returns the markdown vjuu shows for AskUserQuestion: the context, then one choice block per question.
fn ask_markdown(context: Option<&str>, questions: &[serde_json::Value]) -> String {
    let mut md = String::new();
    if let Some(context) = context {
        md += &format!("{}\n\n---\n\n", context.trim());
    }
    md += "_Pick an answer to each question, or comment to answer in your own words._\n";
    for q in questions {
        if let Some(header) = q["header"].as_str().filter(|h| !h.is_empty()) {
            md += &format!("\n### {header}\n");
        }
        md += &format!("\n```choice\n{}\n", q["question"].as_str().unwrap_or_default());
        for option in q["options"].as_array().into_iter().flatten() {
            // shortcut: a label containing ": " is cut short there, as the block reads "label: description".
            let label = option["label"].as_str().unwrap_or_default();
            match option["description"].as_str().filter(|d| !d.is_empty()) {
                Some(description) => md += &format!("- {label}: {description}\n"),
                None => md += &format!("- {label}\n"),
            }
        }
        md += "```\n";
    }
    md
}

/// Returns the hook output for a review of AskUserQuestion: the answers when every question was picked and nothing
/// else said, otherwise the whole review for Claude to read; None when the user answered nothing.
fn ask_decision(tool_input: &serde_json::Value, review: &str) -> Option<String> {
    if matches!(Verdict::of(review), None | Some(Verdict::Discard)) || !has_comments(review) {
        return None;
    }
    let (picks, other_comments) = picks(review);
    let questions = tool_input["questions"].as_array().into_iter().flatten();
    let answers: Option<serde_json::Map<String, serde_json::Value>> = questions
        .map(|q| {
            let question = q["question"].as_str()?;
            Some((question.to_string(), picks.get(&squash(question))?.clone().into()))
        })
        .collect();
    let output = match answers {
        Some(answers) if !other_comments => {
            let mut input = tool_input.clone();
            input["answers"] = answers.into();
            serde_json::json!({ "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": input })
        }
        _ => {
            let reason = format!(
                "The user answered in vjuu instead of the question picker. Use their picks and comments below, and \
                 ask again only about what is still open.\n\n{}",
                review.trim()
            );
            serde_json::json!({
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            })
        }
    };
    Some(serde_json::json!({ "hookSpecificOutput": output }).to_string())
}

/// Returns a review's choice picks, keyed by the question with its whitespace squashed, and whether it has other comments.
fn picks(review: &str) -> (HashMap<String, String>, bool) {
    let mut picks = HashMap::new();
    let mut other_comments = false;
    let mut lines = review.lines();
    while let Some(line) = lines.next() {
        if let Some(question) = line.strip_prefix("## Choice: ") {
            let answer = lines.next().unwrap_or_default();
            if answer != "No answer." {
                picks.insert(squash(question), answer.to_string());
            }
        } else if line.starts_with("## ") {
            other_comments = true;
        }
    }
    (picks, other_comments)
}

/// vjuu shows a choice question's lines joined by spaces.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const PERMISSION_QUESTION: &str = "Allow this?";

/// Claude Code's PermissionRequest hook: asks Allow / Deny in vjuu, under what Claude wants to do.
/// A pick decides; without one, approved allows once and redo denies with the comments as the reason; discarded
/// leaves the terminal prompt.
fn hook_permission() -> ! {
    if !config::load().hooks.permission {
        process::exit(0)
    }
    let event = hook_event();
    // Their own hooks already showed these, and the plan's terminal prompt is where the next mode is picked.
    if matches!(event["tool_name"].as_str(), Some("ExitPlanMode" | "AskUserQuestion")) {
        process::exit(0)
    }
    let path = env::temp_dir().join(format!("vjuu-permission-{}.md", event["session_id"].as_str().unwrap_or("none")));
    if fs::write(&path, permission_markdown(&event)).is_err() {
        process::exit(0)
    }
    if let Some(decision) = permission_decision(&event, &review_in_child("permission", &event, &[path.as_os_str()])) {
        println!("{decision}");
    }
    process::exit(0)
}

/// Returns the markdown vjuu shows for a permission request: what the tool would do, then Allow / Deny buttons.
fn permission_markdown(event: &serde_json::Value) -> String {
    let tool = event["tool_name"].as_str().unwrap_or("a tool");
    let input = &event["tool_input"];
    let text = |key: &str| input[key].as_str().unwrap_or_default();
    let mut md = format!("# Claude wants to use {tool}\n\n");
    match tool {
        "Bash" | "PowerShell" => {
            if !text("description").is_empty() {
                md += &format!("{}\n\n", text("description"));
            }
            md += &fence(if tool == "Bash" { "sh" } else { "powershell" }, text("command"));
        }
        "Edit" => {
            let lines = |prefix: &str, s: &str| s.lines().map(|l| format!("{prefix}{l}\n")).collect::<String>();
            md += &format!("`{}`\n\n", text("file_path"));
            md += &fence("diff", &(lines("-", text("old_string")) + &lines("+", text("new_string"))));
        }
        "Write" => {
            md += &format!("`{}`\n\n", text("file_path"));
            let ext = Path::new(text("file_path")).extension().and_then(|e| e.to_str()).unwrap_or_default();
            md += &fence(ext, text("content"));
        }
        _ => md += &fence("json", &serde_json::to_string_pretty(input).unwrap_or_default()),
    }
    md += &format!("\n```choice\n{PERMISSION_QUESTION}\n- Allow: just this once\n");
    // Claude Code's own suggestion, e.g. a rule for every `npm test` from now on.
    let rules: Vec<String> = event["permission_suggestions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["rules"].as_array().cloned().unwrap_or_default())
        .map(|r| match r["ruleContent"].as_str() {
            Some(content) => format!("`{}({content})`", r["toolName"].as_str().unwrap_or_default()),
            None => format!("`{}`", r["toolName"].as_str().unwrap_or_default()),
        })
        .collect();
    if !rules.is_empty() {
        md += &format!("- Always allow: {}\n", rules.join(", "));
    }
    md += "- Deny: comment to tell Claude why\n```\n";
    md
}

/// Returns `text` in a fenced code block whose fence is longer than any backtick run inside it.
fn fence(lang: &str, text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest.max(2) + 1);
    format!("{ticks}{lang}\n{}\n{ticks}\n", text.trim_end())
}

/// Returns the PermissionRequest hook output for a review: a pick decides, and without one approved allows once and
/// redo denies; None when discarded, closed, or vjuu failed, so the terminal prompt asks.
fn permission_decision(event: &serde_json::Value, review: &str) -> Option<String> {
    let verdict = Verdict::of(review)?;
    if verdict == Verdict::Discard {
        return None;
    }
    let (picks, _) = picks(review);
    let decision = match (picks.get(PERMISSION_QUESTION).map(String::as_str), verdict) {
        (Some("Allow"), _) | (None, Verdict::Approved) => serde_json::json!({ "behavior": "allow" }),
        (Some("Always allow"), _) => {
            serde_json::json!({ "behavior": "allow", "updatedPermissions": event["permission_suggestions"] })
        }
        // Deny, or redo without a pick.
        _ => serde_json::json!({
            "behavior": "deny",
            "message": format!("The user denied this in vjuu.\n\n{}", review.trim()),
        }),
    };
    Some(
        serde_json::json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
            .to_string(),
    )
}

/// Returns a hook's JSON event from stdin; anything unexpected is Null, so the hook falls through and decides nothing.
fn hook_event() -> serde_json::Value {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    serde_json::from_str(&input).unwrap_or_default()
}

/// Runs vjuu with `args` as a child titled with the hook session's name, and returns the review it prints.
fn review_in_child(kind: &str, event: &serde_json::Value, args: &[&OsStr]) -> String {
    let Ok(exe) = env::current_exe() else { return String::new() };
    let mut vjuu = Command::new(exe);
    vjuu.env(KIND_VAR, kind);
    if let Some(title) = event["session_id"].as_str().and_then(session_title) {
        vjuu.args(["--session", &title]);
    }
    let out = vjuu.args(args).stdin(process::Stdio::null()).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Returns whether a review has anything for the agent: empty output (vjuu failed) or "No comments." has not.
fn has_comments(review: &str) -> bool {
    let review = review.trim();
    !review.is_empty() && !review.ends_with("\nNo comments.")
}

/// Returns the tool calls Claude made since the user's last prompt, latest first.
/// shortcut: Claude Code's transcript format is undocumented; lines it does not recognise are skipped.
fn tool_calls_this_turn(transcript: &str) -> Vec<serde_json::Value> {
    let mut calls = Vec::new();
    for line in transcript.lines().rev() {
        let Ok(mut entry) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        // Tool results and hook feedback are user entries too; only what the user typed starts a turn.
        if entry["type"] == "user" && entry["origin"]["kind"] == "human" {
            break;
        }
        if let Some(blocks) = entry["message"]["content"].as_array_mut() {
            calls.extend(blocks.drain(..).filter(|b| b["type"] == "tool_use"));
        }
    }
    calls
}

/// Returns the files Claude's edit tools touched since the user's last prompt.
fn edited_this_turn(transcript: &str) -> Vec<String> {
    let mut files: Vec<String> = tool_calls_this_turn(transcript)
        .iter()
        .filter_map(|call| match call["name"].as_str() {
            Some("Edit" | "MultiEdit" | "Write") => call["input"]["file_path"].as_str().map(String::from),
            Some("NotebookEdit") => call["input"]["notebook_path"].as_str().map(String::from),
            _ => None,
        })
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Returns the shell commands (Bash, PowerShell) Claude ran since the user's last prompt.
fn shell_commands(transcript: &str) -> Vec<String> {
    tool_calls_this_turn(transcript)
        .iter()
        .filter(|call| matches!(call["name"].as_str(), Some("Bash" | "PowerShell")))
        .filter_map(|call| call["input"]["command"].as_str().map(String::from))
        .collect()
}

/// Returns the files that changed on disk at or after `since` (Unix seconds), committed during the turn or not, in
/// the repository at `cwd` and in each of its worktrees that one of the shell `commands` names (`cd .wt/fix`).
/// Checking every worktree would take seconds in a repository with dozens of them, at the end of every turn.
/// shortcut: a deleted file has no time to compare, so a deletion shows only when an edit tool named the file; a
/// file the user saved in an editor during the turn shows too.
fn changed_since(cwd: &Path, commands: &[String], since: i64) -> Vec<String> {
    let Ok(top) = Path::new(git_in(cwd, &["rev-parse", "--show-toplevel"]).trim()).canonicalize() else {
        return Vec::new();
    };
    let named = |worktree: &Path| {
        worktree.file_name().is_some_and(|name| commands.iter().any(|c| c.contains(&*name.to_string_lossy())))
    };
    let mut files = Vec::new();
    for listed in git_in(cwd, &["worktree", "list", "--porcelain"]).lines().filter_map(|l| l.strip_prefix("worktree "))
    {
        let worktree = Path::new(listed);
        let Ok(dir) = worktree.canonicalize() else { continue };
        if dir != top && !(dir.starts_with(&top) && named(worktree)) {
            continue;
        }
        let base = turn_base(worktree, Some(since));
        let names = git_in(worktree, &["diff", "--name-only", &base])
            + &git_in(worktree, &["ls-files", "--others", "--exclude-standard"]);
        for path in names.lines().map(|name| worktree.join(name)) {
            let changed = fs::metadata(&path).ok().filter(fs::Metadata::is_file).and_then(|m| m.modified().ok());
            let secs = changed.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
            if secs.is_some_and(|s| s >= since) {
                files.push(path.to_string_lossy().into_owned());
            }
        }
    }
    files
}

/// Returns when the user's last prompt was sent, in Unix seconds.
fn turn_started(transcript: &str) -> Option<i64> {
    transcript
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|entry| entry["type"] == "user" && entry["origin"]["kind"] == "human")
        .and_then(|entry| unix_seconds(entry["timestamp"].as_str()?))
}

/// Parses a UTC timestamp such as `2026-10-24T16:37:19.724Z` into Unix seconds.
fn unix_seconds(iso: &str) -> Option<i64> {
    let n = |at: usize, len: usize| -> Option<i64> { iso.get(at..at + len)?.parse().ok() };
    let (y, m, d) = (n(0, 4)?, n(5, 2)?, n(8, 2)?);
    // Days since the epoch, from Howard Hinnant's days_from_civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    Some((era * 146_097 + doe - 719_468) * 86_400 + n(11, 2)? * 3600 + n(14, 2)? * 60 + n(17, 2)?)
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).args(["-c", "core.quotepath=off"]).args(args).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Returns this turn's changes to those of `files` inside the repository at `cwd`, new files included: committed
/// during the turn (commits authored since `since`) or not, each in its own worktree (`.work/<branch>`, say).
/// shortcut: uncommitted changes from earlier turns to the same files show too, as git cannot tell turns apart.
fn turn_diff(cwd: &Path, files: &[String], since: Option<i64>) -> String {
    let Ok(top) = Path::new(git_in(cwd, &["rev-parse", "--show-toplevel"]).trim()).canonicalize() else {
        return String::new();
    };
    // Files outside the session's repository are not this review's business; worktrees inside it are.
    let mut by_worktree: std::collections::BTreeMap<PathBuf, Vec<&str>> = Default::default();
    for file in files {
        let Some(dir) = Path::new(file).parent().and_then(|d| d.canonicalize().ok()) else { continue };
        if !dir.starts_with(&top) {
            continue;
        }
        if let Ok(worktree) = Path::new(git_in(&dir, &["rev-parse", "--show-toplevel"]).trim()).canonicalize() {
            by_worktree.entry(worktree).or_default().push(file);
        }
    }
    let mut diff = String::new();
    for (worktree, files) in by_worktree {
        let base = turn_base(&worktree, since);
        diff += &git_in(&worktree, &[&["diff", "--no-color", "--no-ext-diff", &base, "--"][..], &files].concat());
        for new in
            git_in(&worktree, &[&["ls-files", "--others", "--exclude-standard", "--"][..], &files].concat()).lines()
        {
            diff += &git_in(&worktree, &["diff", "--no-color", "--no-ext-diff", "--no-index", "--", "/dev/null", new]);
        }
    }
    diff
}

/// Returns the commit the worktree was at when the turn started: the parent of the oldest of the latest commits
/// authored since `since` (author dates survive a rebase), or HEAD when the turn committed nothing.
fn turn_base(worktree: &Path, since: Option<i64>) -> String {
    // git's empty tree: the base when the turn made the repository's first commit.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let Some(since) = since else { return "HEAD".into() };
    let mut base = None;
    for line in git_in(worktree, &["log", "-n", "200", "--format=%at %P"]).lines() {
        let mut fields = line.split_whitespace();
        if fields.next().and_then(|t| t.parse::<i64>().ok()).is_none_or(|authored| authored < since) {
            break;
        }
        base = Some(fields.next().unwrap_or(EMPTY_TREE).to_string());
    }
    base.unwrap_or_else(|| "HEAD".into())
}

/// Returns the hook output that hands the review of this turn's changes to Claude; None when discarded, closed, or
/// vjuu failed, so the turn just ends.
fn turn_feedback(review: &str) -> Option<String> {
    let review = review.trim();
    let context = match (Verdict::of(review)?, has_comments(review)) {
        (Verdict::Discard, _) => return None,
        (Verdict::Approved, false) => "The user approved this turn's changes in vjuu. Proceed to the next step \
            without asking for approval again, as the vjuu skill says for `approved`."
            .to_string(),
        (Verdict::Approved, true) => format!(
            "The user approved this turn's changes in vjuu, with the comments below. Apply the minor ones and \
             proceed without asking for approval again, as the vjuu skill says for `approved`. Treat a comment that \
             changes behaviour as `redo`: act on it and stop, so the changes open in vjuu again.\n\n{review}"
        ),
        (Verdict::Redo, true) => format!(
            "The user asked for a redo of this turn's changes in vjuu. Act on each comment that asks for a change and \
             answer each question, then stop: the changes open in vjuu again.\n\n{review}"
        ),
        (Verdict::Redo, false) => "The user asked for a redo of this turn's changes in vjuu without saying what to \
            change. Ask what to change."
            .to_string(),
    };
    Some(
        serde_json::json!({ "hookSpecificOutput": { "hookEventName": "Stop", "additionalContext": context } })
            .to_string(),
    )
}

/// Returns the hook output for a plan review: approved allows ExitPlanMode, redo sends the plan back with the review;
/// None when discarded, closed, or vjuu failed, so Claude Code's own approval prompt follows.
fn plan_decision(review: &str) -> Option<String> {
    let review = review.trim();
    let output = match Verdict::of(review)? {
        Verdict::Discard => return None,
        Verdict::Approved => {
            let mut output = serde_json::json!({
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "Approved in vjuu.",
            });
            if has_comments(review) {
                output["additionalContext"] = format!(
                    "The user approved the plan in vjuu, with the comments below. Apply the minor ones as you carry \
                     out the plan. If one changes behaviour, update the plan file and open it in vjuu again before \
                     doing that part.\n\n{review}"
                )
                .into();
            }
            output
        }
        Verdict::Redo => serde_json::json!({
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": format!(
                "The user asked for a redo of the plan in vjuu. Act on each comment, update the plan file, then call \
                 ExitPlanMode again.\n\n{review}"
            ),
        }),
    };
    Some(serde_json::json!({ "hookSpecificOutput": output }).to_string())
}

fn request(docs: Vec<Doc>) -> Request {
    let autoclose = env::var_os("VJUU_AUTOCLOSE").is_some();
    Request {
        docs,
        autoclose,
        session: None,
        cwd: env::current_dir().ok(),
        settings: false,
        sample: false,
        comments: None,
        earlier: Vec::new(),
        kind: env::var(KIND_VAR).ok(),
    }
}

/// Returns a window for the settings panel alone, with vjuu's README and a sample diff behind it, so a theme preview
/// shows markdown, code and diff colours.
fn settings_request() -> Request {
    let doc = |name: &str, kind: &str, text: &str| Doc {
        name: name.into(),
        kind: kind.into(),
        text: text.into(),
        base: None,
        changes: None,
    };
    // The diff first: the modal panel blocks scrolling, so only the top of the window shows.
    let docs = vec![
        doc("sample.diff", "diff", include_str!("sample.diff")),
        doc("README.md", "md", include_str!("../README.md")),
    ];
    Request { settings: true, sample: true, ..request(docs) }
}

fn print_config() -> ! {
    println!("# {}\n{}", config::path().display(), config::load().to_toml());
    process::exit(0)
}

fn set_config(key: &str, value: &str) -> ! {
    let mut config = config::load();
    config.set(key, value).unwrap_or_else(|e| fail(&e));
    config::save(&config).unwrap_or_else(|e| fail(&format!("Cannot save {}: {e}", config::path().display())));
    theme::write_compiled(&config.theme).unwrap_or_else(|e| fail(&format!("Cannot compile the theme: {e}")));
    process::exit(0)
}

fn file_doc(arg: &str) -> Doc {
    // A `file://` URL, as copied from a browser's address bar. A Windows path parses as a URL too, scheme `c`.
    let local = url::Url::parse(arg).ok().filter(|u| u.scheme() == "file").map(|u| {
        let path = u.to_file_path().unwrap_or_else(|()| fail(&format!("Not a local file: `{arg}`")));
        path.to_string_lossy().into_owned()
    });
    let path = local.as_deref().unwrap_or(arg);
    let bytes = fs::read(path).unwrap_or_else(|e| fail(&format!("Cannot read `{path}`: {e}")));
    // Lenient with odd encodings (Latin-1 source, a binary byte or two): show the file rather than refuse it.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let base = Path::new(path).canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    Doc { name: path.into(), kind: kind_of(path, &text).into(), text, base, changes: None }
}

/// Returns the file's document with its changes since `previous`, an earlier copy of it; none when nothing changed.
fn revised_doc(previous: &str, path: &str) -> Doc {
    let doc = file_doc(path);
    // git's diff works outside a repository too; without git, the file just shows in full.
    let diff = git_in(Path::new("."), &["diff", "--no-color", "--no-ext-diff", "--no-index", "--", previous, path]);
    // Named after the file on both sides, not after the earlier copy, so comments name the file.
    let changes = diff.find("\n@@ ").map(|at| format!("--- a/{0}\n+++ b/{0}{1}", doc.name, &diff[at..]));
    Doc { changes, ..doc }
}

/// Returns how a file is shown: "diff", "md", "html" (the rendered page), or "code" (line-numbered source; the UI
/// picks the language). The extension wins; content decides only for diff-like text in other files and for stdin.
fn kind_of(path: &str, text: &str) -> &'static str {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "diff" | "patch" => "diff",
        "md" | "markdown" => "md",
        _ if is_diff(text) => "diff",
        "html" | "htm" => "html",
        // Extensionless files (README, NOTES) are usually prose.
        "" => "md",
        _ => "code",
    }
}

fn is_diff(text: &str) -> bool {
    text.starts_with("diff ") || (text.starts_with("--- ") || text.contains("\n+++ ")) && text.contains("\n@@ ")
}

fn git_diff(rest: &[&str]) -> Request {
    // color.ui=always or a configured diff.external would otherwise hand us something that isn't a plain diff.
    let out = Command::new("git")
        .args(["diff", "--no-color", "--no-ext-diff"])
        .args(rest)
        .output()
        .unwrap_or_else(|e| fail(&format!("Cannot run git: {e}")));
    if !out.status.success() {
        fail(&String::from_utf8_lossy(&out.stderr));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        println!("No changes to review.");
        process::exit(0);
    }
    let name = format!("git diff {}", rest.join(" ")).trim_end().to_string();
    request(vec![Doc { name, kind: "diff".into(), text, base: env::current_dir().ok(), changes: None }])
}

fn client(mut conn: Stream, request: &Request, started: Instant) -> ! {
    let mut line = serde_json::to_vec(request).expect("Request serialises");
    line.push(b'\n');
    if let Err(e) = conn.write_all(&line).and_then(|_| conn.flush()) {
        fail(&format!("Cannot reach the resident vjuu: {e}"));
    }
    let mut md = String::new();
    // Windows reports the server closing the pipe as an error rather than EOF.
    let read = conn.read_to_string(&mut md);
    if request.autoclose {
        eprintln!("rendered in {} ms (resident)", started.elapsed().as_millis());
        process::exit(0)
    }
    if md.is_empty() {
        fail(&format!("The resident vjuu closed without a review ({read:?}). `vjuu last` may have it."));
    }
    print!("{md}");
    process::exit(0)
}

/// Starts the resident instance, with its tray icon, unless it is running already.
fn start() -> ! {
    if Stream::connect(pipe_name()).is_err() {
        spawn_server();
    }
    process::exit(0)
}

/// Stops the resident instance, e.g. before replacing the exe. Open review windows close without a review.
fn stop() -> ! {
    if let Ok(mut conn) = Stream::connect(pipe_name()) {
        let _ = conn.write_all(b"stop\n").and_then(|_| conn.flush());
    }
    process::exit(0)
}

fn spawn_server() {
    if let Ok(exe) = env::current_exe() {
        // Started by a hook's vjuu, it would otherwise take every later window for that hook's kind.
        platform::spawn_detached(Command::new(exe).arg("--serve").env_remove(KIND_VAR));
    }
}

/// Accepts clients on a thread and hands each request to the event loop.
fn listen(proxy: EventLoopProxy<AppEvent>) {
    // A socket file left by a crashed instance would block us forever; we only get here when nobody answered it.
    let listener =
        match ListenerOptions::new().name(pipe_name()).try_overwrite(platform::socket_may_be_stale()).create_sync() {
            Ok(listener) => listener,
            // Another resident instance won the race.
            Err(_) => process::exit(0),
        };
    thread::spawn(move || {
        for conn in listener.incoming().filter_map(Result::ok) {
            let mut reader = BufReader::new(conn);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            if line.trim() == "stop" {
                process::exit(0);
            }
            match serde_json::from_str::<Request>(&line) {
                Ok(request) => {
                    let _ = proxy.send_event(AppEvent::Open(request, Some(reader.into_inner())));
                }
                Err(e) => eprintln!("Bad request: {e}"),
            }
        }
    });
}

/// Runs the event loop: one window with one review (cold), or resident, with a review per tab and a pre-warmed spare.
fn run(initial: Option<Request>, started: Instant) -> ! {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    // Keep the webview profile out of the exe's directory (`~/.cargo/bin` after `cargo install`).
    let mut context = WebContext::new(Some(platform::data_dir().join("webview")));
    let mut host = Host::new(&event_loop);
    let mut spare_due = None;
    // Only the resident instance has a tray: it outlives the reviews and holds every session's.
    let resident = initial.is_none();
    let mut tray: Option<tray::Tray> = None;
    // The waiting reviews the tray lists; None makes a new tray icon catch up.
    let mut listed: Option<Vec<(TabId, String)>> = None;
    let tray_proxy = Arc::new(Mutex::new(proxy.clone()));
    let new_tray = move |target: &EventLoopWindowTarget<AppEvent>| {
        let proxy = tray_proxy.clone();
        tray::Tray::new(target, move |action| {
            let _ = proxy.lock().expect("proxy lock").send_event(AppEvent::Tray(action));
        })
    };
    // Read once, like the tray setting; Apply in the panel updates it, `vjuu config` from the next resident instance.
    let mut focus = config::load().focus;
    match initial {
        Some(request) => {
            let cwd = request.cwd.clone();
            let id = host.open(&proxy, &mut context, request, Some(Reply::Stdout), started, focus);
            look_up_context(&proxy, id, cwd);
        }
        None => {
            listen(proxy.clone());
            spare_due = Some(Instant::now());
        }
    }
    event_loop.run(move |event, target, control_flow| {
        match event {
            // macOS wants the tray made once the loop runs; the other platforms do not mind.
            Event::NewEvents(StartCause::Init) if resident && config::load().tray => tray = new_tray(target),
            #[cfg(windows)]
            Event::UserEvent(AppEvent::Tray(tray::Action::Menu)) => {
                if let Some(tray) = &tray {
                    tray.show_menu();
                }
            }
            Event::UserEvent(AppEvent::Tray(tray::Action::Oldest)) => {
                if let Some(id) = host.tabs().min_by_key(|(_, t)| t.started).map(|(id, _)| *id) {
                    host.raise(id);
                }
            }
            Event::UserEvent(AppEvent::Tray(tray::Action::Open(id))) => host.raise(id),
            Event::UserEvent(AppEvent::Ui(_, Msg::Show { tab })) => host.show(tab),
            Event::UserEvent(AppEvent::Tray(tray::Action::Settings)) => {
                let _ = proxy.send_event(AppEvent::Open(settings_request(), None));
            }
            Event::UserEvent(AppEvent::Tray(tray::Action::Stop)) => process::exit(0),
            Event::UserEvent(AppEvent::Context(id, git)) => {
                if let Some(s) = host.get_mut(id) {
                    s.context = Some(git);
                    show_context(s);
                }
                if host.is_active(id)
                    && let Some(s) = host.get(id)
                {
                    host.window.set_title(&s.title());
                }
            }
            Event::UserEvent(AppEvent::Open(request, conn)) => {
                let cwd = request.cwd.clone();
                let id = host.open(&proxy, &mut context, request, conn.map(Reply::Client), Instant::now(), focus);
                look_up_context(&proxy, id, cwd);
                spare_due = Some(Instant::now() + SPARE_DELAY);
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::State { review })) => {
                if let Some(s) = host.get_mut(id) {
                    s.latest = review;
                }
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Settings { config })) => {
                match config::save(&config) {
                    Ok(()) => {
                        if let Some(s) = host.get_mut(id) {
                            s.applied = true;
                        }
                    }
                    Err(e) => eprintln!("Cannot save {}: {e}", config::path().display()),
                }
                match theme::write_compiled(&config.theme) {
                    // Every tab recolours, not only the one with the panel, and so does the spare.
                    Ok(()) => host.webviews().for_each(|webview| {
                        let _ = webview.evaluate_script("window.vjuuThemeSaved && vjuuThemeSaved()");
                    }),
                    Err(e) => eprintln!("Cannot compile the theme: {e}"),
                }
                focus = config.focus;
                if !config.tray {
                    tray = None;
                } else if resident && tray.is_none() {
                    tray = new_tray(target);
                    listed = None;
                }
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Done { verdict })) => finish(&mut host, id, Some(verdict)),
            // Closing is no decision: the agent must not read "looks good" into it.
            Event::UserEvent(AppEvent::Ui(_, Msg::Close { tab })) => finish(&mut host, tab, Some(Verdict::Discard)),
            Event::UserEvent(AppEvent::Ui(_, Msg::Quit))
            | Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                let ids: Vec<TabId> = host.tabs().map(|(id, _)| *id).collect();
                ids.into_iter().for_each(|id| finish(&mut host, id, Some(Verdict::Discard)));
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Ready)) => {
                if let Some(s) = host.get(id) {
                    show_context(s);
                    host.show_strip_in(s);
                    let active = host.is_active(id);
                    if active && !s.background {
                        bring_to_front(&host.window, &s.webview);
                    }
                    // Opened behind on purpose, or the OS kept it there (focus-stealing prevention): say so.
                    if tray.is_some() && !(active && host.window.is_focused()) {
                        notify(s.title());
                    }
                }
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Rendered)) => {
                if let Some(s) = host.get(id)
                    && matches!(s.reply, Some(Reply::Stdout))
                {
                    eprintln!("rendered in {} ms", s.started.elapsed().as_millis());
                }
                finish(&mut host, id, None)
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => host.fit(),
            // Activation focuses the native window, not the webview inside it; pass keyboard focus on, or keys
            // go nowhere until the page is clicked (seen with resident windows and after alt-tab).
            Event::WindowEvent { event: WindowEvent::Focused(true), .. } => {
                if let Some(s) = host.tabs().find(|(id, _)| host.is_active(*id)).map(|(_, s)| s) {
                    let _ = s.webview.focus();
                }
            }
            Event::UserEvent(AppEvent::Ui(_, msg)) => window_command(&host.window, msg),
            _ => {}
        }
        if let Some(tray) = &mut tray {
            let open = waiting(&host);
            if listed.as_ref() != Some(&open) {
                tray.show_waiting(&open);
                listed = Some(open);
            }
        }
        if spare_due.is_some_and(|due| due <= Instant::now()) {
            host.warm_spare(&proxy, &mut context);
            spare_due = None;
        }
        // The tray menu's ages count minutes.
        let tick = (tray.is_some() && host.tabs().next().is_some()).then(|| Instant::now() + Duration::from_secs(60));
        *control_flow = match spare_due.into_iter().chain(tick).min() {
            Some(due) => ControlFlow::WaitUntil(due),
            None => ControlFlow::Wait,
        };
    })
}

/// Returns the waiting reviews, oldest first, each with its line for the tray menu.
fn waiting(host: &Host) -> Vec<(TabId, String)> {
    let mut open: Vec<&(TabId, Tab)> = host.tabs().collect();
    open.sort_by_key(|(_, s)| s.started);
    open.into_iter().map(|(id, s)| (*id, s.label())).collect()
}

/// Looks up the git context of `cwd` on a thread and hands it to the tab, twice: the checkout at once, and again
/// with the PR or MR number once `gh` or `glab` answers over the network. The tab never waits for either.
fn look_up_context(proxy: &EventLoopProxy<AppEvent>, id: TabId, cwd: Option<PathBuf>) {
    let Some(cwd) = cwd else { return };
    let proxy = proxy.clone();
    thread::spawn(move || {
        let run = |program: &str, args: &[&str]| run_in(&cwd, program, args);
        let Some(checkout) = checkout_context(run) else { return };
        let _ = proxy.send_event(AppEvent::Context(id, checkout.clone()));
        if let Some(change) = change_request(run) {
            let _ = proxy.send_event(AppEvent::Context(id, format!("{checkout} · {change}")));
        }
    });
}

fn show_context(s: &Tab) {
    if let Some(git) = &s.context {
        let git = serde_json::to_string(git).expect("string serialises");
        let _ = s.webview.evaluate_script(&format!("window.vjuuContext && vjuuContext({git})"));
    }
}

/// Returns a command's trimmed output, or None when it fails, prints nothing or is not installed.
fn run_in(cwd: &Path, program: &str, args: &[&str]) -> Option<String> {
    let out = platform::no_window(&mut Command::new(program))
        .current_dir(cwd)
        .args(args)
        .stdin(process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

/// Returns "repo · worktree · branch" for the checkout `run` runs git in; the worktree only when it is a linked one
/// not named after its branch.
fn checkout_context(run: impl Fn(&str, &[&str]) -> Option<String>) -> Option<String> {
    let top = PathBuf::from(run("git", &["rev-parse", "--show-toplevel"])?);
    // Every worktree shares the main checkout's .git, whose folder names the repository.
    let common = PathBuf::from(run("git", &["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
    let main = common.parent()?;
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    let branch = run("git", &["branch", "--show-current"]);
    let worktree = name(&top).filter(|w| top != main && Some(w) != branch.as_ref());
    Some([name(main), worktree, branch].into_iter().flatten().collect::<Vec<_>>().join(" · "))
}

/// Returns "PR #12" or "MR !12" for the current branch, asking `gh` or `glab` depending on where origin lives.
fn change_request(run: impl Fn(&str, &[&str]) -> Option<String>) -> Option<String> {
    let origin = run("git", &["remote", "get-url", "origin"])?;
    if origin.contains("github") {
        run("gh", &["pr", "view", "--json", "number", "--jq", ".number"]).map(|n| format!("PR #{n}"))
    } else if origin.contains("gitlab") {
        let mr: serde_json::Value = serde_json::from_str(&run("glab", &["mr", "view", "--output", "json"])?).ok()?;
        mr["iid"].as_u64().map(|n| format!("MR !{n}"))
    } else {
        None
    }
}

/// Shows a system notification that a review window is waiting, without holding up the event loop.
/// shortcut: on Windows the toast is credited to PowerShell, as vjuu registers no app id of its own.
fn notify(title: String) {
    thread::spawn(move || {
        let body = title.strip_prefix("vjuu: ").unwrap_or(&title);
        let _ = notify_rust::Notification::new().summary("vjuu is waiting for you").body(body).show();
    });
}

/// Moves, resizes, minimises or maximises a frameless window for the page's title bar and edges.
fn window_command(window: &Window, msg: Msg) {
    match msg {
        // drag_window hands the move to Windows, so Aero Snap keeps working.
        Msg::Drag => {
            let _ = window.drag_window();
        }
        Msg::Resize { dir } => {
            let dir = match dir.as_str() {
                "n" => ResizeDirection::North,
                "s" => ResizeDirection::South,
                "e" => ResizeDirection::East,
                "w" => ResizeDirection::West,
                "ne" => ResizeDirection::NorthEast,
                "nw" => ResizeDirection::NorthWest,
                "se" => ResizeDirection::SouthEast,
                "sw" => ResizeDirection::SouthWest,
                _ => return,
            };
            let _ = window.drag_resize_window(dir);
        }
        Msg::Minimize => window.set_minimized(true),
        Msg::Maximize => window.set_maximized(!window.is_maximized()),
        Msg::Themes => {
            let _ = theme::ensure_shipped();
            platform::open_url(&theme::dir().to_string_lossy());
        }
        Msg::State { .. }
        | Msg::Done { .. }
        | Msg::Rendered
        | Msg::Ready
        | Msg::Settings { .. }
        | Msg::Show { .. }
        | Msg::Close { .. }
        | Msg::Quit => {}
    }
}

/// Returns whether `url` is vjuu's own page or one of its files.
fn is_vjuu(url: &str) -> bool {
    // WebView2 serves custom protocols as http://<scheme>.localhost; WebKit keeps vjuu://.
    url.starts_with("vjuu://localhost/") || url.starts_with("http://vjuu.localhost/")
}

/// Saves the review and sends it where it was asked for; `verdict` is None for autoclose timing runs.
fn finish(host: &mut Host, id: TabId, verdict: Option<Verdict>) {
    let Some(s) = host.remove(id) else { return };
    let (name, only_settings, keep) = match s.slot.lock().expect("slot lock").as_ref() {
        Some(r) => (r.name(), r.sample, r.comments.clone()),
        None => (String::new(), false, None),
    };
    if verdict.is_some()
        && let Some(path) = keep
    {
        let json = serde_json::to_vec(&s.latest.comments).expect("comments serialise");
        if let Err(e) = fs::write(&path, json) {
            eprintln!("Could not keep the comments in {}: {e}", path.display());
        }
    }
    let md = match (verdict, only_settings) {
        // Nothing to review: Apply saved the settings, or Esc left them as they were.
        (_, true) if s.applied => "Settings saved.\n".into(),
        (_, true) => "Settings unchanged.\n".into(),
        (Some(verdict), false) => save_review(&name, &s.latest, verdict),
        (None, false) => String::new(),
    };
    match s.reply {
        Some(Reply::Stdout) => {
            // Tear the webview down before exiting, or Chromium logs a window-class error to stderr.
            drop(s.webview);
            print!("{md}");
            process::exit(0)
        }
        Some(Reply::Client(mut conn)) => {
            let _ = conn.write_all(md.as_bytes()).and_then(|_| conn.flush());
        }
        None => {}
    }
}

fn serve(path: &str, slot: &Slot) -> Response<Cow<'static, [u8]>> {
    let not_found = || Response::builder().status(404).body(Cow::Borrowed(&[][..])).expect("static response");
    let (body, mime): (Cow<'static, [u8]>, &str) = match path {
        "/" => (Cow::Borrowed(include_bytes!("../ui/index.html")), "text/html"),
        "/app.js" => (Cow::Borrowed(include_bytes!("../ui/app.js")), "text/javascript"),
        "/style.css" => (Cow::Borrowed(include_bytes!("../ui/style.css")), "text/css"),
        "/vendor/markdown-it.min.js" => {
            (Cow::Borrowed(include_bytes!("../ui/vendor/markdown-it.min.js")), "text/javascript")
        }
        "/vendor/diff2html-ui-base.min.js" => {
            (Cow::Borrowed(include_bytes!("../ui/vendor/diff2html-ui-base.min.js")), "text/javascript")
        }
        "/vendor/highlight.min.js" => {
            (Cow::Borrowed(include_bytes!("../ui/vendor/highlight.min.js")), "text/javascript")
        }
        "/vendor/hljs-languages.min.js" => {
            (Cow::Borrowed(include_bytes!("../ui/vendor/hljs-languages.min.js")), "text/javascript")
        }
        "/vendor/diff2html.min.css" => (Cow::Borrowed(include_bytes!("../ui/vendor/diff2html.min.css")), "text/css"),
        "/vendor/inter.woff2" => (Cow::Borrowed(include_bytes!("../ui/vendor/inter.woff2")), "font/woff2"),
        "/vendor/jetbrains-mono.woff2" => {
            (Cow::Borrowed(include_bytes!("../ui/vendor/jetbrains-mono.woff2")), "font/woff2")
        }
        "/logo.svg" => (Cow::Borrowed(include_bytes!("../assets/icons/vjuu.svg")), "image/svg+xml"),
        // Read when the panel opens, so a change made with `vjuu config` meanwhile shows; with the themes to pick from.
        // The help reads the version from here too.
        "/config.json" => {
            let mut config = serde_json::to_value(config::load()).expect("Config serialises");
            config["themes"] = serde_json::to_value(theme::list()).expect("themes serialise");
            config["version"] = env!("CARGO_PKG_VERSION").into();
            (Cow::Owned(config.to_string().into_bytes()), "application/json")
        }
        "/theme.css" => (Cow::Owned(theme::compiled()), "text/css"),
        // The settings panel's preview of one theme.
        p if p.starts_with("/themes/") && p.ends_with(".css") => {
            match theme::preview(&p["/themes/".len()..p.len() - ".css".len()]) {
                Some(css) => (Cow::Owned(css.into_bytes()), "text/css"),
                None => return not_found(),
            }
        }
        // A spare window has no document yet; the page waits for vjuuLoad().
        "/doc.json" => match slot.lock().expect("slot lock").as_ref() {
            Some(r) => (Cow::Owned(serde_json::to_vec(r).expect("Request serialises")), "application/json"),
            None => return not_found(),
        },
        // Relative images in markdown, and an HTML page's stylesheets and images, as /files/<doc index>/<path>.
        // shortcut: no percent-decoding, so paths with spaces 404; root-relative (/img/x.png) ones 404 too.
        _ => {
            let file = path.strip_prefix("/files/").and_then(|p| p.split_once('/')).and_then(|(i, rest)| {
                let base =
                    slot.lock().expect("slot lock").as_ref()?.docs.get(i.parse::<usize>().ok()?)?.base.clone()?;
                Some(base.join(rest))
            });
            match file.and_then(|f| fs::read(f).ok()) {
                // Images are sniffed; stylesheets and SVG need their type.
                Some(bytes) => (
                    Cow::Owned(bytes),
                    match Path::new(path).extension().and_then(|e| e.to_str()) {
                        Some("svg") => "image/svg+xml",
                        Some("css") => "text/css",
                        _ => "application/octet-stream",
                    },
                ),
                None => return not_found(),
            }
        }
    };
    let mut response = Response::builder().header("Content-Type", mime);
    // theme.css keeps its URL when Apply recompiles it; a cached copy would open new windows in the old theme.
    if path == "/theme.css" {
        response = response.header("Cache-Control", "no-store");
    }
    response.body(body).expect("static response")
}

fn reviews_dir() -> PathBuf {
    platform::data_dir().join("reviews")
}

/// Writes the review to the reviews directory and returns what the agent gets: the review, or for a discarded one
/// only the verdict, as its comments are dropped. The file keeps them either way, so nothing typed is lost.
fn save_review(name: &str, review: &Review, verdict: Verdict) -> String {
    let md = review::to_markdown(name, review, verdict);
    let sent = match verdict {
        Verdict::Discard => format!("discard\n# Review: {name}\n"),
        Verdict::Approved | Verdict::Redo => md.clone(),
    };
    let dir = reviews_dir();
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let slug: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).take(60).collect();
    let path = dir.join(format!("{secs}-{slug}.md"));
    if let Err(e) = fs::create_dir_all(&dir).and_then(|_| fs::write(&path, &md)) {
        eprintln!("Could not save review to {}: {e}", path.display());
    }
    sent
}

fn print_last() -> ! {
    let last = fs::read_dir(reviews_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .max_by_key(|p| p.file_name().map(|n| n.to_os_string()));
    match last.map(fs::read_to_string) {
        Some(Ok(md)) => {
            print!("{md}");
            process::exit(0)
        }
        Some(Err(e)) => fail(&format!("Cannot read last review: {e}")),
        None => fail("No reviews yet."),
    }
}

/// Installs the skill for each agent found in the home directory; on a fresh install, then opens the settings.
fn setup() -> Request {
    let home = env::home_dir().unwrap_or_else(|| fail("Cannot find the home directory."));
    let done = install_skill(&home).unwrap_or_else(|e| fail(&format!("Cannot install the skills: {e}")));
    if done.written.is_empty() {
        fail("Found neither Claude Code (~/.claude) nor Codex (~/.codex). Install one, then run `vjuu setup` again.");
    }
    for path in done.written {
        println!("Installed {}", path.display());
    }
    for path in done.kept {
        println!("Kept your own {}", path.display());
    }
    // The question gate, on by default; the plugin registers its own, so it is skipped when the plugin is enabled.
    if let Some(claude) = claude_dir().filter(|d| d.is_dir()) {
        let settings = claude.join("settings.json");
        match gate::install(&settings) {
            Ok(true) => {
                println!("Added the question gate to {} (`vjuu config gate off` switches it off)", settings.display())
            }
            Ok(false) => {}
            Err(e) => eprintln!("Did not add the question gate: {e}"),
        }
    }
    // Written first, so the first review window does not show the panel a second time.
    if config::path().exists() || config::save(&config::Config::default()).is_err() {
        process::exit(0)
    }
    settings_request()
}

/// The skills `vjuu setup` installs, by name. ship and sync are what the question gate tells Claude to run.
const SKILLS: [(&str, &str); 3] = [
    ("vjuu", SKILL),
    ("ship", include_str!("../skills/ship/SKILL.md")),
    ("sync", include_str!("../skills/sync/SKILL.md")),
];
/// Opens the comment in every skill vjuu ships but its own; a file without it is the user's, and never overwritten.
const SHIPPED: &str = "<!-- Shipped with vjuu;";

/// What `install_skill` did.
#[derive(Debug, Default, PartialEq)]
struct Installed {
    written: Vec<PathBuf>,
    /// Skills left out because the user has their own by that name: a skill without vjuu's marker, or a command.
    kept: Vec<PathBuf>,
}

/// Writes the skills for Claude Code and Codex, where installed. The vjuu skill is always replaced; ship and sync
/// only where they are vjuu's own, so a user's `/ship` keeps working.
fn install_skill(home: &Path) -> std::io::Result<Installed> {
    // Codex reads user skills from ~/.agents/skills, a folder other agents share, and its own commands from prompts.
    // shortcut: ignores CLAUDE_CONFIG_DIR and CODEX_HOME; honour them if someone moves their config.
    let mut done = Installed::default();
    for (agent, skills, commands) in [(".claude", ".claude", "commands"), (".codex", ".agents", "prompts")] {
        if !home.join(agent).is_dir() {
            continue;
        }
        for (name, text) in SKILLS {
            let dir = home.join(skills).join("skills").join(name);
            let path = dir.join("SKILL.md");
            let command = home.join(agent).join(commands).join(format!("{name}.md"));
            let theirs = fs::read_to_string(&path).is_ok_and(|old| !old.contains(SHIPPED));
            if name != "vjuu" && (theirs || command.is_file()) {
                done.kept.push(if theirs { path } else { command });
                continue;
            }
            fs::create_dir_all(&dir)?;
            fs::write(&path, text)?;
            done.written.push(path);
        }
    }
    Ok(done)
}

fn fail(msg: &str) -> ! {
    eprintln!("{}", msg.trim_end());
    process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::{
        Installed, SHIPPED, SKILL, SKILLS, ask_decision, ask_markdown, edited_this_turn, file_doc, install_skill,
        is_diff, is_vjuu, kind_of, last_assistant_text, plan_decision, previous_plan, revised_doc, session_dir,
        transcript_title, turn_diff, turn_feedback, turn_started, unix_seconds,
    };
    use super::{change_request, checkout_context, fence, permission_decision, permission_markdown};
    use super::{changed_since, shell_commands};
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Answers like git, gh and glab would for a checkout; `None` where a command would fail.
    fn fake<'a>(answers: &'a [(&'a str, &'a str)]) -> impl Fn(&str, &[&str]) -> Option<String> + 'a {
        move |program, args| {
            let command = format!("{program} {}", args.join(" "));
            answers.iter().find(|(c, _)| *c == command).map(|(_, out)| out.to_string())
        }
    }

    #[test]
    fn checkout_names_repo_worktree_and_branch() {
        let common = ("git rev-parse --path-format=absolute --git-common-dir", "C:/p/vjuu/.git");
        let main = [("git rev-parse --show-toplevel", "C:/p/vjuu"), common, ("git branch --show-current", "main")];
        assert_eq!(checkout_context(fake(&main)).as_deref(), Some("vjuu · main"));
        // A linked worktree named after its branch adds nothing.
        let same =
            [("git rev-parse --show-toplevel", "C:/p/vjuu/.work/tray"), common, ("git branch --show-current", "tray")];
        assert_eq!(checkout_context(fake(&same)).as_deref(), Some("vjuu · tray"));
        let other =
            [("git rev-parse --show-toplevel", "C:/p/vjuu/.work/fix"), common, ("git branch --show-current", "tray")];
        assert_eq!(checkout_context(fake(&other)).as_deref(), Some("vjuu · fix · tray"));
        // Detached HEAD: no branch.
        assert_eq!(checkout_context(fake(&main[..2])).as_deref(), Some("vjuu"));
        assert_eq!(checkout_context(fake(&[])), None, "not a git checkout");
    }

    #[test]
    fn change_requests_from_github_or_gitlab() {
        let github = [
            ("git remote get-url origin", "https://github.com/kimvais/vjuu.git"),
            ("gh pr view --json number --jq .number", "27"),
        ];
        assert_eq!(change_request(fake(&github)).as_deref(), Some("PR #27"));
        let gitlab = [
            ("git remote get-url origin", "git@gitlab.com:vainu/data.git"),
            ("glab mr view --output json", r#"{"iid": 412, "title": "x"}"#),
        ];
        assert_eq!(change_request(fake(&gitlab)).as_deref(), Some("MR !412"));
        // No PR for the branch, or a host neither tool knows.
        assert_eq!(change_request(fake(&github[..1])), None);
        assert_eq!(change_request(fake(&[("git remote get-url origin", "ssh://git.example.com/x")])), None);
    }
    use std::fs;

    fn bash_request() -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": "npm test", "description": "Run the tests" },
            "permission_suggestions": [{ "type": "addRules", "behavior": "allow", "destination": "localSettings",
                "rules": [{ "toolName": "Bash", "ruleContent": "npm test:*" }] }],
        })
    }

    #[test]
    fn permission_requests_show_what_the_tool_would_do() {
        assert_eq!(
            permission_markdown(&bash_request()),
            "# Claude wants to use Bash\n\nRun the tests\n\n```sh\nnpm test\n```\n\
             \n```choice\nAllow this?\n- Allow: just this once\n- Always allow: `Bash(npm test:*)`\n\
             - Deny: comment to tell Claude why\n```\n"
        );
        let edit = serde_json::json!({ "tool_name": "Edit",
            "tool_input": { "file_path": "a.rs", "old_string": "x\ny", "new_string": "z" } });
        let md = permission_markdown(&edit);
        assert!(md.starts_with("# Claude wants to use Edit\n\n`a.rs`\n\n```diff\n-x\n-y\n+z\n```\n"), "{md}");
        // No suggestion, no "Always allow".
        assert!(!md.contains("Always allow"), "{md}");
        let other = permission_markdown(&serde_json::json!({ "tool_name": "WebFetch", "tool_input": { "url": "u" } }));
        assert!(other.contains("```json\n{\n  \"url\": \"u\"\n}\n```"), "{other}");
    }

    #[test]
    fn fences_outlast_backticks_inside() {
        assert_eq!(fence("sh", "ls\n"), "```sh\nls\n```\n");
        assert_eq!(fence("md", "a ```b``` c"), "````md\na ```b``` c\n````\n");
    }

    #[test]
    fn permission_picks_decide() {
        let event = bash_request();
        let decide = |review: &str| {
            permission_decision(&event, review).map(|d| serde_json::from_str::<serde_json::Value>(&d).unwrap())
        };
        // Discarded, closed, vjuu failed, or no verdict: the terminal prompt asks, whatever was picked.
        for review in [
            "",
            "# Review: p.md\nNo comments.\n",
            "discard\n# Review: p.md\n",
            "# Review: p.md\n\n## Choice: Allow this?\nAllow\n",
        ] {
            assert_eq!(decide(review), None, "{review:?}");
        }
        let picked = |answer: &str| format!("approved\n# Review: p.md\n\n## Choice: Allow this?\n{answer}\n");
        let decision = |review: &str| decide(review).unwrap()["hookSpecificOutput"]["decision"].clone();
        assert_eq!(decision(&picked("Allow")), serde_json::json!({ "behavior": "allow" }));
        assert_eq!(decision("approved\n# Review: p.md\nNo comments.\n"), serde_json::json!({ "behavior": "allow" }));
        assert_eq!(
            decision(&picked("Always allow")),
            serde_json::json!({ "behavior": "allow", "updatedPermissions": event["permission_suggestions"] })
        );
        let because = "redo\n# Review: p.md (1 comment)\n\n## p.md:5\n> npm test\n\nRun only the unit tests.\n";
        for review in [picked("Deny"), because.to_string(), "redo\n# Review: p.md\nNo comments.\n".to_string()] {
            let out = decide(&review).unwrap();
            assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
            assert_eq!(out["hookSpecificOutput"]["decision"]["behavior"], "deny", "{review}");
            let message = out["hookSpecificOutput"]["decision"]["message"].as_str().unwrap();
            assert!(message.ends_with(review.trim()), "{message}");
        }
    }
    use std::process::Command;

    fn questions() -> serde_json::Value {
        serde_json::json!({ "questions": [
            { "question": "Which framework?", "header": "Framework", "multiSelect": false, "options": [
                { "label": "React", "description": "Component library" }, { "label": "Vue", "description": "" } ] },
            { "question": "Ship\n now?", "header": "", "options": [ { "label": "Yes" }, { "label": "No" } ] },
        ] })
    }

    #[test]
    fn questions_become_choice_blocks_under_the_context() {
        let md = ask_markdown(Some("I found two options.\n"), questions()["questions"].as_array().unwrap());
        assert_eq!(
            md,
            "I found two options.\n\n---\n\n\
             _Pick an answer to each question, or comment to answer in your own words._\n\
             \n### Framework\n\n```choice\nWhich framework?\n- React: Component library\n- Vue\n```\n\
             \n```choice\nShip\n now?\n- Yes\n- No\n```\n"
        );
    }

    #[test]
    fn picks_answer_the_tool_and_comments_go_to_claude() {
        let input = questions();
        let decide = |review: &str| -> Option<serde_json::Value> {
            ask_decision(&input, review).map(|d| serde_json::from_str::<serde_json::Value>(&d).unwrap())
        };
        // Discarded or closed, even with picks, or approved without one: Claude Code's picker asks instead.
        let picks = "# Review: ask.md\n\n## Choice: Which framework?\nVue\n\n## Choice: Ship now?\nNo\n";
        for review in [
            "approved\n# Review: ask.md\nNo comments.\n".to_string(),
            String::new(),
            format!("discard\n{picks}"),
            picks.to_string(),
        ] {
            assert_eq!(decide(&review), None, "{review:?}");
        }

        let picked = &format!("approved\n{picks}");
        let mut expected = input.clone();
        expected["answers"] = serde_json::json!({ "Which framework?": "Vue", "Ship\n now?": "No" });
        assert_eq!(
            decide(picked),
            Some(serde_json::json!({ "hookSpecificOutput": {
                "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": expected } }))
        );

        let commented = format!("{picked}\n## ask.md:9\n> - Vue\n\nVue 3 only.\n");
        let unanswered =
            "redo\n# Review: ask.md\n\n## Choice: Which framework?\nReact\n\n## Choice: Ship now?\nNo answer.\n";
        for review in [commented.as_str(), unanswered] {
            let out = decide(review).unwrap();
            assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny", "{review}");
            let reason = out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
            assert!(reason.ends_with(review.trim()), "{reason}");
        }
    }

    #[test]
    fn context_is_the_latest_message_this_turn() {
        let said = |text: &str| {
            format!(r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#)
        };
        let ask = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"AskUserQuestion"}]}}"#;
        let prompt = r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"go"}}"#;
        let transcript = [said("earlier"), prompt.into(), said("first"), said("latest"), ask.into()].join("\n");
        assert_eq!(last_assistant_text(&transcript).as_deref(), Some("latest"));
        assert_eq!(last_assistant_text(&[said("earlier"), prompt.into()].join("\n")), None);
    }

    #[test]
    fn edits_since_the_last_prompt() {
        let prompt =
            |text: &str| format!(r#"{{"type":"user","origin":{{"kind":"human"}},"message":{{"content":"{text}"}}}}"#);
        let tool = |name: &str, key: &str, path: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"{name}","input":{{"{key}":"{path}"}}}}]}}}}"#
            )
        };
        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        let transcript = [
            prompt("first"),
            tool("Edit", "file_path", "old.rs"),
            tool("Bash", "command", "sed -i s/a/b/ old.rs"),
            prompt("second"),
            tool("Read", "file_path", "read.rs"),
            tool("Bash", "command", "sed -i s/a/b/ c.rs"),
            tool("PowerShell", "command", "cargo fmt"),
            tool("Edit", "file_path", "b.rs"),
            result.into(),
            tool("Write", "file_path", "a.md"),
            tool("Edit", "file_path", "b.rs"),
            tool("NotebookEdit", "notebook_path", "n.ipynb"),
            "not json".into(),
        ]
        .join("\n");
        assert_eq!(edited_this_turn(&transcript), ["a.md", "b.rs", "n.ipynb"]);
        assert!(edited_this_turn(&prompt("only")).is_empty());
        // Latest first; the earlier turn's command is not this turn's.
        assert_eq!(shell_commands(&transcript), ["cargo fmt", "sed -i s/a/b/ c.rs"]);
        assert!(shell_commands(&[prompt("first"), tool("Edit", "file_path", "a.rs")].join("\n")).is_empty());
    }

    #[test]
    fn shell_edits_are_what_changed_since_the_turn_started() {
        let repo = std::env::temp_dir().join(format!("vjuu-turn-shell-{}", std::process::id()));
        fs::create_dir_all(&repo).unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        // The setup is authored an hour before the turn, the turn's commit during it.
        let git_at = |when: u64, dir: &Path, args: &[&str]| {
            let status = Command::new("git")
                .current_dir(dir)
                .env("GIT_AUTHOR_DATE", format!("@{when} +0000"))
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        let git = |dir: &Path, args: &[&str]| git_at(now - 3600, dir, args);
        let write = |path: PathBuf, text: &str| fs::write(path, text).unwrap();
        git(&repo, &["init", "-q"]);
        write(repo.join(".gitignore"), ".work/\ntarget/\n");
        for name in ["sedded.txt", "earlier.txt"] {
            write(repo.join(name), "one\n");
        }
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "init"]);
        for name in ["fix", "other"] {
            git(&repo, &["worktree", "add", "-q", "-b", name, repo.join(".work").join(name).to_str().unwrap()]);
        }
        // Before the turn: changed, but an hour ago.
        write(repo.join("earlier.txt"), "two\n");
        let hour_ago = SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options().write(true).open(repo.join("earlier.txt")).unwrap().set_modified(hour_ago).unwrap();
        // This turn: a rewrite, a new file, build output, and a commit in the worktree the command went to.
        write(repo.join("sedded.txt"), "two\n");
        write(repo.join("made.txt"), "new\n");
        fs::create_dir_all(repo.join("target")).unwrap();
        write(repo.join("target").join("out.bin"), "built\n");
        let fix = repo.join(".work").join("fix");
        write(fix.join("sedded.txt"), "fixed\n");
        git_at(now, &fix, &["commit", "-qam", "fix"]);
        // A worktree no command named is not looked at, though it changed too.
        write(repo.join(".work").join("other").join("sedded.txt"), "other\n");

        let since = now as i64 - 60;
        let commands = ["cd .work/fix && sed -i s/one/fixed/ sedded.txt".to_string()];
        let mut found: Vec<String> = changed_since(&repo, &commands, since)
            .iter()
            .map(|f| Path::new(f).strip_prefix(&repo).unwrap().to_string_lossy().replace('\\', "/"))
            .collect();
        found.sort();
        assert_eq!(found, [".work/fix/sedded.txt", "made.txt", "sedded.txt"]);
        // Nothing changed since: nothing found.
        assert!(changed_since(&repo, &commands, now as i64 + 3600).is_empty());
        fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn turn_verdicts_go_back_to_claude() {
        let comments = "# Review: x.diff (1 comment)\n\n## a.rs:3\n> +x\n\nWhy?\n";
        // Discarded (with its comments dropped), closed, vjuu failed, or no verdict: the turn ends.
        for review in ["", "discard\n# Review: x.diff\n", "# Review: x.diff\nNo comments.\n", comments] {
            assert_eq!(turn_feedback(review), None, "{review:?}");
        }
        let context = |review: &str| -> String {
            let feedback: serde_json::Value = serde_json::from_str(&turn_feedback(review).unwrap()).unwrap();
            assert_eq!(feedback["hookSpecificOutput"]["hookEventName"], "Stop");
            feedback["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
        };
        for (verdict, says) in [("approved", "proceed without asking"), ("redo", "open in vjuu again")] {
            let review = format!("{verdict}\n{comments}");
            let context = context(&review);
            assert!(context.contains(says) && context.ends_with(review.trim()), "{context}");
        }
        assert!(context("approved\n# Review: x.diff\nNo comments.\n").contains("Proceed"));
        assert!(context("redo\n# Review: x.diff\nNo comments.\n").contains("Ask what to change"));
    }

    #[test]
    fn diffs_only_the_turns_files_in_the_repository() {
        let root = std::env::temp_dir().join(format!("vjuu-turn-{}", std::process::id()));
        let repo = root.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git =
            |args: &[&str]| assert!(Command::new("git").current_dir(&repo).args(args).status().unwrap().success());
        git(&["init", "-q"]);
        for (name, text) in [("edited.txt", "old\n"), ("untouched.txt", "same\n")] {
            fs::write(repo.join(name), text).unwrap();
        }
        git(&["add", "."]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
        fs::write(repo.join("edited.txt"), "new\n").unwrap();
        fs::write(repo.join("untouched.txt"), "changed but not this turn\n").unwrap();
        fs::write(repo.join("created.txt"), "fresh\n").unwrap();
        fs::write(root.join("outside.txt"), "elsewhere\n").unwrap();
        let files: Vec<String> = [repo.join("created.txt"), repo.join("edited.txt"), root.join("outside.txt")]
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let diff = turn_diff(&repo, &files, None);
        for expected in ["+++ b/edited.txt", "-old", "+new", "created.txt", "+fresh"] {
            assert!(diff.contains(expected), "{expected} missing from\n{diff}");
        }
        assert!(!diff.contains("untouched") && !diff.contains("elsewhere"), "{diff}");
        assert_eq!(turn_diff(&repo, &[], None), "");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn diffs_a_worktree_with_the_turns_commits() {
        let repo = std::env::temp_dir().join(format!("vjuu-turn-wt-{}", std::process::id()));
        let wt = repo.join(".work").join("branch");
        fs::create_dir_all(&repo).unwrap();
        // Author dates as Unix seconds: the earlier turn at 1e9, this turn (started at 1.5e9) at 2e9.
        let git = |dir: &std::path::Path, date: &str, args: &[&str]| {
            let status = Command::new("git")
                .current_dir(dir)
                .env("GIT_AUTHOR_DATE", format!("@{date} +0000"))
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&repo, "0", &["init", "-q"]);
        fs::write(repo.join(".gitignore"), ".work/\n").unwrap();
        fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&repo, "1000000000", &["add", "."]);
        git(&repo, "1000000000", &["commit", "-qm", "init"]);
        git(&repo, "0", &["worktree", "add", "-q", "-b", "branch", wt.to_str().unwrap()]);
        fs::write(wt.join("a.txt"), "two\n").unwrap();
        git(&wt, "1000000000", &["commit", "-qam", "earlier turn"]);
        fs::write(wt.join("a.txt"), "three\n").unwrap();
        fs::write(wt.join("b.txt"), "new\n").unwrap();
        git(&wt, "2000000000", &["add", "."]);
        git(&wt, "2000000000", &["commit", "-qm", "this turn"]);
        fs::write(wt.join("a.txt"), "four\n").unwrap();
        let files: Vec<String> =
            ["a.txt", "b.txt"].iter().map(|f| wt.join(f).to_string_lossy().into_owned()).collect();
        // Committed and uncommitted changes of this turn, from the worktree, though the session is in the main checkout.
        let diff = turn_diff(&repo, &files, Some(1_500_000_000));
        for expected in ["-two", "+four", "+++ b/b.txt", "+new"] {
            assert!(diff.contains(expected), "{expected} missing from\n{diff}");
        }
        assert!(!diff.contains("-one") && !diff.contains("three\n"), "{diff}");
        // Without a turn start: uncommitted changes only, as before.
        assert!(turn_diff(&repo, &files, None).contains("-three"));
        fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn reads_the_turns_start() {
        assert_eq!(unix_seconds("2026-10-24T16:37:19.724Z"), Some(1_792_859_839));
        assert_eq!(unix_seconds("2024-02-29T23:59:59Z"), Some(1_709_251_199));
        assert_eq!(unix_seconds("1970-01-01T00:00:00Z"), Some(0));
        for bad in ["", "2026-10-24", "yesterday"] {
            assert_eq!(unix_seconds(bad), None, "{bad}");
        }
        let prompt = |ts: &str| format!(r#"{{"type":"user","origin":{{"kind":"human"}},"timestamp":"{ts}"}}"#);
        let result = r#"{"type":"user","timestamp":"2026-10-24T17:00:00Z","message":{"content":[]}}"#;
        let transcript = [prompt("2026-10-24T16:00:00Z"), prompt("2026-10-24T16:37:19Z"), result.into()].join("\n");
        assert_eq!(turn_started(&transcript), Some(1_792_859_839));
        assert_eq!(turn_started(""), None);
    }

    #[test]
    fn plan_verdicts_decide() {
        let comments = "# Review: plan.md (1 comment)\n\n## plan.md:3\n> Step 2\n\nSkip this step.\n";
        // Discarded, closed, vjuu failed, or no verdict: leave it to Claude Code's own prompt.
        for review in ["", "discard\n# Review: plan.md\n", "# Review: plan.md\nNo comments.\n", comments] {
            assert_eq!(plan_decision(review), None, "{review:?}");
        }
        let decide = |review: &str| -> serde_json::Value {
            let decision: serde_json::Value = serde_json::from_str(&plan_decision(review).unwrap()).unwrap();
            decision["hookSpecificOutput"].clone()
        };
        let redo = format!("redo\n{comments}");
        let out = decide(&redo);
        assert_eq!((&out["hookEventName"], &out["permissionDecision"]), (&"PreToolUse".into(), &"deny".into()));
        assert!(out["permissionDecisionReason"].as_str().unwrap().ends_with(redo.trim()), "{out}");
        // Approved exits plan mode, instead of falling through to Claude Code's prompt; comments come along.
        let out = decide("approved\n# Review: plan.md\nNo comments.\n");
        assert_eq!(out["permissionDecision"], "allow");
        assert!(out.get("additionalContext").is_none(), "{out}");
        let approved = format!("approved\n{comments}");
        let out = decide(&approved);
        assert_eq!(out["permissionDecision"], "allow");
        assert!(out["additionalContext"].as_str().unwrap().ends_with(approved.trim()), "{out}");
    }

    #[test]
    fn revised_plans_open_as_their_changes() {
        let dir = std::env::temp_dir().join(format!("vjuu-revised-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let plan = dir.join("plan.md");
        let kept = dir.join("session");
        fs::write(&plan, "# Plan\n\nStep one\n").unwrap();
        assert_eq!(previous_plan(&plan, &kept), None, "first time: nothing to compare with");
        assert_eq!(previous_plan(&plan, &kept), None, "shown again unchanged");
        fs::write(&plan, "# Plan\n\nStep two\n").unwrap();
        let previous = previous_plan(&plan, &kept).unwrap();
        assert_eq!(fs::read_to_string(&previous).unwrap(), "# Plan\n\nStep one\n");

        let name = plan.to_str().unwrap();
        let doc = revised_doc(previous.to_str().unwrap(), name);
        assert_eq!((doc.kind.as_str(), doc.text.as_str()), ("md", "# Plan\n\nStep two\n"));
        let changes = doc.changes.unwrap();
        assert!(changes.starts_with(&format!("--- a/{name}\n+++ b/{name}\n@@ ")), "{changes}");
        assert!(changes.ends_with("-Step one\n+Step two\n"), "{changes}");
        assert_eq!(revised_doc(name, name).changes, None, "no changes");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn session_ids_stay_in_their_folder() {
        let sessions = session_dir("x").parent().unwrap().to_path_buf();
        assert_eq!(session_dir("0b1c-9f"), sessions.join("0b1c-9f"));
        for id in ["../../evil", "..\\..", ""] {
            assert_eq!(session_dir(id).parent().unwrap(), sessions, "{id}");
        }
    }

    #[test]
    fn file_kinds() {
        for (path, text, kind) in [
            ("src/main.rs", "fn main() {}\n", "code"),
            ("notes.TXT", "plain\n", "code"),
            ("docs/plan.md", "# Plan\n", "md"),
            ("site/index.HTML", "<!doctype html>\n", "html"),
            ("old.htm", "<p>\n", "html"),
            ("page.html.diff", "", "diff"),
            ("README", "# Title\n", "md"),
            ("change.patch", "", "diff"),
            ("saved.txt", "diff --git a/x b/x\n", "diff"),
            // A markdown file that quotes a diff in a fenced block is still markdown.
            ("docs/plan.md", "# Plan\n```diff\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n```\n", "md"),
            // Stdin has no extension: detected from the content.
            ("", "diff --git a/x b/x\n", "diff"),
            ("", "# Notes\n", "md"),
        ] {
            assert_eq!(kind_of(path, text), kind, "{path}");
        }
    }

    #[test]
    fn opens_file_urls() {
        let path = std::env::temp_dir().join(format!("vjuu url {}.html", std::process::id()));
        fs::write(&path, "<p>hi</p>").unwrap();
        // Percent-encoded, as a browser shows it.
        let doc = file_doc(url::Url::from_file_path(&path).unwrap().as_str());
        assert_eq!(
            (doc.name.as_str(), doc.kind.as_str(), doc.text.as_str()),
            (path.to_str().unwrap(), "html", "<p>hi</p>")
        );
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn session_titles() {
        let ai = r#"{"type":"ai-title","aiTitle":"Generated"}"#;
        let renamed = r#"{"type":"custom-title","customTitle":"Mine"}"#;
        let quoted = r#"{"type":"user","message":"{\"customTitle\":\"quoted\"}"}"#;
        for (lines, title) in [
            (vec![], None),
            (vec![ai], None),
            (vec![renamed, ai], Some("Mine")),
            (vec![renamed, quoted], Some("Mine")),
            (vec![quoted, "not json customTitle"], None),
        ] {
            assert_eq!(transcript_title(&lines.join("\n")).as_deref(), title, "{lines:?}");
        }
    }

    #[test]
    fn only_vjuu_urls_are_trusted() {
        for (url, trusted) in [
            ("vjuu://localhost/", true),
            ("http://vjuu.localhost/", true),
            ("http://vjuu.localhost/files/0/a.css", true),
            ("http://vjuu.localhost.evil.example/", false),
            ("about:srcdoc", false),
            ("null", false),
            ("https://example.com/", false),
        ] {
            assert_eq!(is_vjuu(url), trusted, "{url}");
        }
    }

    #[test]
    fn detects_diffs() {
        assert!(is_diff("diff --git a/x b/x\n"));
        assert!(is_diff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n"));
        assert!(!is_diff("# Title\n\n--- \n\nnot a diff\n"));
        assert!(!is_diff("- a list\n- item\n"));
    }

    #[test]
    fn installs_the_skills_only_for_agents_present() {
        let home = std::env::temp_dir().join(format!("vjuu-setup-{}", std::process::id()));
        let skill = |root: &str, name: &str| home.join(root).join("skills").join(name).join("SKILL.md");
        let all = |root: &str| ["vjuu", "ship", "sync"].map(|n| skill(root, n));
        fs::create_dir_all(&home).unwrap();
        assert_eq!(install_skill(&home).unwrap(), Installed::default());
        fs::create_dir_all(home.join(".codex")).unwrap();
        assert_eq!(install_skill(&home).unwrap().written, all(".agents"));
        fs::create_dir_all(home.join(".claude")).unwrap();
        let done = install_skill(&home).unwrap();
        assert_eq!(done.written, [all(".claude"), all(".agents")].concat());
        // Running it again (after an upgrade) overwrites vjuu's own skills.
        for (path, (_, text)) in done.written.iter().zip(SKILLS.iter().chain(SKILLS.iter())) {
            assert_eq!(&fs::read_to_string(path).unwrap(), text, "{}", path.display());
        }
        assert!(SKILLS[1..].iter().all(|(_, text)| text.contains(SHIPPED)) && !SKILL.contains(SHIPPED));
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn keeps_the_users_own_ship_and_sync() {
        let home = std::env::temp_dir().join(format!("vjuu-setup-own-{}", std::process::id()));
        let own_skill = home.join(".claude").join("skills").join("ship").join("SKILL.md");
        let own_command = home.join(".claude").join("commands").join("sync.md");
        for file in [&own_skill, &own_command] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "mine").unwrap();
        }
        let done = install_skill(&home).unwrap();
        assert_eq!(done.kept, [own_skill.clone(), own_command.clone()]);
        assert_eq!(done.written, [home.join(".claude").join("skills").join("vjuu").join("SKILL.md")]);
        assert_eq!(fs::read_to_string(&own_skill).unwrap(), "mine");
        assert!(!home.join(".claude").join("skills").join("sync").exists());
        fs::remove_dir_all(&home).unwrap();
    }
}
