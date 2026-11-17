# Focus modes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A three-value `focus` setting (`front`, `behind`, `hidden`), clickable Windows notifications that open their
review, and a `desktop` setting (`follow`, `switch`) that moves vjuu to the virtual desktop in use when it comes forward.

**Architecture:** `config.rs` gains two enums; `Host` decides placement through one pure function and moves the
window to the current desktop in `raise`; a Windows-only toast in `notify` routes its click back as the tray's
`Open(id)` event. The settings panel shows both settings as selects.

**Tech Stack:** Rust (tao, wry, serde/toml), `tauri-winrt-notification` 0.8 (already in Cargo.lock), `windows` 0.62
(already in Cargo.lock) for `IVirtualDesktopManager`, `windows-sys` 0.61 for the registry; JS UI tested with Playwright.

**Spec:** `docs/superpowers/specs/2026-11-17-focus-modes-design.md`

## Global Constraints

- `focus` values: `front` (default), `behind`, `hidden`. `desktop` values: `follow` (default), `switch`.
- Old files keep loading: `focus = true` reads as `front`, `focus = false` as `behind`.
- `hidden` with the tray off acts as `behind`.
- Notification click and desktop following are Windows only; elsewhere both settings change nothing beyond `hidden` /
  `behind` placement, and the settings panel says so.
- Repo rule: new functionality has a keyboard shortcut in README's Keys table, F1 help (`#help`) and the control's
  tooltip. The new controls live in the settings panel, opened with Ctrl+, and driven with Tab / arrows / Ctrl+Enter;
  their `title` says so.
- Errors in config: `Unknown value ...` messages in the existing style of `Config::set`.

## Review Focus

- `hidden` while the window is already visible with other tabs: the new review is a background tab, the window does
  not move or take focus, a notification still shows. Pinned in Task 2's `placement` tests.
- `hidden` while the window is minimised: it stays minimised; a click restores it (`raise` already unminimises).
  Pinned in Task 2.
- Toast clicked after its review was closed: nothing happens, no panic (`Host::raise` on a missing id is a no-op).
  Pinned in Task 4's manual check.
- Settings Apply from a panel that never touched the selects keeps `focus` / `desktop` unchanged (no `"on"` strings
  leaking from the old checkbox). Pinned in Task 5's `test_settings`.
- A foreground window with no desktop (the taskbar, the desktop itself): following fails quietly and the window
  comes forward where it is. Pinned in Task 3's error handling (`GetWindowDesktopId` error → skip the move).

---

### Task 1: `Focus` and `Desktop` settings

**Files:**
- Modify: `src/config.rs`

**Interfaces:**
- Produces:
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")] pub enum Focus { Front, Behind, Hidden }`
    with a `Deserialize` that also takes a bool (`true` → `Front`, `false` → `Behind`); name the bridge `CompatFocus`
    (an untagged enum `{ Bool(bool), Mode(Focus) }` with `#[serde(from)]` style conversion is fine).
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "lowercase")] pub enum Desktop { Follow, Switch }`
  - `Config { focus: Focus, desktop: Desktop, .. }`, defaults `Focus::Front`, `Desktop::Follow`; `desktop` serialised
    right after `focus`.
  - `impl Config { pub fn placement_focus(&self) -> Focus }`: `Hidden` becomes `Behind` when `!self.tray`.

- [ ] **Step 1: Write the failing tests** in `config.rs`'s test module:
  - `missing_keys_stay_on`: expected struct gets `focus: Focus::Front, desktop: Desktop::Follow`.
  - `round_trips_with_a_header`: add `config.set("focus", "hidden")` and `config.set("desktop", "switch")`; expected
    text has `focus = "hidden"\ndesktop = "switch"\n` in place of `focus = true\n`.
  - `reads_old_focus_bools`: `toml::from_str::<Config>("focus = false\n").unwrap().focus == Focus::Behind`, and
    `"focus = true\n"` gives `Front`.
  - `hidden_needs_the_tray`: with `focus = Hidden, tray = false`, `placement_focus()` is `Behind`; with tray on it is
    `Hidden`.
  - `rejects_unknown_settings_and_values`: add `("focus", "on")`, `("focus", "yes")`, `("desktop", "here")`.
- [ ] **Step 2:** `cargo test config` → the new and changed tests fail to compile or fail.
- [ ] **Step 3: Implement.** `set` accepts `focus` = `front|behind|hidden` and `desktop` = `follow|switch`, else
  ``Unknown value `{value}`: use front, behind or hidden.`` / ``...: use follow or switch.``; add `desktop` to the
  unknown-setting message's list. Update the usage text at `src/main.rs:47` so `focus` is listed with its three values
  and `desktop` with its two.
- [ ] **Step 4:** `cargo test config` → pass. `cargo build` fails where `main.rs` / `host.rs` use `focus` as a bool:
  fix those by passing `config.placement_focus()` and comparing with `Focus::Front` until Task 2 replaces them.
- [ ] **Step 5:** `cargo test` → pass. Commit `Make focus three-way and add the desktop setting`.

### Task 2: Placement of a new review

**Files:**
- Modify: `src/host.rs` (`open` 107-152, `raise` 193-200), `src/main.rs` (1054, 1058, 1100, 1125)

**Interfaces:**
- Consumes: `config::Focus` from Task 1.
- Produces:
  - `pub struct Placement { pub front: bool, pub show_window: bool, pub take_focus: bool }` and
    `pub fn placement(focus: Focus, empty: bool, visible: bool, here: bool) -> Placement` in `host.rs`
    (`empty`: no active tab; `visible`: window visible and not minimised; `here`: window visible and focused).
  - `Host::open(..., focus: Focus)` replacing the bool.
  - `Host::raise(id)` also makes a hidden window visible.

Rules (`front` = the new tab becomes the active one; `show_window` = make a hidden window visible now;
`take_focus` = raise it):

| focus | front | show_window | take_focus |
|---|---|---|---|
| Front | `empty \|\| !here` | true | `front` |
| Behind | `empty` | true (unfocusable while appearing, as today) | false |
| Hidden | `empty` | false | false |

`tab.background = !take_focus`. A minimised window is never restored by `open` unless `take_focus`.

- [ ] **Step 1: Write the failing test** `placement_rules` in `host.rs` (`#[cfg(test)] mod tests`): loop the table
  above over every `(empty, visible, here)` with `visible == false ⇒ here == false`, asserting whole `Placement`
  values. Include the Review Focus cases: `Hidden, empty=false, visible=true, here=true` →
  `Placement { front: false, show_window: false, take_focus: false }`; `Hidden, empty=true, visible=false` →
  `front: true, show_window: false`.
- [ ] **Step 2:** `cargo test placement` → fails (not defined).
- [ ] **Step 3: Implement** `placement` and use it in `open`; in `raise`, call `self.window.set_visible(true)` before
  unminimising. `main.rs` keeps `let mut focus = config::load().placement_focus();` and updates it on Apply.
- [ ] **Step 4:** `cargo test` → pass. Manual: `vjuu config focus hidden`, restart the resident instance
  (`vjuu start`), run `vjuu README.md` → no window, a notification and the tray dot; click the tray icon → the review
  shows. `vjuu config focus behind` → today's off behaviour.
- [ ] **Step 5:** Commit `Open a review hidden when focus is hidden`.

### Task 3: Follow the current desktop

**Files:**
- Modify: `src/platform.rs` (new fn next to `bring_to_front`), `src/host.rs` (`raise`, a `desktop` field),
  `src/main.rs` (set it on start and on Apply), `Cargo.toml` (windows target deps)

**Interfaces:**
- Consumes: `config::Desktop` from Task 1; `Host::raise` from Task 2.
- Produces: `pub fn move_to_current_desktop(window: &Window)` in `platform.rs` (no-op off Windows);
  `Host { pub desktop: Desktop, .. }`.

- [ ] **Step 1:** Add `windows = { version = "0.62", features = ["Win32_UI_Shell", "Win32_System_Com"] }` under
  `[target.'cfg(windows)'.dependencies]` (comment: already in the tree through wry/tao).
- [ ] **Step 2: Implement** `move_to_current_desktop`: `CoCreateInstance(&VirtualDesktopManager, None,
  CLSCTX_ALL)` → `IVirtualDesktopManager`; if `IsWindowOnCurrentVirtualDesktop(hwnd)` is true, return; else
  `GetWindowDesktopId(GetForegroundWindow())` and `MoveWindowToDesktop(hwnd, &id)`. Any error → return quietly (the
  window then comes forward on its own desktop, today's behaviour). The event loop thread already has COM initialised
  by WebView2; do not call `CoInitializeEx`. Comment why: Windows' own switch-to-its-desktop is what `switch` keeps.
- [ ] **Step 3:** In `Host::raise`, call it before `bring_to_front` when `self.desktop == Desktop::Follow`.
- [ ] **Step 4:** `cargo build`, `cargo clippy --all-targets -- -D warnings` → clean. Manual check (no automated test
  can drive virtual desktops): open a review, move to desktop 2 (Win+Ctrl+→), click the tray icon → the window
  appears on desktop 2. `vjuu config desktop switch` → the same click switches to desktop 1.
- [ ] **Step 5:** Commit `Bring vjuu to the desktop in use`.

### Task 4: Clickable notification on Windows

**Files:**
- Modify: `src/main.rs` (`notify` 1272-1279, its call at 1162, resident start), `src/platform.rs` (app id
  registration), `Cargo.toml`

**Interfaces:**
- Consumes: `tray::Action::Open(TabId)` and `AppEvent::Tray` (existing); `Host::raise` from Tasks 2-3.
- Produces: `fn notify(proxy: &EventLoopProxy<AppEvent>, id: TabId, title: String)`;
  `pub const APP_ID: &str = "vjuu"` and `pub fn register_app_id()` in `platform.rs` (no-op off Windows).

- [ ] **Step 1:** Add `tauri-winrt-notification = "0.8.1"` under the windows target deps (comment: already in the
  tree through notify-rust); add `Win32_System_Registry` to `windows-sys` features.
- [ ] **Step 2: Implement** `register_app_id`: write `HKCU\Software\Classes\AppUserModelId\vjuu` values `DisplayName
  = "vjuu"` and `IconUri` = `config_dir()/vjuu.png`, writing `assets/icons/vjuu-512.png` there (`include_bytes!`)
  when missing. Errors are logged with `eprintln!` and ignored: the toast then shows under PowerShell's name, as
  today. Call it once when the resident instance starts, before the tray.
- [ ] **Step 3: Implement** `notify` on Windows: `Toast::new(APP_ID).title("vjuu is waiting for you").text1(body)
  .on_activated(move |_| { proxy.send_event(AppEvent::Tray(tray::Action::Open(id))); Ok(()) }).show()` on a thread.
  Off Windows keep the notify-rust body. Fix the stale comment at 1273.
- [ ] **Step 4:** `cargo clippy --all-targets -- -D warnings` → clean; `cargo test` → pass. Manual: `focus hidden`,
  open two reviews, click the first toast → that review's tab shows, on the current desktop. Close a review, click
  its toast in the Action Center → nothing happens, vjuu keeps running. The toast names vjuu with its icon.
- [ ] **Step 5:** Commit `Open the review when its notification is clicked`.

### Task 5: Settings panel and docs

**Files:**
- Modify: `ui/index.html` (73-76), `ui/app.js` (94-141), `tests/ui_test.py` (`_CONFIG` 19-24, `test_settings`
  909-945), `README.md` (121-132, 271, Keys table)

**Interfaces:**
- Consumes: the JSON `/config.json` now has `focus: "front"|"behind"|"hidden"` and `desktop: "follow"|"switch"`.
- Produces: `<select name="focus">` and `<select name="desktop">` in `#settings`.

- [ ] **Step 1: Write the failing test.** `_CONFIG['focus'] = 'behind'`, add `'desktop': 'follow'`. In
  `test_settings`: drop `focus` from the checkbox list (`checked()` becomes 6 values); assert
  `page.input_value('#settings [name=focus]') == 'behind'` and `desktop` is `'follow'`; select `hidden` and `switch`
  with the keyboard (focus the select, `ArrowDown`), Apply with Ctrl+Enter, and assert the whole posted `config` equals
  `_CONFIG` with `turn` changed and `focus: 'hidden', desktop: 'switch'`. Also assert an Apply without touching them
  posts `focus: 'behind', desktop: 'follow'` unchanged.
- [ ] **Step 2:** `uv run --no-project --with playwright python tests/ui_test.py` → `test_settings` fails.
- [ ] **Step 3: Implement.** Replace the focus checkbox with a labelled select (options "Bring to the front",
  "Open behind, with a notification", "Only a notification and the tray dot"), and add a "When vjuu comes forward"
  select ("Move it to the desktop I am on", "Switch to its desktop"). Each has a `title` naming Ctrl+, and noting the
  desktop and click behaviours are Windows only. `openSettings` / `applySettings` read and post the select values.
- [ ] **Step 4:** UI tests → all pass.
- [ ] **Step 5: Docs.** README 121-132: describe the three focus values, the clickable notification and `desktop`;
  line 271: `vjuu config focus hidden` and `vjuu config desktop switch` examples. Keys table and F1 help: no new keys;
  check the settings row there mentions Ctrl+, opens these settings.
- [ ] **Step 6:** `cargo test`, `cargo clippy --all-targets -- -D warnings`, UI tests → pass. Commit
  `Show focus and desktop in the settings panel`.
