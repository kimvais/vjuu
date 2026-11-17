# Focus modes, clickable notifications, follow the desktop

## Why

A new review grabbing the screen interrupts whatever Kimmo is doing. He wants to be told (notification, tray dot)
and come to the review when he chooses, on the desktop he is on.

## Goals

- `focus` takes three values, set in the settings view and with `vjuu config focus <value>`:

  | `focus` | A new review |
  |---|---|
  | `front` | comes to the front (today's `on`, the default) |
  | `behind` | opens behind the window in use, with a notification (today's `off`) |
  | `hidden` | no window: only the notification and the tray dot. In an already visible window it is a background tab |

- Clicking the notification opens that review, like picking it in the tray menu.
- A new `desktop` setting, `follow` (default) or `switch`: when vjuu comes to the front (notification, tray, `front`
  mode), `follow` moves its window to the virtual desktop in use; `switch` keeps today's behaviour, where Windows
  switches to vjuu's desktop.

## Non-goals

- Clickable notifications or desktop following on macOS and Linux: they keep today's behaviour; the settings have no
  effect there, and the settings view says so.
- A global hotkey to summon vjuu.

## Anti-goals

- A review that nothing points at: `hidden` with the tray off would leave no way to find the review, so it acts as
  `behind` then.

## Constraints

- Old config files keep loading: `focus = true` / `false` read as `front` / `behind`; a TOML string otherwise.
- Repo rule: new functionality has a keyboard shortcut, listed in README's Keys table, F1 help and the control's
  tooltip. The settings view's new controls follow its existing keyboard handling.

## Approaches

1. **`tauri-winrt-notification` directly** (picked): already in the tree through notify-rust; its `on_activated`
   callback sends an event to the loop to raise the tab. Needs an app id registered under
   `HKCU\Software\Classes\AppUserModelId\<id>` (display name, icon) when the resident instance starts, so the toast
   says vjuu, not PowerShell. Small change.
2. Hand-rolled WinRT through the `windows` crate: more control (buttons, replacing a toast when its tab closes) for
   noticeably more code. Not needed for a click.
3. Also Linux's notification default action through notify-rust: a waiting thread per notification; nobody needs it
   yet.

## Decided

- Three-value `focus`, `desktop` follow/switch, notification click raises that review's tab, Windows only for both.
- Desktop following uses the public `IVirtualDesktopManager`: the desktop of the foreground window, then
  `MoveWindowToDesktop` for vjuu's window, before `bring_to_front`.

## Left to the builder

- The app id string and where the icon file for the toast lives.
- What a click on a toast whose review has closed does (nothing, or the oldest waiting review).
- How the settings view shows the two picks (radio group or select).
