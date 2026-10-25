//! vjuu's one window: every review is a tab in it, each tab a webview of its own, one shown at a time.

use std::time::Instant;

use serde::Serialize;
use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use wry::{WebContext, WebView, WebViewBuilder};

use crate::platform::{self, bring_to_front};
use crate::{AppEvent, Msg, Reply, Request, Review, Slot, fail, is_vjuu, serve, tray};

pub type TabId = u64;

pub struct Tab {
    // Declared before `_context` so it is dropped first.
    pub webview: WebView,
    /// This tab's own web context where the platform needs one (see `platform::CONTEXT_PER_WEBVIEW`).
    _context: Option<WebContext>,
    pub slot: Slot,
    pub reply: Option<Reply>,
    pub latest: Review,
    pub started: Instant,
    /// The git context, kept to hand to the page again if it arrived before the page could take it.
    pub context: Option<String>,
    /// The settings panel's Apply saved, so a settings-only tab can say so when it closes.
    pub applied: bool,
    /// Opened behind (the `focus` setting off, or while another tab was in use), so it never comes to the front by
    /// itself.
    pub background: bool,
    /// Shown at least once; until then the tab strip marks it.
    seen: bool,
}

impl Tab {
    /// Returns "fix login · plan · vjuu · main · 4 min": the asking session, what it asks (or the documents), the git
    /// checkout and how long it has waited; whichever are known.
    pub fn label(&self) -> String {
        let slot = self.slot.lock().expect("slot lock");
        let (session, what) =
            slot.as_ref().map(|r| (r.session.clone(), r.kind.clone().unwrap_or_else(|| r.name()))).unzip();
        [session.flatten(), what, self.context.clone(), Some(tray::age(self.started.elapsed()))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Returns the window title while this tab is shown.
    pub fn title(&self) -> String {
        let title = self.slot.lock().expect("slot lock").as_ref().map(Request::title).unwrap_or_default();
        match &self.context {
            Some(git) => format!("{title} · {git}"),
            None => title,
        }
    }
}

/// One tab as the page's tab strip draws it.
#[derive(Serialize)]
struct StripTab {
    id: TabId,
    /// The session, or the documents when there is none.
    name: String,
    kind: Option<String>,
    /// The tooltip: everything the tray menu says about it.
    title: String,
    active: bool,
    unseen: bool,
}

pub struct Host {
    // Declared before `window` so the webviews are dropped first.
    tabs: Vec<(TabId, Tab)>,
    /// A hidden webview with the page already loaded, so the next review shows at once.
    spare: Option<(TabId, Tab)>,
    active: Option<TabId>,
    next: TabId,
    /// Sessions in the order they first opened a tab: a session's next review comes back to where its last one was.
    sessions: Vec<String>,
    pub window: Window,
}

impl Host {
    /// Opens the window, hidden until it has a tab.
    pub fn new(target: &EventLoopWindowTarget<AppEvent>) -> Host {
        let builder = WindowBuilder::new()
            .with_title("vjuu")
            .with_visible(false)
            .with_focused(false)
            .with_inner_size(LogicalSize::new(1100.0, 900.0));
        let window =
            platform::frame(builder).build(target).unwrap_or_else(|e| fail(&format!("Cannot open window: {e}")));
        platform::style_frame(&window);
        platform::fit_height(&window);
        Host { tabs: Vec::new(), spare: None, active: None, next: 0, sessions: Vec::new(), window }
    }

    /// Loads the page into a hidden webview for the next review, unless one is waiting already.
    pub fn warm_spare(&mut self, proxy: &EventLoopProxy<AppEvent>, context: &mut WebContext) {
        if self.spare.is_none() {
            self.spare = Some(self.new_tab(proxy, context, None, false));
        }
    }

    /// Shows `request` in a tab of its own: in front when the window was empty, or when the user is elsewhere and
    /// `focus` is on; otherwise behind the tab in use. Returns the tab.
    pub fn open(
        &mut self,
        proxy: &EventLoopProxy<AppEvent>,
        context: &mut WebContext,
        request: Request,
        reply: Option<Reply>,
        started: Instant,
        focus: bool,
    ) -> TabId {
        let rank = self.rank(request.session.as_deref());
        let (id, mut tab) = match self.spare.take() {
            Some((id, tab)) => {
                *tab.slot.lock().expect("slot lock") = Some(request);
                let _ = tab.webview.evaluate_script("window.vjuuLoad && vjuuLoad()");
                (id, tab)
            }
            None => self.new_tab(proxy, context, Some(request), focus),
        };
        tab.reply = reply;
        tab.started = started;
        let here = self.window.is_visible() && self.window.is_focused();
        let front = self.active.is_none() || (focus && !here);
        tab.background = !(front && focus);
        let ranks: Vec<usize> = self.tabs.iter().map(|(_, t)| self.rank_of(t)).collect();
        self.tabs.insert(position(&ranks, rank), (id, tab));
        if front {
            self.show(id);
        }
        if !self.window.is_visible() {
            if focus {
                self.window.set_visible(true);
            } else {
                // Unfocusable while it appears, so Windows does not activate it; clickable again right after.
                self.window.set_focusable(false);
                self.window.set_visible(true);
                self.window.set_focusable(true);
            }
        }
        if front && focus {
            self.raise(id);
        }
        self.show_strip();
        id
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|(t, _)| *t == id).map(|(_, tab)| tab)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|(t, _)| *t == id).map(|(_, tab)| tab)
    }

    pub fn is_active(&self, id: TabId) -> bool {
        self.active == Some(id)
    }

    /// Returns the tabs in strip order.
    pub fn tabs(&self) -> impl Iterator<Item = &(TabId, Tab)> {
        self.tabs.iter()
    }

    /// Returns every webview, the spare's too.
    pub fn webviews(&self) -> impl Iterator<Item = &WebView> {
        self.tabs.iter().chain(&self.spare).map(|(_, tab)| &tab.webview)
    }

    /// Shows tab `id` and hides the others.
    pub fn show(&mut self, id: TabId) {
        if self.get(id).is_none() {
            return;
        }
        self.active = Some(id);
        for (t, tab) in &mut self.tabs {
            let _ = tab.webview.set_visible(*t == id);
            if *t == id {
                tab.seen = true;
                self.window.set_title(&tab.title());
                let _ = tab.webview.focus();
            }
        }
        self.show_strip();
    }

    /// Shows tab `id` and brings the window to the front with it.
    pub fn raise(&mut self, id: TabId) {
        self.show(id);
        if let Some(tab) = self.get(id) {
            self.window.set_minimized(false);
            bring_to_front(&self.window, &tab.webview);
        }
    }

    /// Takes tab `id` out; the one next to it is shown instead, and the window hides once the last is gone.
    pub fn remove(&mut self, id: TabId) -> Option<Tab> {
        let at = self.tabs.iter().position(|(t, _)| *t == id)?;
        let (_, tab) = self.tabs.remove(at);
        if self.active == Some(id) {
            self.active = None;
            match self.tabs.get(at).or(self.tabs.last()).map(|(t, _)| *t) {
                Some(next) => self.show(next),
                None => self.window.set_visible(false),
            }
        }
        self.show_strip();
        Some(tab)
    }

    /// Sizes every webview to the window, after it was resized.
    pub fn fit(&self) {
        for webview in self.webviews() {
            platform::fit_webview(webview, &self.window);
        }
    }

    /// Hands every tab's page the tab strip.
    pub fn show_strip(&self) {
        for (_, tab) in &self.tabs {
            self.show_strip_in(tab);
        }
    }

    /// Hands one tab's page the tab strip, e.g. once it has loaded.
    pub fn show_strip_in(&self, tab: &Tab) {
        let strip: Vec<StripTab> = self
            .tabs
            .iter()
            .map(|(id, t)| {
                // Before taking the slot's lock: label() takes it too.
                let title = t.label();
                let request = t.slot.lock().expect("slot lock");
                StripTab {
                    id: *id,
                    name: request.as_ref().map(|r| r.session.clone().unwrap_or_else(|| r.name())).unwrap_or_default(),
                    kind: request.as_ref().and_then(|r| r.kind.clone()),
                    title,
                    active: self.active == Some(*id),
                    unseen: !t.seen,
                }
            })
            .collect();
        let json = serde_json::to_string(&strip).expect("strip serialises");
        let _ = tab.webview.evaluate_script(&format!("window.vjuuTabs && vjuuTabs({json})"));
    }

    /// Returns where a session's tabs go in the strip: in the order the sessions first opened one; tabs without a
    /// session last.
    fn rank(&mut self, session: Option<&str>) -> usize {
        let Some(session) = session else { return usize::MAX };
        match self.sessions.iter().position(|s| s == session) {
            Some(rank) => rank,
            None => {
                self.sessions.push(session.to_string());
                self.sessions.len() - 1
            }
        }
    }

    fn rank_of(&self, tab: &Tab) -> usize {
        let request = tab.slot.lock().expect("slot lock");
        let session = request.as_ref().and_then(|r| r.session.as_deref());
        session.and_then(|s| self.sessions.iter().position(|known| known == s)).unwrap_or(usize::MAX)
    }

    /// Creates a tab's webview in the window, hidden; without a request its page waits until one is handed over.
    fn new_tab(
        &mut self,
        proxy: &EventLoopProxy<AppEvent>,
        context: &mut WebContext,
        request: Option<Request>,
        focus: bool,
    ) -> (TabId, Tab) {
        let id = self.next;
        self.next += 1;
        let slot: Slot = std::sync::Arc::new(std::sync::Mutex::new(request));
        let proxy = proxy.clone();
        let serve_slot = slot.clone();
        let mut own =
            platform::CONTEXT_PER_WEBVIEW.then(|| WebContext::new(Some(platform::data_dir().join("webview"))));
        let context = own.as_mut().unwrap_or(context);
        let builder = WebViewBuilder::new_with_web_context(context)
            .with_visible(false)
            // A spare is built while the user is reading another tab: it must not take their keys.
            .with_focused(focus)
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
                // Only vjuu's own page: an HTML page under review runs its scripts in a frame, and must not be able
                // to finish the review or fill it in, should the platform hand frames the message channel.
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
        let webview = platform::build_webview(builder, &self.window)
            .unwrap_or_else(|e| fail(&format!("Cannot create webview: {e}")));
        let tab = Tab {
            webview,
            _context: own,
            slot,
            reply: None,
            latest: Review::default(),
            started: Instant::now(),
            context: None,
            applied: false,
            background: false,
            seen: false,
        };
        (id, tab)
    }
}

/// Returns where a tab of `rank` goes among tabs of `ranks` (sorted): after its own session's and earlier ones'.
fn position(ranks: &[usize], rank: usize) -> usize {
    ranks.iter().take_while(|&&r| r <= rank).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_a_tab_next_to_its_session() {
        assert_eq!(position(&[], usize::MAX), 0);
        assert_eq!(position(&[0, 0, 1, usize::MAX], 0), 2, "after the session's other tabs");
        assert_eq!(position(&[0, 2], 1), 1, "between the sessions that came before and after");
        assert_eq!(position(&[0, usize::MAX], usize::MAX), 2, "no session: last");
        assert_eq!(position(&[1, usize::MAX], 0), 0, "an earlier session goes first");
    }
}
