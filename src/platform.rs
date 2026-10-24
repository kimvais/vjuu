//! Everything that differs between Windows, macOS and Linux.
//!
//! Windows gets a frameless window with a Windows Terminal style title bar drawn by ui/, plus workarounds for its
//! focus rules. macOS and Linux keep the native window frame.

use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use interprocess::local_socket::{GenericFilePath, GenericNamespaced, Name, prelude::*};
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

/// WebKitGTK allows one handler per URI scheme per web context, so every Linux window needs its own context;
/// WebView2 and WKWebView can share one.
pub const CONTEXT_PER_WINDOW: bool = cfg!(all(unix, not(target_os = "macos")));

/// Whether ui/ draws the title bar and resize edges (Windows) or the OS frame is kept.
pub const FRAMELESS: bool = cfg!(windows);

/// Returns where vjuu keeps saved reviews and the webview profile.
pub fn data_dir() -> PathBuf {
    #[cfg(windows)]
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));
    base.unwrap_or_else(env::temp_dir).join("vjuu")
}

/// Returns where vjuu keeps its settings: the platform's usual (on Windows, roaming) config folder.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    base.unwrap_or_else(env::temp_dir).join("vjuu")
}

/// Returns the per-user socket the resident instance listens on.
pub fn pipe_name() -> Name<'static> {
    // shortcut: default pipe/socket permissions; fine on a single-user machine, restrict them if shared.
    let user = env::var("USERNAME").or_else(|_| env::var("USER")).unwrap_or_default();
    let name = format!("vjuu-{user}.sock");
    // Windows named pipes and Linux abstract sockets have a namespace; macOS needs a socket file.
    let name = if GenericNamespaced::is_supported() {
        name.to_ns_name::<GenericNamespaced>()
    } else {
        env::temp_dir().join(name).to_fs_name::<GenericFilePath>()
    };
    name.expect("valid socket name")
}

/// Whether a socket left behind by a crashed instance has to be replaced (socket files only).
pub fn socket_may_be_stale() -> bool {
    !GenericNamespaced::is_supported()
}

/// Stops every child from inheriting our stdio handles; children get their own through `Stdio`.
/// CreateProcess inherits every inheritable handle, not just the three it is given. A hook's stdout pipe would
/// pass through its `vjuu` child into the resident instance that child starts, and Claude Code, waiting for EOF,
/// would get it only when the resident instance exits.
pub fn keep_stdio_private() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        for std_handle in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            unsafe { SetHandleInformation(GetStdHandle(std_handle), HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

/// Whether vjuu shares no console with anyone: double-clicked (Windows made it a console of its own) or started
/// with none, as winget's validator may. Run from a terminal, the shell shares it. Never elsewhere: Finder and file
/// managers open a terminal.
pub fn own_console() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::GetConsoleProcessList;
        let mut ids = [0u32; 2];
        unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) <= 1 }
    }
    #[cfg(not(windows))]
    false
}

/// Starts `command` detached from the caller: no shared stdio, and not killed with the caller's terminal.
pub fn spawn_detached(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x8;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group, so Ctrl+C in the caller's terminal doesn't take the resident instance with it.
        command.process_group(0);
    }
    let _ = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

/// Opens an http(s) link in the default browser.
pub fn open_url(url: &str) {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = Command::new(opener).arg(url).spawn();
}

/// Makes the window frameless on Windows, where ui/ draws the title bar, and gives it the exe's icon.
pub fn frame(builder: WindowBuilder) -> WindowBuilder {
    #[cfg(windows)]
    {
        use tao::platform::windows::{IconExtWindows, WindowBuilderExtWindows};
        use tao::window::Icon;
        // Ordinal 1 is vjuu.ico, embedded by build.rs.
        let icon = || Icon::from_resource(1, None).ok();
        builder
            .with_decorations(false)
            .with_undecorated_shadow(true)
            .with_window_icon(icon())
            .with_taskbar_icon(icon())
    }
    #[cfg(not(windows))]
    builder
}

/// Builds the webview into the window; on Linux it goes into tao's GTK box so Wayland works too.
pub fn build_webview(builder: WebViewBuilder, window: &Window) -> wry::Result<WebView> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().expect("tao windows have a default GTK box"))
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    builder.build(window)
}

/// Gives the window the full height of its screen, keeping its width, centred horizontally.
///
/// Windows uses the monitor's work area, so the taskbar stays visible. macOS and Linux don't expose one through
/// tao, so 92% of the screen height stands in for it (menu bar, dock, panels).
pub fn fit_height(window: &Window) {
    use tao::dpi::{PhysicalPosition, PhysicalSize};
    let width = window.outer_size().width;
    #[cfg(windows)]
    let area = {
        use tao::platform::windows::WindowExtWindows;
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
        };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..unsafe { std::mem::zeroed() } };
        let ok =
            unsafe { GetMonitorInfoW(MonitorFromWindow(window.hwnd() as _, MONITOR_DEFAULTTONEAREST), &mut info) }
                != 0;
        let work = info.rcWork;
        ok.then(|| (work.left, work.top, (work.right - work.left) as u32, (work.bottom - work.top) as u32))
    };
    #[cfg(not(windows))]
    let area = window.current_monitor().map(|monitor| {
        let (pos, size) = (monitor.position(), monitor.size());
        let height = size.height * 92 / 100;
        (pos.x, pos.y + ((size.height - height) / 2) as i32, size.width, height)
    });
    let Some((left, top, area_width, height)) = area else {
        return;
    };
    let width = width.min(area_width);
    window.set_outer_position(PhysicalPosition::new(left + ((area_width - width) / 2) as i32, top));
    window.set_inner_size(PhysicalSize::new(width, height));
    // Windows 11 draws a 1px border outside the client area; trim whatever now overlaps the taskbar.
    #[cfg(windows)]
    {
        use tao::platform::windows::WindowExtWindows;
        use windows_sys::Win32::Foundation::RECT;
        use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
        let mut visible: RECT = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            DwmGetWindowAttribute(
                window.hwnd() as _,
                DWMWA_EXTENDED_FRAME_BOUNDS as u32,
                (&raw mut visible).cast(),
                size_of::<RECT>() as u32,
            )
        } == 0;
        let overlap = visible.bottom - (top + height as i32);
        if ok && overlap > 0 {
            window.set_inner_size(PhysicalSize::new(width, height - overlap as u32));
        }
    }
}

/// vjuu's brand colour (#a6e22e, Monokai green) as a Windows COLORREF (0x00BBGGRR); ui/style.css has it as --brand.
#[cfg(windows)]
const BRAND_COLORREF: u32 = 0x002E_E2A6;

/// Asks Windows 11 for rounded corners and a brand-coloured border, which frameless windows don't get by
/// default; older Windows ignores both. No-op elsewhere.
pub fn style_frame(window: &Window) {
    #[cfg(windows)]
    {
        use tao::platform::windows::WindowExtWindows;
        use windows_sys::Win32::Graphics::Dwm::{
            DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
        };
        let set = |attribute: i32, value: u32| unsafe {
            DwmSetWindowAttribute(
                window.hwnd() as _,
                attribute as u32,
                (&raw const value).cast(),
                size_of_val(&value) as u32,
            );
        };
        set(DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32);
        set(DWMWA_BORDER_COLOR, BRAND_COLORREF);
    }
    #[cfg(not(windows))]
    let _ = window;
}

/// Brings the window to the front with keyboard focus in the page.
///
/// Windows' foreground lock stops a process that has not had recent user input (the resident instance, or a
/// client started by an agent) from taking focus: `set_focus` alone just flashes the taskbar button. Sharing the
/// input queue of the window that has focus for a moment is the accepted workaround.
pub fn bring_to_front(window: &Window, webview: &WebView) {
    #[cfg(windows)]
    {
        use tao::platform::windows::WindowExtWindows;
        use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_MENU, keybd_event};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
        };
        let hwnd = window.hwnd() as _;
        unsafe {
            let ours = GetCurrentThreadId();
            let theirs = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
            let attached = theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, 1) != 0;
            BringWindowToTop(hwnd);
            // Console windows (cmd, pwsh) refuse the attach; a synthetic Alt tap counts as user input and lifts the lock.
            if SetForegroundWindow(hwnd) == 0 {
                keybd_event(VK_MENU as u8, 0, 0, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
                SetForegroundWindow(hwnd);
            }
            if attached {
                AttachThreadInput(ours, theirs, 0);
            }
        }
    }
    window.set_focus();
    let _ = webview.focus();
}
