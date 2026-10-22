mod review;

use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use std::{env, fs};

use serde::{Deserialize, Serialize};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;
use wry::http::Response;

use review::Review;

const USAGE: &str = "usage: revu <file.md|file.diff> | revu - | revu --git [git diff args] | revu last";

#[derive(Serialize)]
struct Doc {
    name: String,
    kind: &'static str,
    text: String,
    autoclose: bool,
}

/// Messages posted by ui/app.js via `window.ipc.postMessage`.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Msg {
    State { review: Review },
    Done,
    Rendered,
}

fn main() {
    let started = Instant::now();
    let args: Vec<String> = env::args().skip(1).collect();
    let (doc, base) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["last"] => print_last(),
        ["--git", ref rest @ ..] => git_diff(rest),
        ["-"] => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).unwrap_or_else(|e| fail(&format!("Cannot read stdin: {e}")));
            (doc("stdin".into(), text, false), env::current_dir().ok())
        }
        [path] if !path.starts_with('-') => {
            let text = fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("Cannot read `{path}`: {e}")));
            let ext_diff = matches!(Path::new(path).extension().and_then(|e| e.to_str()), Some("diff" | "patch"));
            (doc(path.into(), text, ext_diff), Path::new(path).canonicalize().ok().and_then(|p| p.parent().map(Path::to_path_buf)))
        }
        _ => {
            eprintln!("{USAGE}");
            process::exit(2)
        }
    };
    run_window(doc, base, started);
}

fn doc(name: String, text: String, force_diff: bool) -> Doc {
    let kind = if force_diff || is_diff(&text) { "diff" } else { "md" };
    Doc { name, kind, text, autoclose: env::var_os("REVU_AUTOCLOSE").is_some() }
}

fn is_diff(text: &str) -> bool {
    text.starts_with("diff ") || (text.starts_with("--- ") || text.contains("\n+++ ")) && text.contains("\n@@ ")
}

fn git_diff(rest: &[&str]) -> (Doc, Option<PathBuf>) {
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
    (doc(name, text, true), env::current_dir().ok())
}

fn run_window(doc: Doc, base: Option<PathBuf>, started: Instant) -> ! {
    let event_loop = EventLoopBuilder::<Msg>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(format!("revu: {}", doc.name))
        .with_inner_size(LogicalSize::new(1100.0, 900.0))
        .build(&event_loop)
        .unwrap_or_else(|e| fail(&format!("Cannot open window: {e}")));
    let proxy = event_loop.create_proxy();
    let name = doc.name.clone();
    let doc_json = serde_json::to_vec(&doc).expect("Doc serialises");
    let _webview = WebViewBuilder::new()
        .with_custom_protocol("revu".into(), move |_id, request| serve(request.uri().path(), &doc_json, base.as_deref()))
        .with_url("revu://localhost/")
        .with_navigation_handler(|url| {
            // WebView2 serves custom protocols as http://<scheme>.localhost.
            if url.starts_with("revu:") || url.starts_with("http://revu.localhost") {
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
                let _ = proxy.send_event(msg);
            }
            Err(e) => eprintln!("Bad message from UI: {e}"),
        })
        .build(&window)
        .unwrap_or_else(|e| fail(&format!("Cannot create webview: {e}")));

    let mut latest = Review::default();
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(Msg::State { review }) => latest = review,
            Event::UserEvent(Msg::Done) | Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                finish(&name, &latest)
            }
            Event::UserEvent(Msg::Rendered) => {
                eprintln!("rendered in {} ms", started.elapsed().as_millis());
                process::exit(0)
            }
            _ => {}
        }
    })
}

fn serve(path: &str, doc_json: &[u8], base: Option<&Path>) -> Response<Cow<'static, [u8]>> {
    let (body, mime): (Cow<'static, [u8]>, &str) = match path {
        "/" => (Cow::Borrowed(include_bytes!("../ui/index.html")), "text/html"),
        "/app.js" => (Cow::Borrowed(include_bytes!("../ui/app.js")), "text/javascript"),
        "/style.css" => (Cow::Borrowed(include_bytes!("../ui/style.css")), "text/css"),
        "/vendor/markdown-it.min.js" => (Cow::Borrowed(include_bytes!("../ui/vendor/markdown-it.min.js")), "text/javascript"),
        "/vendor/diff2html-ui.min.js" => (Cow::Borrowed(include_bytes!("../ui/vendor/diff2html-ui.min.js")), "text/javascript"),
        "/vendor/diff2html.min.css" => (Cow::Borrowed(include_bytes!("../ui/vendor/diff2html.min.css")), "text/css"),
        "/doc.json" => (Cow::Owned(doc_json.to_vec()), "application/json"),
        // Relative images in markdown. shortcut: no percent-decoding, so paths with spaces 404.
        _ => match base.and_then(|b| fs::read(b.join(path.trim_start_matches('/'))).ok()) {
            Some(bytes) => (Cow::Owned(bytes), if path.ends_with(".svg") { "image/svg+xml" } else { "application/octet-stream" }),
            None => return Response::builder().status(404).body(Cow::Borrowed(&[][..])).expect("static response"),
        },
    };
    Response::builder().header("Content-Type", mime).body(body).expect("static response")
}

fn reviews_dir() -> PathBuf {
    PathBuf::from(env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA is set on Windows")).join("revu").join("reviews")
}

fn finish(name: &str, review: &Review) -> ! {
    let md = review::to_markdown(name, review);
    let dir = reviews_dir();
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).take(60).collect();
    let path = dir.join(format!("{secs}-{slug}.md"));
    // Save before printing so a crashed pipe never loses the review.
    match fs::create_dir_all(&dir).and_then(|_| fs::write(&path, &md)) {
        Ok(()) => eprintln!("Saved to {}", path.display()),
        Err(e) => eprintln!("Could not save review to {}: {e}", path.display()),
    }
    print!("{md}");
    process::exit(0)
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
    use super::is_diff;

    #[test]
    fn detects_diffs() {
        assert!(is_diff("diff --git a/x b/x\n"));
        assert!(is_diff("--- a/x\n+++ b/x\n@@ -1 +1 @@\n"));
        assert!(!is_diff("# Title\n\n--- \n\nnot a diff\n"));
        assert!(!is_diff("- a list\n- item\n"));
    }
}
