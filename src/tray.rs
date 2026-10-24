//! The resident instance's tray icon: shows whether reviews are waiting, brings the oldest forward, opens settings.

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// What the tray asks the event loop to do.
pub enum Action {
    Oldest,
    Settings,
    Stop,
}

pub struct Tray {
    icon: TrayIcon,
}

impl Tray {
    /// Shows the icon; `send` runs on whichever thread the platform reports clicks on.
    pub fn new(send: impl Fn(Action) + Clone + Send + Sync + 'static) -> Option<Tray> {
        let settings = MenuItem::new("Settings", true, None);
        let stop = MenuItem::new("Stop vjuu", true, None);
        let menu = Menu::new();
        menu.append(&settings).and_then(|()| menu.append(&stop)).ok()?;
        let icon = TrayIconBuilder::new()
            .with_icon(icon(false))
            .with_tooltip("vjuu")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            // No tray on this desktop (a Linux one without a StatusNotifier host): reviews still work.
            .map_err(|e| eprintln!("No tray icon: {e}"))
            .ok()?;
        let click = send.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                click(Action::Oldest);
            }
        }));
        let (settings, stop) = (settings.id().clone(), stop.id().clone());
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == settings {
                send(Action::Settings);
            } else if event.id == stop {
                send(Action::Stop);
            }
        }));
        Some(Tray { icon })
    }

    pub fn show_waiting(&self, waiting: usize) {
        let _ = self.icon.set_icon(Some(icon(waiting > 0)));
        let tip = match waiting {
            0 => "vjuu".to_string(),
            1 => "vjuu: 1 review waiting".to_string(),
            n => format!("vjuu: {n} reviews waiting"),
        };
        let _ = self.icon.set_tooltip(Some(tip));
    }
}

const SIZE: u32 = 32;
const ORANGE: [u8; 4] = [0xfd, 0x97, 0x1f, 0xff];

fn icon(waiting: bool) -> Icon {
    Icon::from_rgba(pixels(waiting), SIZE, SIZE).expect("icon size matches its pixels")
}

/// The brand tile as raw RGBA, exported by assets/icons/export.py; the array type checks its size.
const TILE: &[u8; (SIZE * SIZE * 4) as usize] = include_bytes!("../assets/icons/tray.rgba");

/// Returns the icon as RGBA: the brand tile, with an orange dot while reviews wait.
fn pixels(waiting: bool) -> Vec<u8> {
    let mut rgba = TILE.to_vec();
    if waiting {
        for (i, pixel) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = ((i as u32 % SIZE) as f32 + 0.5, (i as u32 / SIZE) as f32 + 0.5);
            if (x - 25.0).powi(2) + (y - 7.0).powi(2) <= 30.0 {
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
    fn adds_the_waiting_dot_to_the_tile() {
        let (idle, waiting) = (pixels(false), pixels(true));
        assert_eq!(at(&idle, 0, 0)[3], 0, "corners are rounded off");
        assert_eq!(at(&idle, 25, 7), [0x27, 0x28, 0x22, 0xff], "tile background");
        assert_eq!(at(&waiting, 25, 7), ORANGE);
        assert_eq!(at(&waiting, 16, 20), at(&idle, 16, 20), "the dot leaves the mark alone");
    }
}
