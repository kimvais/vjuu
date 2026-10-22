mod review;

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, fs, thread};

use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Name, Stream, prelude::*};
use serde::{Deserialize, Serialize};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::WindowBuilderExtWindows;
use tao::window::{ResizeDirection, Window, WindowBuilder, WindowId};
use wry::http::Response;
use wry::{WebContext, WebView, WebViewBuilder};

use review::Review;

const USAGE: &str = "usage: vjuu <file.md|file.diff> | vjuu - | vjuu --git [git diff args] | vjuu last | vjuu stop";
/// Wait before warming the next spare window, so it does not compete with the one just shown.
const SPARE_DELAY: Duration = Duration::from_millis(500);
const DETACHED_PROCESS: u32 = 0x8;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;

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
}

impl Request {
    fn name(&self) -> String {
        self.docs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", ")
    }
}

/// Messages posted by ui/app.js via `window.ipc.postMessage`.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Msg {
    State { review: Review },
    Done,
    Rendered,
    // The window is frameless; the page draws the title bar and asks for these.
    Drag,
    Resize { dir: String },
    Minimize,
    Maximize,
    Close,
}

enum AppEvent {
    Open(Request, Stream),
    Ui(WindowId, Msg),
}

/// Where a finished review goes.
enum Reply {
    Stdout,
    Client(Stream),
}

type Slot = Arc<Mutex<Option<Request>>>;

struct Session {
    // Declared before `window` so it is dropped first.
    webview: WebView,
    window: Window,
    slot: Slot,
    reply: Option<Reply>,
    latest: Review,
    started: Instant,
}

fn main() {
    let started = Instant::now();
    let args: Vec<String> = env::args().skip(1).collect();
    let request = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["last"] => print_last(),
        ["stop"] => stop(),
        ["--serve"] => run(None, started),
        ["--git", ref rest @ ..] => git_diff(rest),
        ["-"] => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).unwrap_or_else(|e| fail(&format!("Cannot read stdin: {e}")));
            request(vec![doc("stdin".into(), text, false, env::current_dir().ok())])
        }
        ref paths @ [_, ..] if paths.iter().all(|p| !p.starts_with('-')) => request(paths.iter().map(|p| file_doc(p)).collect()),
        _ => {
            eprintln!("{USAGE}");
            process::exit(2)
        }
    };
    if env::var_os("VJUU_COLD").is_none() {
        match Stream::connect(pipe_name()) {
            Ok(conn) => client(conn, &request, started),
            // Nobody listening: show this one ourselves and start a resident instance for next time.
            Err(_) => spawn_server(),
        }
    }
    run(Some(request), started)
}

fn request(docs: Vec<Doc>) -> Request {
    Request { docs, autoclose: env::var_os("VJUU_AUTOCLOSE").is_some() }
}

fn doc(name: String, text: String, force_diff: bool, base: Option<PathBuf>) -> Doc {
    let kind = if force_diff || is_diff(&text) { "diff" } else { "md" };
    Doc { name, kind: kind.into(), text, base }
}

fn file_doc(path: &str) -> Doc {
    let text = fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("Cannot read `{path}`: {e}")));
    let base = Path::new(path).canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    Doc { name: path.into(), kind: kind_of(path, &text).into(), text, base }
}

/// Returns how a file is shown: "diff", "md", or "code" (line-numbered source; the UI picks the language).
fn kind_of(path: &str, text: &str) -> &'static str {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "diff" | "patch" => "diff",
        _ if is_diff(text) => "diff",
        // Extensionless files (README, NOTES) are usually prose.
        "md" | "markdown" | "" => "md",
        _ => "code",
    }
}

fn is_diff(text: &str) -> bool {
    text.starts_with("diff ") || (text.starts_with("--- ") || text.contains("\n+++ ")) && text.contains("\n@@ ")
}

fn git_diff(rest: &[&str]) -> Request {
    let out = Command::new("git").arg("diff").args(rest).output().unwrap_or_else(|e| fail(&format!("Cannot run git: {e}")));
    if !out.status.success() {
        fail(&String::from_utf8_lossy(&out.stderr));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if text.trim().is_empty() {
        println!("No changes to review.");
        process::exit(0);
    }
    let name = format!("git diff {}", rest.join(" ")).trim_end().to_string();
    request(vec![doc(name, text, true, env::current_dir().ok())])
}

fn pipe_name() -> Name<'static> {
    // shortcut: default pipe ACL; fine on a single-user machine, set a security descriptor if shared.
    format!("vjuu-{}.sock", env::var("USERNAME").unwrap_or_default())
        .to_ns_name::<GenericNamespaced>()
        .expect("valid pipe name")
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
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
    let Ok(exe) = env::current_exe() else { return };
    // CreateProcess inherits every inheritable handle, including our stdout pipe; the caller would then
    // wait for EOF until the resident instance exits.
    for std_handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe { SetHandleInformation(GetStdHandle(std_handle), HANDLE_FLAG_INHERIT, 0) };
    }
    let _ = Command::new(exe)
        .arg("--serve")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn();
}

/// Accepts clients on a thread and hands each request to the event loop.
fn listen(proxy: EventLoopProxy<AppEvent>) {
    let listener = match ListenerOptions::new().name(pipe_name()).create_sync() {
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
                    let _ = proxy.send_event(AppEvent::Open(request, reader.into_inner()));
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
    // Keep WebView2's profile out of the exe's directory (`~/.cargo/bin` after `cargo install`).
    let mut context = WebContext::new(Some(data_dir().join("webview")));
    let mut sessions: HashMap<WindowId, Session> = HashMap::new();
    let mut spare: Option<Session> = None;
    let mut spare_due = None;
    match initial {
        Some(request) => {
            let title = format!("vjuu: {}", request.name());
            let mut s = new_session(&event_loop, &proxy, &mut context, Some(request));
            s.window.set_title(&title);
            s.reply = Some(Reply::Stdout);
            s.started = started;
            sessions.insert(s.window.id(), s);
        }
        None => {
            listen(proxy.clone());
            spare_due = Some(Instant::now());
        }
    }
    event_loop.run(move |event, target, control_flow| {
        match event {
            Event::UserEvent(AppEvent::Open(request, conn)) => {
                let title = format!("vjuu: {}", request.name());
                let mut s = spare.take().unwrap_or_else(|| new_session(target, &proxy, &mut context, None));
                *s.slot.lock().expect("slot lock") = Some(request);
                s.reply = Some(Reply::Client(conn));
                s.started = Instant::now();
                s.window.set_title(&title);
                let _ = s.webview.evaluate_script("window.vjuuLoad && vjuuLoad()");
                s.window.set_visible(true);
                s.window.set_focus();
                sessions.insert(s.window.id(), s);
                spare_due = Some(Instant::now() + SPARE_DELAY);
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::State { review })) => {
                if let Some(s) = sessions.get_mut(&id) {
                    s.latest = review;
                }
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Done)) => finish(&mut sessions, id, true),
            Event::UserEvent(AppEvent::Ui(id, Msg::Rendered)) => {
                if let Some(s) = sessions.get(&id)
                    && matches!(s.reply, Some(Reply::Stdout))
                {
                    eprintln!("rendered in {} ms", s.started.elapsed().as_millis());
                }
                finish(&mut sessions, id, false)
            }
            Event::UserEvent(AppEvent::Ui(id, Msg::Close))
            | Event::WindowEvent { window_id: id, event: WindowEvent::CloseRequested, .. } => finish(&mut sessions, id, true),
            Event::UserEvent(AppEvent::Ui(id, msg)) => {
                if let Some(s) = sessions.get(&id) {
                    window_command(&s.window, msg);
                }
            }
            _ => {}
        }
        if spare.is_none() && spare_due.is_some_and(|due| due <= Instant::now()) {
            spare = Some(new_session(target, &proxy, &mut context, None));
            spare_due = None;
        }
        *control_flow = match spare_due {
            Some(due) => ControlFlow::WaitUntil(due),
            None => ControlFlow::Wait,
        };
    })
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
        Msg::State { .. } | Msg::Done | Msg::Rendered | Msg::Close => {}
    }
}

/// Asks Windows 11 for rounded corners, which frameless windows don't get by default; older Windows ignores it.
fn round_corners(window: &Window) {
    use tao::platform::windows::WindowExtWindows;
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute};
    let preference = DWMWCP_ROUND;
    unsafe {
        DwmSetWindowAttribute(
            window.hwnd() as _,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&raw const preference).cast(),
            size_of_val(&preference) as u32,
        );
    }
}

/// Creates a window and webview; without a request it stays hidden until a client hands one over.
fn new_session(
    target: &EventLoopWindowTarget<AppEvent>,
    proxy: &EventLoopProxy<AppEvent>,
    context: &mut WebContext,
    request: Option<Request>,
) -> Session {
    let window = WindowBuilder::new()
        .with_title("vjuu")
        .with_visible(request.is_some())
        .with_inner_size(LogicalSize::new(1100.0, 900.0))
        // Frameless like Windows Terminal: ui/ draws the title bar.
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .build(target)
        .unwrap_or_else(|e| fail(&format!("Cannot open window: {e}")));
    round_corners(&window);
    let slot: Slot = Arc::new(Mutex::new(request));
    let proxy = proxy.clone();
    let id = window.id();
    let serve_slot = slot.clone();
    let webview = WebViewBuilder::new_with_web_context(context)
        .with_custom_protocol("vjuu".into(), move |_id, request| serve(request.uri().path(), &serve_slot))
        .with_url("vjuu://localhost/")
        .with_navigation_handler(|url| {
            // WebView2 serves custom protocols as http://<scheme>.localhost.
            if url.starts_with("vjuu:") || url.starts_with("http://vjuu.localhost") {
                return true;
            }
            // External links go to the default browser so the review page is never replaced.
            if url.starts_with("http://") || url.starts_with("https://") {
                let _ = Command::new("explorer").arg(&url).spawn();
            }
            false
        })
        .with_ipc_handler(move |request| match serde_json::from_str::<Msg>(request.body()) {
            Ok(msg) => {
                let _ = proxy.send_event(AppEvent::Ui(id, msg));
            }
            Err(e) => eprintln!("Bad message from UI: {e}"),
        })
        .build(&window)
        .unwrap_or_else(|e| fail(&format!("Cannot create webview: {e}")));
    Session { webview, window, slot, reply: None, latest: Review::default(), started: Instant::now() }
}

/// Saves the review and sends it where it was asked for; `save` is false for autoclose timing runs.
fn finish(sessions: &mut HashMap<WindowId, Session>, id: WindowId, save: bool) {
    let Some(s) = sessions.remove(&id) else { return };
    let name = s.slot.lock().expect("slot lock").as_ref().map(Request::name).unwrap_or_default();
    let md = if save { save_review(&name, &s.latest) } else { String::new() };
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
        "/vendor/markdown-it.min.js" => (Cow::Borrowed(include_bytes!("../ui/vendor/markdown-it.min.js")), "text/javascript"),
        "/vendor/diff2html-ui-base.min.js" => (Cow::Borrowed(include_bytes!("../ui/vendor/diff2html-ui-base.min.js")), "text/javascript"),
        "/vendor/highlight.min.js" => (Cow::Borrowed(include_bytes!("../ui/vendor/highlight.min.js")), "text/javascript"),
        "/vendor/hljs-languages.min.js" => (Cow::Borrowed(include_bytes!("../ui/vendor/hljs-languages.min.js")), "text/javascript"),
        "/vendor/diff2html.min.css" => (Cow::Borrowed(include_bytes!("../ui/vendor/diff2html.min.css")), "text/css"),
        "/vendor/inter.woff2" => (Cow::Borrowed(include_bytes!("../ui/vendor/inter.woff2")), "font/woff2"),
        "/vendor/jetbrains-mono.woff2" => (Cow::Borrowed(include_bytes!("../ui/vendor/jetbrains-mono.woff2")), "font/woff2"),
        // A spare window has no document yet; the page waits for vjuuLoad().
        "/doc.json" => match slot.lock().expect("slot lock").as_ref() {
            Some(r) => (Cow::Owned(serde_json::to_vec(r).expect("Request serialises")), "application/json"),
            None => return not_found(),
        },
        // Relative images in markdown, as /files/<doc index>/<path>; app.js rewrites them.
        // shortcut: no percent-decoding, so paths with spaces 404.
        _ => {
            let file = path.strip_prefix("/files/").and_then(|p| p.split_once('/')).and_then(|(i, rest)| {
                let base = slot.lock().expect("slot lock").as_ref()?.docs.get(i.parse::<usize>().ok()?)?.base.clone()?;
                Some(base.join(rest))
            });
            match file.and_then(|f| fs::read(f).ok()) {
                Some(bytes) => (Cow::Owned(bytes), if path.ends_with(".svg") { "image/svg+xml" } else { "application/octet-stream" }),
                None => return not_found(),
            }
        }
    };
    Response::builder().header("Content-Type", mime).body(body).expect("static response")
}

fn data_dir() -> PathBuf {
    PathBuf::from(env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA is set on Windows")).join("vjuu")
}

fn reviews_dir() -> PathBuf {
    data_dir().join("reviews")
}

/// Returns the review as markdown after writing it to the reviews directory.
fn save_review(name: &str, review: &Review) -> String {
    let md = review::to_markdown(name, review);
    let dir = reviews_dir();
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).take(60).collect();
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

fn fail(msg: &str) -> ! {
    eprintln!("{}", msg.trim_end());
    process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::{is_diff, kind_of};

    #[test]
    fn file_kinds() {
        for (path, text, kind) in [
            ("src/main.rs", "fn main() {}\n", "code"),
            ("notes.TXT", "plain\n", "code"),
            ("docs/plan.md", "# Plan\n", "md"),
            ("README", "# Title\n", "md"),
            ("change.patch", "", "diff"),
            ("saved.txt", "diff --git a/x b/x\n", "diff"),
        ] {
            assert_eq!(kind_of(path, text), kind, "{path}");
        }
    }

    #[test]
    fn detects_diffs() {
        assert!(is_diff("diff --git a/x b/x\n"));
        assert!(is_diff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n"));
        assert!(!is_diff("# Title\n\n--- \n\nnot a diff\n"));
        assert!(!is_diff("- a list\n- item\n"));
    }
}
