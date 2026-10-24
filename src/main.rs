mod config;
mod platform;
mod review;
mod theme;
mod tray;

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, fs, thread};

use interprocess::local_socket::{ListenerOptions, Stream, prelude::*};
use serde::{Deserialize, Serialize};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{ResizeDirection, Window, WindowBuilder, WindowId};
use wry::http::Response;
use wry::{WebContext, WebView, WebViewBuilder};

use platform::{bring_to_front, pipe_name};
use review::Review;

const HELP: &str = "\
vjuu: a review window for agent handoffs. Comment on lines, click Done, and the review comes back on stdout.

Usage:
  vjuu <file>...                 Review files in one window: markdown, a diff or patch, an HTML page
                                 (or file:// URL), or any source file
  vjuu -                         Review stdin: a diff or markdown
  vjuu --git [<git diff args>]   Review `git diff` in the current directory
  vjuu --session <name> ...      Name the asking session in the title bar (automatic under Claude Code)
  vjuu last                      Print the most recent review again

  vjuu config                    Open the settings
  vjuu config list               Print the settings and where they are saved
  vjuu config <setting> <value>  Change one: plan, ask, turn, permission, tray or focus (on / off),
                                 theme (a theme or auto), theme.light or theme.dark (a theme)
  vjuu setup                     Install the skill for Claude Code and Codex
  vjuu stop                      Stop the resident instance (before upgrading)
  vjuu hook <name>               Claude Code hooks: plan, ask, turn or permission

  vjuu -h, --help                Show this
  vjuu -v, --version             Show the version

More at https://vjuu.it";
/// The skill that tells agents to run vjuu; built in, so it always matches this version.
const SKILL: &str = include_str!("../skills/vjuu/SKILL.md");
/// Wait before warming the next spare window, so it does not compete with the one just shown.
const SPARE_DELAY: Duration = Duration::from_millis(500);

#[derive(Serialize, Deserialize)]
struct Doc {
    name: String,
    kind: String,
    text: String,
    /// Directory the document's relative images resolve against.
    base: Option<PathBuf>,
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
    Done,
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
}

enum AppEvent {
    /// A window to show; the stream is the client waiting for its review, None when vjuu opened it itself.
    Open(Request, Option<Stream>),
    Ui(WindowId, Msg),
    Tray(tray::Action),
    /// A window's git context, looked up after it opened.
    Context(WindowId, String),
}

/// Where a finished review goes.
enum Reply {
    Stdout,
    Client(Stream),
}

type Slot = Arc<Mutex<Option<Request>>>;

struct Session {
    // Declared before `window` and `_context` so it is dropped first.
    webview: WebView,
    window: Window,
    /// This window's own web context where the platform needs one (see `platform::CONTEXT_PER_WINDOW`).
    _context: Option<WebContext>,
    slot: Slot,
    reply: Option<Reply>,
    latest: Review,
    started: Instant,
    /// The git context, kept to hand to the page again if it arrived before the page could take it.
    context: Option<String>,
    /// The settings panel's Apply saved, so a settings-only window can say so when it closes.
    applied: bool,
    /// Opened behind the window in use (the `focus` setting off), so it is never brought to the front by itself.
    background: bool,
}

fn main() {
    let started = Instant::now();
    let mut args: Vec<String> = env::args().skip(1).collect();
    // Only as the first argument, so it never eats a `--git` argument.
    let session = (args.len() > 1 && args[0] == "--session").then(|| args.drain(..2).nth(1).expect("checked length"));
    let mut request = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["-h" | "--help"] => {
            println!("{HELP}");
            process::exit(0)
        }
        ["-v" | "--version"] => {
            println!("vjuu {}", env!("CARGO_PKG_VERSION"));
            process::exit(0)
        }
        // Nothing to review: the help, but as an error, since a script calling vjuu so has a bug.
        [] => {
            eprintln!("{HELP}");
            process::exit(2)
        }
        ["last"] => print_last(),
        ["stop"] => stop(),
        ["setup"] => setup(),
        ["config"] => settings_request(),
        ["config", "list"] => print_config(),
        ["config", key, value] => set_config(key, value),
        ["hook", "plan"] => hook_plan(),
        ["hook", "turn"] => hook_turn(),
        ["hook", "ask"] => hook_ask(),
        ["hook", "permission"] => hook_permission(),
        ["--serve"] => run(None, started),
        ["--git", ref rest @ ..] => git_diff(rest),
        ["-"] => {
            let mut bytes = Vec::new();
            std::io::stdin().read_to_end(&mut bytes).unwrap_or_else(|e| fail(&format!("Cannot read stdin: {e}")));
            let text = String::from_utf8_lossy(&bytes).into_owned();
            let doc =
                Doc { name: "stdin".into(), kind: kind_of("", &text).into(), text, base: env::current_dir().ok() };
            request(vec![doc])
        }
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

/// Returns the `/rename` name of the Claude Code session with this id.
fn session_title(id: &str) -> Option<String> {
    let root = env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| {
        env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")).map(|home| PathBuf::from(home).join(".claude"))
    })?;
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

/// Claude Code's PreToolUse hook on ExitPlanMode: opens the plan in vjuu, and comments send it back for changes.
/// Without comments it decides nothing, so Claude Code's own approval prompt follows and picks the next mode.
fn hook_plan() -> ! {
    if !config::load().hooks.plan {
        process::exit(0)
    }
    let event = hook_event();
    // Claude Code injects the plan file's path; without one there is nothing to anchor comments to.
    let Some(plan) = event["tool_input"]["planFilePath"].as_str().filter(|p| Path::new(p).is_file()) else {
        process::exit(0)
    };
    if let Some(decision) = plan_decision(&review_in_child(&event, Path::new(plan))) {
        println!("{decision}");
    }
    process::exit(0)
}

/// Claude Code's Stop hook: shows the diff of the files Claude edited this turn, and comments go back to Claude.
/// Without edits, or without comments, it lets the turn end.
fn hook_turn() -> ! {
    if !config::load().hooks.turn {
        process::exit(0)
    }
    let event = hook_event();
    let Some(cwd) = event["cwd"].as_str() else { process::exit(0) };
    let transcript = event["transcript_path"].as_str().and_then(|p| fs::read_to_string(p).ok()).unwrap_or_default();
    let diff = turn_diff(Path::new(cwd), &edited_this_turn(&transcript));
    // One file per session, so parallel sessions do not overwrite each other's diff.
    let path = env::temp_dir().join(format!("vjuu-turn-{}.diff", event["session_id"].as_str().unwrap_or("none")));
    if diff.is_empty() || fs::write(&path, diff).is_err() {
        process::exit(0)
    }
    if let Some(feedback) = turn_feedback(&review_in_child(&event, &path)) {
        println!("{feedback}");
    }
    process::exit(0)
}

/// Claude Code's PreToolUse hook on AskUserQuestion: asks the questions in vjuu, under Claude's last message.
/// Picks alone answer the tool; comments or unanswered questions go back to Claude as the whole review instead.
/// Closing the window without answering leaves the questions to Claude Code's own picker.
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
    if let Some(decision) = ask_decision(&event["tool_input"], &review_in_child(&event, &path)) {
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
    if !has_comments(review) {
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
/// A pick decides; comments without one deny, with the comments as the reason; nothing leaves the terminal prompt.
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
    if let Some(decision) = permission_decision(&event, &review_in_child(&event, &path)) {
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

/// Returns the PermissionRequest hook output for a review, or None when the user decided nothing there.
fn permission_decision(event: &serde_json::Value, review: &str) -> Option<String> {
    if !has_comments(review) {
        return None;
    }
    let (picks, _) = picks(review);
    let decision = match picks.get(PERMISSION_QUESTION).map(String::as_str) {
        Some("Allow") => serde_json::json!({ "behavior": "allow" }),
        Some("Always allow") => {
            serde_json::json!({ "behavior": "allow", "updatedPermissions": event["permission_suggestions"] })
        }
        // Deny, or comments without a pick.
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

/// Runs vjuu on `path` as a child titled with the hook session's name, and returns the review it prints.
fn review_in_child(event: &serde_json::Value, path: &Path) -> String {
    let Ok(exe) = env::current_exe() else { return String::new() };
    let mut vjuu = Command::new(exe);
    if let Some(title) = event["session_id"].as_str().and_then(session_title) {
        vjuu.args(["--session", &title]);
    }
    let out = vjuu.arg(path).stdin(process::Stdio::null()).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

/// Returns whether a review has anything for the agent: empty output (vjuu failed) or "No comments." has not.
fn has_comments(review: &str) -> bool {
    let review = review.trim();
    !review.is_empty() && !review.ends_with("\nNo comments.")
}

/// Returns the files Claude's edit tools touched since the user's last prompt.
/// shortcut: Claude Code's transcript format is undocumented; lines it does not recognise are skipped.
fn edited_this_turn(transcript: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in transcript.lines().rev() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        // Tool results and hook feedback are user entries too; only what the user typed starts a turn.
        if entry["type"] == "user" && entry["origin"]["kind"] == "human" {
            break;
        }
        for block in entry["message"]["content"].as_array().into_iter().flatten() {
            let input = &block["input"];
            let file = match block["name"].as_str() {
                Some("Edit" | "MultiEdit" | "Write") if block["type"] == "tool_use" => input["file_path"].as_str(),
                Some("NotebookEdit") if block["type"] == "tool_use" => input["notebook_path"].as_str(),
                _ => None,
            };
            files.extend(file.map(String::from));
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Returns the uncommitted changes to those of `files` inside the git repository at `cwd`, new files included.
/// shortcut: changes from earlier turns to the same files show too, as git cannot tell turns apart.
fn turn_diff(cwd: &Path, files: &[String]) -> String {
    let git = |args: &[&str]| {
        let out = Command::new("git").current_dir(cwd).args(["-c", "core.quotepath=off"]).args(args).output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
    };
    let Ok(top) = Path::new(git(&["rev-parse", "--show-toplevel"]).trim()).canonicalize() else {
        return String::new();
    };
    // git refuses paths outside the repository, and they are not this review's business anyway.
    let inside: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| Path::new(f).canonicalize().is_ok_and(|p| p.starts_with(&top)))
        .collect();
    if inside.is_empty() {
        return String::new();
    }
    let mut diff = git(&[&["diff", "--no-color", "--no-ext-diff", "HEAD", "--"][..], &inside].concat());
    for new in git(&[&["ls-files", "--others", "--exclude-standard", "--"][..], &inside].concat()).lines() {
        diff += &git(&["diff", "--no-color", "--no-ext-diff", "--no-index", "--", "/dev/null", new]);
    }
    diff
}

/// Returns the hook output that hands the review of this turn's changes to Claude, or None without comments.
fn turn_feedback(review: &str) -> Option<String> {
    if !has_comments(review) {
        return None;
    }
    let context = format!(
        "The user reviewed this turn's changes in vjuu. Act on each comment that asks for a change and answer each \
         question. If no comment asks for anything, just stop.\n\n{}",
        review.trim()
    );
    Some(
        serde_json::json!({ "hookSpecificOutput": { "hookEventName": "Stop", "additionalContext": context } })
            .to_string(),
    )
}

/// Returns the hook output that rejects the plan with the review as the reason, or None when there is nothing to act on.
fn plan_decision(review: &str) -> Option<String> {
    if !has_comments(review) {
        return None;
    }
    let review = review.trim();
    let reason = format!(
        "The user reviewed the plan in vjuu. Act on each comment that asks for a change, update the plan file, \
         then call ExitPlanMode again. If no comment asks for a change (\"LGTM\"), call ExitPlanMode again \
         right away.\n\n{review}"
    );
    let output = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    });
    Some(output.to_string())
}

fn request(docs: Vec<Doc>) -> Request {
    let autoclose = env::var_os("VJUU_AUTOCLOSE").is_some();
    Request { docs, autoclose, session: None, cwd: env::current_dir().ok(), settings: false, sample: false }
}

/// Returns a window for the settings panel alone, with vjuu's README and a sample diff behind it, so a theme preview
/// shows markdown, code and diff colours.
fn settings_request() -> Request {
    let doc = |name: &str, kind: &str, text: &str| Doc {
        name: name.into(),
        kind: kind.into(),
        text: text.into(),
        base: None,
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
    Doc { name: path.into(), kind: kind_of(path, &text).into(), text, base }
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
    request(vec![Doc { name, kind: "diff".into(), text, base: env::current_dir().ok() }])
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

/// Stops the resident instance, e.g. before replacing the exe. Open review windows close without a review.
fn stop() -> ! {
    if let Ok(mut conn) = Stream::connect(pipe_name()) {
        let _ = conn.write_all(b"stop\n").and_then(|_| conn.flush());
    }
    process::exit(0)
}

fn spawn_server() {
    if let Ok(exe) = env::current_exe() {
        platform::spawn_detached(Command::new(exe).arg("--serve"));
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

/// Runs the window event loop: one visible document (cold) or resident with a pre-warmed spare window.
fn run(initial: Option<Request>, started: Instant) -> ! {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    // Keep the webview profile out of the exe's directory (`~/.cargo/bin` after `cargo install`).
    let mut context = WebContext::new(Some(platform::data_dir().join("webview")));
    let mut sessions: HashMap<WindowId, Session> = HashMap::new();
    let mut spare: Option<Session> = None;
    let mut spare_due = None;
    // Only the resident instance has a tray: it outlives the windows and holds every session's.
    let resident = initial.is_none();
    let mut tray: Option<tray::Tray> = None;
    // How many waiting windows the tray shows; MAX makes a new tray icon catch up.
    let mut shown = usize::MAX;
    let tray_proxy = Arc::new(Mutex::new(proxy.clone()));
    let new_tray = move || {
        let proxy = tray_proxy.clone();
        tray::Tray::new(move |action| {
            let _ = proxy.lock().expect("proxy lock").send_event(AppEvent::Tray(action));
        })
    };
    // Read once, like the tray setting; Apply in the panel updates it, `vjuu config` from the next resident instance.
    let mut focus = config::load().focus;
    match initial {
        Some(request) => {
            let title = request.title();
            let cwd = request.cwd.clone();
            let mut s = new_session(&event_loop, &proxy, &mut context, Some(request), focus);
            look_up_context(&proxy, s.window.id(), cwd);
            s.window.set_title(&title);
            s.reply = Some(Reply::Stdout);
            s.started = started;
            s.background = !focus;
            if focus {
                bring_to_front(&s.window, &s.webview);
            }
            sessions.insert(s.window.id(), s);
        }
        None => {
            listen(proxy.clone());
            spare_due = Some(Instant::now());
        }
    }
    event_loop.run(move |event, target, control_flow| {
        match event {
            // macOS wants the tray made once the loop runs; the other platforms do not mind.
            Event::NewEvents(StartCause::Init) if resident && config::load().tray => tray = new_tray(),
            Event::UserEvent(AppEvent::Tray(tray::Action::Oldest)) => {
                if let Some(s) = sessions.values().min_by_key(|s| s.started) {
                    s.window.set_minimized(false);
                    bring_to_front(&s.window, &s.webview);
                }
            }
            Event::UserEvent(AppEvent::Tray(tray::Action::Settings)) => {
                let _ = proxy.send_event(AppEvent::Open(settings_request(), None));
            }
            Event::UserEvent(AppEvent::Tray(tray::Action::Stop)) => process::exit(0),
            Event::UserEvent(AppEvent::Context(id, git)) => {
                if let Some(s) = sessions.get_mut(&id) {
                    let title = s.slot.lock().expect("slot lock").as_ref().map(Request::title).unwrap_or_default();
                    s.window.set_title(&format!("{title} · {git}"));
                    s.context = Some(git);
                    show_context(s);
                }
            }
            Event::UserEvent(AppEvent::Open(request, conn)) => {
                let title = request.title();
                let cwd = request.cwd.clone();
                let mut s = spare.take().unwrap_or_else(|| new_session(target, &proxy, &mut context, None, true));
                look_up_context(&proxy, s.window.id(), cwd);
                *s.slot.lock().expect("slot lock") = Some(request);
                s.reply = conn.map(Reply::Client);
                s.started = Instant::now();
                s.window.set_title(&title);
                let _ = s.webview.evaluate_script("window.vjuuLoad && vjuuLoad()");
                s.background = !focus;
                if focus {
                    s.window.set_visible(true);
                    bring_to_front(&s.window, &s.webview);
                } else {
                    // Unfocusable while it appears, so Windows does not activate it; clickable again right after.
                    s.window.set_focusable(false);
                    s.window.set_visible(true);
                    s.window.set_focusable(true);
                }
                sessions.insert(s.window.id(), s);
                spare_due = Some(Instant::now() + SPARE_DELAY);
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::State { review })) => {
                if let Some(s) = sessions.get_mut(&id) {
                    s.latest = review;
                }
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Settings { config })) => {
                match config::save(&config) {
                    Ok(()) => {
                        if let Some(s) = sessions.get_mut(&id) {
                            s.applied = true;
                        }
                    }
                    Err(e) => eprintln!("Cannot save {}: {e}", config::path().display()),
                }
                match theme::write_compiled(&config.theme) {
                    // Every open window recolours, not only the one with the panel, and so does the spare.
                    Ok(()) => sessions.values().chain(&spare).for_each(|s| {
                        let _ = s.webview.evaluate_script("window.vjuuThemeSaved && vjuuThemeSaved()");
                    }),
                    Err(e) => eprintln!("Cannot compile the theme: {e}"),
                }
                focus = config.focus;
                if !config.tray {
                    tray = None;
                } else if resident && tray.is_none() {
                    tray = new_tray();
                    shown = usize::MAX;
                }
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Done)) => finish(&mut sessions, id, true),
            Event::UserEvent(AppEvent::Ui(id, Msg::Ready)) => {
                if let Some(s) = sessions.get(&id) {
                    show_context(s);
                    if !s.background {
                        bring_to_front(&s.window, &s.webview);
                    }
                    // Opened behind on purpose, or the OS kept it there (focus-stealing prevention): say so.
                    if tray.is_some() && !s.window.is_focused() {
                        notify(s.window.title());
                    }
                }
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Rendered)) => {
                if let Some(s) = sessions.get(&id)
                    && matches!(s.reply, Some(Reply::Stdout))
                {
                    eprintln!("rendered in {} ms", s.started.elapsed().as_millis());
                }
                finish(&mut sessions, id, false)
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                finish(&mut sessions, window_id, true)
            }
            // Activation focuses the native window, not the webview inside it; pass keyboard focus on, or keys
            // go nowhere until the page is clicked (seen with resident windows and after alt-tab).
            Event::WindowEvent { window_id, event: WindowEvent::Focused(true), .. } => {
                if let Some(s) = sessions.get(&window_id) {
                    let _ = s.webview.focus();
                }
            }
            Event::UserEvent(AppEvent::Ui(id, msg)) => {
                if let Some(s) = sessions.get(&id) {
                    window_command(&s.window, msg);
                }
            }
            _ => {}
        }
        if let Some(tray) = &tray
            && sessions.len() != shown
        {
            tray.show_waiting(sessions.len());
            shown = sessions.len();
        }
        if spare.is_none() && spare_due.is_some_and(|due| due <= Instant::now()) {
            spare = Some(new_session(target, &proxy, &mut context, None, true));
            spare_due = None;
        }
        *control_flow = match spare_due {
            Some(due) => ControlFlow::WaitUntil(due),
            None => ControlFlow::Wait,
        };
    })
}

/// Looks up the git context of `cwd` on a thread and hands it to the window, twice: the checkout at once, and again
/// with the PR or MR number once `gh` or `glab` answers over the network. The window never waits for either.
fn look_up_context(proxy: &EventLoopProxy<AppEvent>, id: WindowId, cwd: Option<PathBuf>) {
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

fn show_context(s: &Session) {
    if let Some(git) = &s.context {
        let git = serde_json::to_string(git).expect("string serialises");
        let _ = s.webview.evaluate_script(&format!("window.vjuuContext && vjuuContext({git})"));
    }
}

/// Returns a command's trimmed output, or None when it fails, prints nothing or is not installed.
fn run_in(cwd: &Path, program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).current_dir(cwd).args(args).stdin(process::Stdio::null()).output().ok()?;
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
        Msg::State { .. } | Msg::Done | Msg::Rendered | Msg::Ready | Msg::Settings { .. } => {}
    }
}

/// Creates a window and webview; without a request it stays hidden until a client hands one over. `focus` false
/// opens a shown window behind the one in use.
fn new_session(
    target: &EventLoopWindowTarget<AppEvent>,
    proxy: &EventLoopProxy<AppEvent>,
    context: &mut WebContext,
    request: Option<Request>,
    focus: bool,
) -> Session {
    // A spare window is built while the user is reading another one: it must not take focus, or their keys
    // go to the hidden spare.
    let shown = request.is_some();
    let builder = WindowBuilder::new()
        .with_title("vjuu")
        .with_visible(shown)
        .with_focused(shown && focus)
        .with_inner_size(LogicalSize::new(1100.0, 900.0));
    let window = platform::frame(builder).build(target).unwrap_or_else(|e| fail(&format!("Cannot open window: {e}")));
    platform::style_frame(&window);
    platform::fit_height(&window);
    let slot: Slot = Arc::new(Mutex::new(request));
    let proxy = proxy.clone();
    let id = window.id();
    let serve_slot = slot.clone();
    let mut own = platform::CONTEXT_PER_WINDOW.then(|| WebContext::new(Some(platform::data_dir().join("webview"))));
    let context = own.as_mut().unwrap_or(context);
    let builder = WebViewBuilder::new_with_web_context(context)
        .with_focused(shown && focus)
        // Tells ui/ whether to draw its own title bar and resize edges.
        .with_initialization_script(format!("window.VJUU_FRAMELESS = {};", platform::FRAMELESS))
        .with_custom_protocol("vjuu".into(), move |_id, request| serve(request.uri().path(), &serve_slot))
        .with_url("vjuu://localhost/")
        .with_navigation_handler(|url| {
            if is_vjuu(&url) {
                return true;
            }
            // External links go to the default browser so the review page is never replaced.
            if url.starts_with("http://") || url.starts_with("https://") {
                platform::open_url(&url);
            }
            false
        })
        .with_ipc_handler(move |request| {
            // Only vjuu's own page: an HTML page under review runs its scripts in a frame, and must not be able to
            // finish the review or fill it in, should the platform hand frames the message channel.
            if !is_vjuu(&request.uri().to_string()) {
                eprintln!("Ignored a message from {}", request.uri());
                return;
            }
            match serde_json::from_str::<Msg>(request.body()) {
                Ok(msg) => {
                    let _ = proxy.send_event(AppEvent::Ui(id, msg));
                }
                Err(e) => eprintln!("Bad message from UI: {e}"),
            }
        });
    let webview =
        platform::build_webview(builder, &window).unwrap_or_else(|e| fail(&format!("Cannot create webview: {e}")));
    Session {
        webview,
        window,
        _context: own,
        slot,
        reply: None,
        latest: Review::default(),
        started: Instant::now(),
        context: None,
        applied: false,
        background: false,
    }
}

/// Returns whether `url` is vjuu's own page or one of its files.
fn is_vjuu(url: &str) -> bool {
    // WebView2 serves custom protocols as http://<scheme>.localhost; WebKit keeps vjuu://.
    url.starts_with("vjuu://localhost/") || url.starts_with("http://vjuu.localhost/")
}

/// Saves the review and sends it where it was asked for; `save` is false for autoclose timing runs.
fn finish(sessions: &mut HashMap<WindowId, Session>, id: WindowId, save: bool) {
    let Some(s) = sessions.remove(&id) else { return };
    let (name, only_settings) = match s.slot.lock().expect("slot lock").as_ref() {
        Some(r) => (r.name(), r.sample),
        None => (String::new(), false),
    };
    let md = match (save, only_settings) {
        // Nothing to review: Apply saved the settings, or Esc left them as they were.
        (_, true) if s.applied => "Settings saved.\n".into(),
        (_, true) => "Settings unchanged.\n".into(),
        (true, false) => save_review(&name, &s.latest),
        (false, false) => String::new(),
    };
    match s.reply {
        Some(Reply::Stdout) => {
            // Tear the webview down before exiting, or Chromium logs a window-class error to stderr.
            drop(s.webview);
            drop(s.window);
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
        // Read when the panel opens, so a change made with `vjuu config` meanwhile shows; with the themes to pick from.
        "/config.json" => {
            let mut config = serde_json::to_value(config::load()).expect("Config serialises");
            config["themes"] = serde_json::to_value(theme::list()).expect("themes serialise");
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

/// Returns the review as markdown after writing it to the reviews directory.
fn save_review(name: &str, review: &Review) -> String {
    let md = review::to_markdown(name, review);
    let dir = reviews_dir();
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let slug: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).take(60).collect();
    let path = dir.join(format!("{secs}-{slug}.md"));
    if let Err(e) = fs::create_dir_all(&dir).and_then(|_| fs::write(&path, &md)) {
        eprintln!("Could not save review to {}: {e}", path.display());
    }
    md
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
    let written = install_skill(&home).unwrap_or_else(|e| fail(&format!("Cannot install the skill: {e}")));
    if written.is_empty() {
        fail("Found neither Claude Code (~/.claude) nor Codex (~/.codex). Install one, then run `vjuu setup` again.");
    }
    for path in written {
        println!("Installed {}", path.display());
    }
    // Written first, so the first review window does not show the panel a second time.
    if config::path().exists() || config::save(&config::Config::default()).is_err() {
        process::exit(0)
    }
    settings_request()
}

/// Writes the skill for Claude Code and Codex, where installed, and returns the files written.
fn install_skill(home: &Path) -> std::io::Result<Vec<PathBuf>> {
    // Codex reads user skills from ~/.agents/skills, a folder other agents share.
    // shortcut: ignores CLAUDE_CONFIG_DIR and CODEX_HOME; honour them if someone moves their config.
    let mut written = Vec::new();
    for (agent, skills) in [(".claude", ".claude"), (".codex", ".agents")] {
        if home.join(agent).is_dir() {
            let dir = home.join(skills).join("skills").join("vjuu");
            let path = dir.join("SKILL.md");
            fs::create_dir_all(&dir)?;
            fs::write(&path, SKILL)?;
            written.push(path);
        }
    }
    Ok(written)
}

fn fail(msg: &str) -> ! {
    eprintln!("{}", msg.trim_end());
    process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::{
        SKILL, ask_decision, ask_markdown, edited_this_turn, file_doc, install_skill, is_diff, is_vjuu, kind_of,
        last_assistant_text, plan_decision, transcript_title, turn_diff, turn_feedback,
    };
    use super::{change_request, checkout_context, fence, permission_decision, permission_markdown};

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
        assert_eq!(decide("# Review: p.md\nNo comments.\n"), None);
        assert_eq!(decide(""), None);
        let picked = |answer: &str| format!("# Review: p.md\n\n## Choice: Allow this?\n{answer}\n");
        let decision = |review: &str| decide(review).unwrap()["hookSpecificOutput"]["decision"].clone();
        assert_eq!(decision(&picked("Allow")), serde_json::json!({ "behavior": "allow" }));
        assert_eq!(
            decision(&picked("Always allow")),
            serde_json::json!({ "behavior": "allow", "updatedPermissions": event["permission_suggestions"] })
        );
        let because = "# Review: p.md (1 comment)\n\n## p.md:5\n> npm test\n\nRun only the unit tests.\n";
        for review in [picked("Deny"), because.to_string()] {
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
        // Closed without a pick: Claude Code's picker asks instead.
        assert_eq!(decide("# Review: ask.md\nNo comments.\n"), None);
        assert_eq!(decide(""), None);

        let picked = "# Review: ask.md\n\n## Choice: Which framework?\nVue\n\n## Choice: Ship now?\nNo\n";
        let mut expected = input.clone();
        expected["answers"] = serde_json::json!({ "Which framework?": "Vue", "Ship\n now?": "No" });
        assert_eq!(
            decide(picked),
            Some(serde_json::json!({ "hookSpecificOutput": {
                "hookEventName": "PreToolUse", "permissionDecision": "allow", "updatedInput": expected } }))
        );

        let commented = format!("{picked}\n## ask.md:9\n> - Vue\n\nVue 3 only.\n");
        let unanswered =
            "# Review: ask.md\n\n## Choice: Which framework?\nReact\n\n## Choice: Ship now?\nNo answer.\n";
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
            prompt("second"),
            tool("Read", "file_path", "read.rs"),
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
    }

    #[test]
    fn turn_comments_go_back_to_claude() {
        for review in ["", "# Review: x.diff\nNo comments.\n"] {
            assert_eq!(turn_feedback(review), None, "{review:?}");
        }
        let review = "# Review: x.diff (1 comment)\n\n## a.rs:3\n> +x\n\nWhy?\n";
        let feedback: serde_json::Value = serde_json::from_str(&turn_feedback(review).unwrap()).unwrap();
        assert_eq!(feedback["hookSpecificOutput"]["hookEventName"], "Stop");
        let context = feedback["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
        assert!(context.ends_with(review.trim()), "{context}");
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
        let diff = turn_diff(&repo, &files);
        for expected in ["+++ b/edited.txt", "-old", "+new", "created.txt", "+fresh"] {
            assert!(diff.contains(expected), "{expected} missing from\n{diff}");
        }
        assert!(!diff.contains("untouched") && !diff.contains("elsewhere"), "{diff}");
        assert_eq!(turn_diff(&repo, &[]), "");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn plan_comments_reject_the_plan() {
        // Closed without comments, or vjuu failed: leave it to Claude Code's own prompt.
        for review in ["", "# Review: plan.md\nNo comments.\n"] {
            assert_eq!(plan_decision(review), None, "{review:?}");
        }
        let review = "# Review: plan.md (1 comment)\n\n## plan.md:3\n> Step 2\n\nSkip this step.\n";
        let decision: serde_json::Value = serde_json::from_str(&plan_decision(review).unwrap()).unwrap();
        assert_eq!(decision["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = decision["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.ends_with(review.trim()), "{reason}");
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
    fn installs_the_skill_only_for_agents_present() {
        let home = std::env::temp_dir().join(format!("vjuu-setup-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        assert!(install_skill(&home).unwrap().is_empty());
        fs::create_dir_all(home.join(".codex")).unwrap();
        assert_eq!(install_skill(&home).unwrap(), [home.join(".agents").join("skills").join("vjuu").join("SKILL.md")]);
        fs::create_dir_all(home.join(".claude")).unwrap();
        let written = install_skill(&home).unwrap();
        assert_eq!(
            written,
            [
                home.join(".claude").join("skills").join("vjuu").join("SKILL.md"),
                home.join(".agents").join("skills").join("vjuu").join("SKILL.md")
            ]
        );
        // Running it again (after an upgrade) overwrites the old skill.
        assert!(written.iter().all(|p| fs::read_to_string(p).unwrap() == SKILL));
        fs::remove_dir_all(&home).unwrap();
    }
}
