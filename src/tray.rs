//! The resident instance's tray icon: shows whether reviews are waiting and lists them, brings one forward, opens
//! settings.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::host::TabId;
use tao::event_loop::EventLoopWindowTarget;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the tray asks the event loop to do.
pub enum Action {
    Oldest,
    /// Right-click on Windows: show the menu from vjuu's own window (see `Tray::show_menu`).
    #[cfg(windows)]
    Menu,
    Open(TabId),
    Settings,
    Stop,
}

/// The menu's waiting reviews: which tab each entry opens.
type Entries = Arc<Mutex<Vec<(MenuId, TabId)>>>;

pub struct Tray {
    icon: TrayIcon,
    entries: Entries,
    /// The menu as last built, for `show_menu`.
    #[cfg(windows)]
    menu: Menu,
    /// A hidden window that owns the menu while it is open.
    #[cfg(windows)]
    owner: tao::window::Window,
}

impl Tray {
    /// Shows the icon; `send` runs on whichever thread the platform reports clicks on.
    pub fn new<T>(
        target: &EventLoopWindowTarget<T>,
        send: impl Fn(Action) + Clone + Send + Sync + 'static,
    ) -> Option<Tray> {
        let (menu, _) = menu(&[]).ok()?;
        let icon = TrayIconBuilder::new()
            .with_icon(icon(false))
            .with_tooltip("vjuu")
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_left_click(false)
            // tray-icon's menu window is WS_EX_NOACTIVATE: the menu never gets the keyboard, so its digits do nothing.
            .with_menu_on_right_click(!cfg!(windows))
            .build()
            // No tray on this desktop (a Linux one without a StatusNotifier host): reviews still work.
            .map_err(|e| eprintln!("No tray icon: {e}"))
            .ok()?;
        let click = send.clone();
        TrayIconEvent::set_event_handler(Some(move |event| match event {
            TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
                click(Action::Oldest)
            }
            #[cfg(windows)]
            TrayIconEvent::Click { button: MouseButton::Right, button_state: MouseButtonState::Up, .. } => {
                click(Action::Menu)
            }
            _ => {}
        }));
        let entries = Entries::default();
        let open = entries.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.0.as_str() {
            SETTINGS => send(Action::Settings),
            STOP => send(Action::Stop),
            _ => {
                let window = open.lock().expect("entries lock").iter().find(|(id, _)| *id == event.id).map(|e| e.1);
                if let Some(window) = window {
                    send(Action::Open(window));
                }
            }
        }));
        #[cfg(windows)]
        let owner = tao::window::WindowBuilder::new()
            .with_visible(false)
            .build(target)
            .map_err(|e| eprintln!("No tray menu window: {e}"))
            .ok()?;
        #[cfg(not(windows))]
        let _ = target;
        Some(Tray {
            icon,
            entries,
            #[cfg(windows)]
            menu,
            #[cfg(windows)]
            owner,
        })
    }

    /// Shows the menu at the cursor, owned by a window that takes the keyboard, so a digit picks a review.
    #[cfg(windows)]
    pub fn show_menu(&self) {
        use tao::platform::windows::WindowExtWindows;
        use tray_icon::menu::ContextMenu;
        // SAFETY: `owner` is a live window for as long as `self` is.
        unsafe { self.menu.show_context_menu_for_hwnd(self.owner.hwnd(), None) };
    }

    /// Shows the waiting reviews, oldest first, each with its line for the menu.
    pub fn show_waiting(&mut self, waiting: &[(TabId, String)]) {
        let _ = self.icon.set_icon(Some(icon(!waiting.is_empty())));
        let tip = match waiting.len() {
            0 => "vjuu".to_string(),
            1 => "vjuu: 1 review waiting".to_string(),
            n => format!("vjuu: {n} reviews waiting"),
        };
        let _ = self.icon.set_tooltip(Some(tip));
        if let Ok((menu, entries)) = menu(waiting) {
            *self.entries.lock().expect("entries lock") = entries;
            self.icon.set_menu(Some(Box::new(menu.clone())));
            #[cfg(windows)]
            {
                self.menu = menu;
            }
        }
    }
}

const SETTINGS: &str = "settings";
const STOP: &str = "stop";

/// Returns the tray menu: the waiting reviews, numbered so a digit opens one while the menu is open, then the rest.
fn menu(waiting: &[(TabId, String)]) -> tray_icon::menu::Result<(Menu, Vec<(MenuId, TabId)>)> {
    let menu = Menu::new();
    let mut entries = Vec::new();
    for (n, (window, label)) in waiting.iter().enumerate() {
        let item = MenuItem::new(numbered(n + 1, label), true, None);
        menu.append(&item)?;
        entries.push((item.id().clone(), *window));
    }
    if !waiting.is_empty() {
        menu.append(&PredefinedMenuItem::separator())?;
    }
    menu.append(&MenuItem::with_id(SETTINGS, "Settings", true, None))?;
    menu.append(&MenuItem::with_id(STOP, "Stop vjuu", true, None))?;
    Ok((menu, entries))
}

/// Returns a menu line, its number (1 to 9) the access key; an `&` in the label is shown as itself.
fn numbered(n: usize, label: &str) -> String {
    let label = label.replace('&', "&&");
    if n <= 9 { format!("&{n}  {label}") } else { format!("{n}  {label}") }
}

/// Returns how long a review has waited, as the menu shows it.
pub fn age(waited: Duration) -> String {
    match waited.as_secs() / 60 {
        0 => "just now".into(),
        m @ ..60 => format!("{m} min"),
        m => format!("{} h {} min", m / 60, m % 60),
    }
}

const SIZE: u32 = 32;
const ORANGE: [u8; 4] = [0xfd, 0x97, 0x1f, 0xff];

fn icon(waiting: bool) -> Icon {
    Icon::from_rgba(pixels(waiting), SIZE, SIZE).expect("icon size matches its pixels")
}

/// The bare mark as raw RGBA, exported by assets/icons/export.py; the array type checks its size.
const MARK: &[u8; (SIZE * SIZE * 4) as usize] = include_bytes!("../assets/icons/tray.rgba");

/// Returns the icon as RGBA: the mark, with an orange dot while reviews wait.
fn pixels(waiting: bool) -> Vec<u8> {
    let mut rgba = MARK.to_vec();
    if waiting {
        for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((i as u32 % SIZE) as f32 + 0.5, (i as u32 / SIZE) as f32 + 0.5);
            // Bottom right, the empty corner under the V's right arm.
            if (x - 26.0).powi(2) + (y - 26.0).powi(2) <= 30.0 {
                *pixel = ORANGE;
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn numbers_menu_lines_with_access_keys() {
        assert_eq!(numbered(1, "fix login · plan"), "&1  fix login · plan");
        assert_eq!(numbered(9, "a & b"), "&9  a && b", "a literal ampersand, not an access key");
        assert_eq!(numbered(10, "x"), "10  x", "no digit left for it");
    }

    #[test]
    fn says_how_long_a_review_waited() {
        let min = |m: u64| age(Duration::from_secs(m * 60));
        assert_eq!(age(Duration::from_secs(59)), "just now");
        assert_eq!(min(1), "1 min");
        assert_eq!(min(59), "59 min");
        assert_eq!(min(60), "1 h 0 min");
        assert_eq!(min(125), "2 h 5 min");
    }

    #[test]
    fn adds_the_waiting_dot_to_the_mark() {
        let (idle, waiting) = (pixels(false), pixels(true));
        assert_eq!(at(&idle, 0, 0)[3], 0, "no background");
        assert_eq!(at(&idle, 26, 26)[3], 0, "the dot's corner is empty");
        assert_eq!(at(&waiting, 26, 26), ORANGE);
        assert_eq!(at(&idle, 12, 12), [0xa6, 0xe2, 0x2e, 0xff], "the mark, edge to edge");
        assert_eq!(at(&waiting, 12, 12), at(&idle, 12, 12), "the dot leaves the mark alone");
        assert_eq!(at(&waiting, 20, 21), at(&idle, 20, 21), "nor the tip of its right arm");
    }
}
