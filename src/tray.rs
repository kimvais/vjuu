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
const DARK: [u8; 4] = [0x27, 0x28, 0x22, 0xff];
const GREEN: [u8; 4] = [0xa6, 0xe2, 0x2e, 0xff];
const ORANGE: [u8; 4] = [0xfd, 0x97, 0x1f, 0xff];

fn icon(waiting: bool) -> Icon {
    Icon::from_rgba(pixels(waiting), SIZE, SIZE).expect("icon size matches its pixels")
}

/// Returns the icon as RGBA: a green "v" on the dark title bar colour, with an orange dot while reviews wait.
/// Drawn here rather than shipped as an image, so there is no asset to keep in step.
fn pixels(waiting: bool) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
            let color = if waiting && (x - 25.0).powi(2) + (y - 7.0).powi(2) <= 30.0 {
                ORANGE
            } else if near_segment((x, y), (9.0, 10.0), (16.0, 24.0))
                || near_segment((x, y), (16.0, 24.0), (23.0, 10.0))
            {
                GREEN
            } else if in_rounded_square(x, y) {
                DARK
            } else {
                [0; 4]
            };
            rgba.extend(color);
        }
    }
    rgba
}

fn near_segment((px, py): (f32, f32), (ax, ay): (f32, f32), (bx, by): (f32, f32)) -> bool {
    let (dx, dy) = (bx - ax, by - ay);
    let t = (((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
    (px - ax - t * dx).powi(2) + (py - ay - t * dy).powi(2) <= 2.2_f32.powi(2)
}

fn in_rounded_square(x: f32, y: f32) -> bool {
    const RADIUS: f32 = 6.0;
    let edge = SIZE as f32;
    // Distance past the inner square that the corners round off.
    let dx = (RADIUS - x).max(x - (edge - RADIUS)).max(0.0);
    let dy = (RADIUS - y).max(y - (edge - RADIUS)).max(0.0);
    dx * dx + dy * dy <= RADIUS * RADIUS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rgba: &[u8], x: u32, y: u32) -> [u8; 4] {
        let i = ((y * SIZE + x) * 4) as usize;
        rgba[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn draws_the_v_and_the_waiting_dot() {
        let (idle, waiting) = (pixels(false), pixels(true));
        assert_eq!(idle.len(), (SIZE * SIZE * 4) as usize);
        assert_eq!(at(&idle, 0, 0), [0; 4], "corners are rounded off");
        assert_eq!(at(&idle, 16, 23), GREEN, "bottom of the v");
        assert_eq!(at(&idle, 16, 12), DARK, "inside the v");
        assert_eq!(at(&idle, 25, 7), DARK);
        assert_eq!(at(&waiting, 25, 7), ORANGE);
    }
}
