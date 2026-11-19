//! The host window and the message loop.
//!
//! The window is topmost, never takes focus and is only as large as the
//! island needs. While nothing moves the loop blocks in `GetMessage`, so an
//! idle Glide uses no CPU at all; while something animates, one frame is
//! drawn per display refresh.

use super::render::{Gfx, Rect};
use super::ui::Input;
use crate::island::{Event, Island};
use crate::modules::shelf::Item;
use crate::shell::ui::Action;
use crate::settings::Settings;
use crate::theme::{Appearance, Theme};
use crate::tray;
use std::cell::RefCell;
use std::time::Instant;
use windows::core::{w, Result};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Largest area the island can occupy, in logical pixels.
const SURFACE: (f32, f32) = (780.0, 440.0);
const TIMER_WAKE: usize = 1;
const TIMER_MINUTE: usize = 2;
/// After the first-run Namaste, open the setup window.
const TIMER_WELCOME: usize = 3;
pub const WM_TRAY: u32 = WM_APP + 1;
/// A new media state is waiting in the inbox.
pub const WM_MEDIA: u32 = WM_APP + 2;
/// A worker left an event in the inbox.
pub const WM_EVENT: u32 = WM_APP + 3;
/// A second launch asks the existing island to make itself visible.
pub const WM_SHOW_EXISTING: u32 = WM_APP + 4;
const WM_POWERBROADCAST: u32 = 0x0218;
const WM_CLIPBOARDUPDATE: u32 = 0x031D;
const WM_INPUT: u32 = 0x00FF;
const WM_MOUSEHWHEEL: u32 = 0x020E;
/// Windows moved, appeared or changed focus: look again at what is under the island.
const WM_WINDOWS_CHANGED: u32 = WM_APP + 6;

/// The shell showed a window (possibly a toast).
const WM_SHELL_SHOWED: u32 = WM_APP + 5;

unsafe extern "system" fn on_shell_event(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    shown: HWND,
    object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole windows (object 0) of the kind toasts are made of; the shell
    // shows many other things.
    let mut class = [0u16; 48];
    let len = if object == 0 { GetClassNameW(shown, &mut class) as usize } else { 0 };
    if String::from_utf16_lossy(&class[..len]) == "Windows.UI.Core.CoreWindow" {
        let hwnd = ISLAND_HWND.load(std::sync::atomic::Ordering::Relaxed);
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_SHELL_SHOWED, WPARAM(0), LPARAM(0));
    }
}

/// Process id of the Windows component that draws toasts.
fn shell_host() -> Option<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    // SAFETY: a process snapshot walked with a correctly sized entry and closed after.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|c| *c == 0).unwrap_or(0);
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case("ShellExperienceHost.exe") {
                found = Some(entry.th32ProcessID);
                break;
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        found
    }
}

static ISLAND_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

unsafe extern "system" fn on_win_event(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    let hwnd = ISLAND_HWND.load(std::sync::atomic::Ordering::Relaxed);
    let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_WINDOWS_CHANGED, WPARAM(0), LPARAM(0));
}

/// Channels on which worker threads hand results to the UI thread.
pub struct Inbox {
    pub media: std::sync::mpsc::Receiver<crate::modules::media::State>,
    pub events: std::sync::mpsc::Receiver<Event>,
    /// Ask the weather worker for a fresh forecast now.
    pub weather: std::sync::mpsc::Sender<()>,
    /// Tell the notification worker that a toast may have just appeared.
    pub notify: std::sync::mpsc::Sender<()>,
    /// For re-subscribing to the volume of a new default device.
    pub post: crate::island::Post,
    pub volume: Option<crate::modules::status::VolumeWatch>,
    /// Subscriptions that must stay alive as long as the app runs.
    pub keep: Vec<Box<dyn std::any::Any>>,
}
/// Sent when the pointer leaves the window (declared with the common controls, not here).
const WM_MOUSELEAVE: u32 = 0x02A3;

pub struct App {
    hwnd: HWND,
    gfx: Gfx,
    /// Click-through layer under the island for its shadow, glow and rings.
    fx_hwnd: HWND,
    fx: Gfx,
    /// While something is typed into the island: the window that had the keyboard before.
    keyboard_from: Option<HWND>,
    pub island: Island,
    theme: Theme,
    pub settings: Settings,
    input: Input,
    last_frame: Option<Instant>,
    need_frame: bool,
    /// Part of the surface the window currently shows (logical pixels).
    covered: Rect,
    /// Screen position of the surface's top centre, in device pixels.
    anchor: (i32, i32),
    tracking: bool,
    inbox: Inbox,
    /// The next clipboard change is our own write: do not shelve it again.
    own_clipboard: bool,
    caps: bool,
    pending_drag: Option<Item>,
    /// Another app's window sits where the island's pill would be.
    obscured: bool,
    /// What of the media state can change the picture; position alone cannot at rest.
    media_key: (bool, bool, String, u64),
    power_read: Option<Instant>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the app unless it is already in use further up the stack
/// (Windows re-enters the window procedure from calls such as `SetWindowPos`).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut app| app.as_mut().map(f)))
}

fn reduce_motion() -> bool {
    let mut on = windows::core::BOOL(1);
    // SAFETY: `on` is a valid BOOL out-value for this query.
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut on as *mut windows::core::BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    !on.as_bool()
}

/// Top centre of the primary monitor's work area and its scale factor.
fn primary_anchor() -> ((i32, i32), f32) {
    // SAFETY: plain monitor queries with valid out-values.
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        let area = info.rcWork;
        (((area.left + area.right) / 2, area.top), dx as f32 / 96.0)
    }
}

impl App {
    fn schedule_minute(&self) {
        // SAFETY: plain value-returning call and timer on our own window.
        unsafe {
            let now = GetLocalTime();
            let until_next = (60 - u32::from(now.wSecond)) * 1000 - u32::from(now.wMilliseconds).min(999) + 30;
            SetTimer(Some(self.hwnd), TIMER_MINUTE, until_next, None);
        }
    }

    /// Is the user in an app? The always-on pill is for the desktop only: with any
    /// app in front, the island tucks back to its lip.
    fn app_under_island(&self) -> bool {
        // SAFETY: stateless window queries.
        unsafe {
            let front = GetForegroundWindow();
            if front.0.is_null() || front == self.hwnd {
                return self.obscured;
            }
            let mut class = [0u16; 64];
            let len = GetClassNameW(front, &mut class) as usize;
            let class = String::from_utf16_lossy(&class[..len]);
            // The desktop, the taskbar and the shell's own surfaces are not apps.
            !matches!(
                class.as_str(),
                "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "SHELLDLL_DefView" | "NotifyIconOverflowWindow"
            )
        }
    }

    pub fn request_frame(&mut self) {
        self.need_frame = true;
    }

    pub fn open_welcome(&mut self) {
        let can_instant = crate::tray::register_script().is_some() && crate::modules::notify::MODE.load(std::sync::atomic::Ordering::Relaxed) != 1;
        crate::welcome::open(self.hwnd, self.gfx.scale, can_instant);
    }

    /// Settings were edited outside the app: read them again and apply.
    pub fn reload_settings(&mut self) {
        self.settings = Settings::load();
        crate::modules::media::COVER_BLUR.store(self.settings.cover_blur.min(3), std::sync::atomic::Ordering::Relaxed);
        self.theme = Theme::resolve(self.settings.appearance, self.settings.accent);
        crate::modules::notify::PRIVATE.store(self.settings.privacy, std::sync::atomic::Ordering::Relaxed);
        let _ = self.inbox.weather.send(());
        self.need_frame = true;
    }

    pub fn apply_appearance(&mut self, appearance: Appearance) {
        self.settings.appearance = appearance;
        self.theme = Theme::resolve(appearance, self.settings.accent);
        self.settings.save();
        self.need_frame = true;
    }

    fn pointer_from_screen(&self) -> (f32, f32) {
        let mut p = POINT::default();
        // SAFETY: `p` is a valid out-value.
        unsafe {
            let _ = GetCursorPos(&mut p);
        }
        let cx = self.gfx.size().0 / 2.0;
        ((p.x - self.anchor.0) as f32 / self.gfx.scale + cx, (p.y - self.anchor.1) as f32 / self.gfx.scale)
    }

    fn update_pointer(&mut self, x: f32, y: f32) {
        let cx = self.gfx.size().0 / 2.0;
        (self.input.x, self.input.y) = (x, y);
        self.input.inside = self.island.hit_area(cx).contains(x, y);
        self.need_frame = true;
    }

    /// Move and size the window so it shows `area` of the surface.
    fn cover(&mut self, area: Rect) {
        if area == self.covered {
            return;
        }
        self.covered = area;
        let scale = self.gfx.scale;
        let cx = self.gfx.size().0 / 2.0;
        let x = self.anchor.0 + ((area.x - cx) * scale).round() as i32;
        let (w, h) = ((area.w * scale).ceil() as i32, (area.h * scale).ceil() as i32);
        self.gfx.set_origin(area.x, area.y);
        // SAFETY: our own window; re-entrant messages fall through to DefWindowProc.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                self.anchor.1,
                w.max(1),
                h.max(1),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            self.place_fx();
        }
    }

    /// The effects layer spans the whole surface, just below the island.
    fn place_fx(&self) {
        let scale = self.gfx.scale;
        let (w, h) = ((SURFACE.0 * scale).ceil() as i32, (SURFACE.1 * scale).ceil() as i32);
        // SAFETY: our own window.
        unsafe {
            let _ = SetWindowPos(self.fx_hwnd, Some(self.hwnd), self.anchor.0 - w / 2, self.anchor.1, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    fn frame(&mut self) {
        let now = Instant::now();
        // After a pause the first frame must not jump by the idle time.
        let dt = self.last_frame.map_or(1.0 / 60.0, |last| (now - last).as_secs_f32().clamp(0.001, 0.05));
        self.last_frame = Some(now);
        self.island.reduce_motion = reduce_motion_cached();
        // Power source and Energy Saver, looked at every few seconds at most.
        if self.power_read.is_none_or(|at| at.elapsed() > std::time::Duration::from_secs(5)) {
            self.power_read = Some(Instant::now());
            let mut status = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
            // SAFETY: `status` is a valid out-value.
            if unsafe { windows::Win32::System::Power::GetSystemPowerStatus(&mut status) }.is_ok() {
                self.island.power = if status.SystemStatusFlag == 1 { 2 } else if status.ACLineStatus == 0 { 1 } else { 0 };
            }
        }
        // SAFETY: plain query.
        self.input.keyboard = unsafe { GetForegroundWindow() } == self.hwnd;
        let frame = self.island.frame(&self.gfx, &self.fx, &self.theme, &self.input, dt, self.obscured);
        self.input.typed_len = 0;
        self.take_keyboard(frame.keyboard);
        if std::mem::take(&mut self.island.mode_changed) {
            self.settings.last_mode = self.island.current_name().to_string();
            self.settings.save();
        }
        (self.input.pressed, self.input.released, self.input.wheel, self.input.wheel_x) = (false, false, 0.0, 0.0);
        for action in frame.actions {
            match action {
                Action::Copy(item) => {
                    self.own_clipboard = super::ole::write_clipboard(self.hwnd, &item);
                }
                Action::Drag(item) => {
                    // Carried out by the message loop: dragging blocks until the drop.
                    self.pending_drag = Some(item);
                }
                Action::Open(target) => {
                    let wide = windows::core::HSTRING::from(target);
                    // SAFETY: opens a link with its default handler.
                    unsafe {
                        windows::Win32::UI::Shell::ShellExecuteW(None, w!("open"), &wide, None, None, SW_SHOWNORMAL);
                    }
                }
            }
        }

        // Grow at once, shrink only when everything has come to rest.
        let grows = frame.bounds.x < self.covered.x
            || frame.bounds.right() > self.covered.right()
            || frame.bounds.bottom() > self.covered.bottom();
        if grows || !frame.animating {
            self.cover(frame.bounds);
        }
        self.need_frame = frame.animating;
        if !frame.animating {
            self.last_frame = None;
        }
        // SAFETY: timers on our own window.
        unsafe {
            match frame.wake_in {
                Some(wait) => {
                    SetTimer(Some(self.hwnd), TIMER_WAKE, wait.as_millis().max(10) as u32, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_WAKE);
                }
            }
        }
    }

    /// The island normally never takes focus. A field being typed into needs the
    /// keyboard, so take it then, and hand it back to where it was afterwards.
    fn take_keyboard(&mut self, wanted: bool) {
        // SAFETY: focus changes on our own window, or back to the one we took it from.
        unsafe {
            if wanted && self.keyboard_from.is_none() {
                self.keyboard_from = Some(GetForegroundWindow());
                let _ = SetForegroundWindow(self.hwnd);
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(self.hwnd));
                self.need_frame = true;
            } else if !wanted {
                if let Some(previous) = self.keyboard_from.take() {
                    if GetForegroundWindow() == self.hwnd && !previous.is_invalid() {
                        let _ = SetForegroundWindow(previous);
                    }
                }
            }
        }
    }

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: valid structure for our own window.
                    self.tracking = unsafe { TrackMouseEvent(&mut track).is_ok() };
                }
                // Screen position, not the message's client position: it stays right
                // whatever Windows decides about the window's frame.
                let (x, y) = self.pointer_from_screen();
                self.update_pointer(x, y);
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                // Resizing the window under the pointer also reports a leave: ask
                // where the pointer really is before believing it.
                let (x, y) = self.pointer_from_screen();
                self.update_pointer(x, y);
            }
            WM_LBUTTONDOWN => {
                let (x, y) = self.pointer_from_screen();
                self.update_pointer(x, y);
                (self.input.down, self.input.pressed) = (true, true);
                (self.input.press_x, self.input.press_y) = (self.input.x, self.input.y);
                // SAFETY: capture on our own window so the release always arrives.
                unsafe {
                    SetCapture(self.hwnd);
                }
                self.need_frame = true;
            }
            WM_LBUTTONUP => {
                (self.input.down, self.input.released) = (false, true);
                // SAFETY: releases the capture taken above.
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.need_frame = true;
            }
            WM_MOUSEWHEEL => {
                // Vertical scrolling scrolls lists on a page; modes change sideways.
                self.input.wheel -= ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
                self.need_frame = true;
            }
            WM_CHAR => {
                let code = wparam.0 as u32;
                if code == 0x16 {
                    // Ctrl+V: paste text, as if typed.
                    for item in super::ole::read_clipboard(self.hwnd) {
                        if let Item::Text(text) = item {
                            text.chars().filter(|c| !c.is_whitespace()).for_each(|c| self.input.type_char(c));
                        }
                    }
                } else if let Some(c) = char::from_u32(code) {
                    self.input.type_char(c);
                }
                self.need_frame = true;
                return Some(LRESULT(0));
            }
            // Gaining or losing the keyboard can end typing.
            WM_ACTIVATE => {
                self.need_frame = true;
            }
            // The whole window is ours to draw: no caption, no border.
            WM_NCCALCSIZE if wparam.0 != 0 => return Some(LRESULT(0)),
            WM_NCHITTEST => return Some(LRESULT(HTCLIENT as isize)),
            // Never take focus away from what the user is doing.
            WM_MOUSEACTIVATE => return Some(LRESULT(MA_NOACTIVATE as isize)),
            WM_TIMER => match wparam.0 {
                TIMER_WAKE => {
                    // SAFETY: timer on our own window.
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_WAKE);
                    }
                    let (x, y) = self.pointer_from_screen();
                    self.update_pointer(x, y);
                }
                TIMER_WELCOME => {
                    // SAFETY: timer on our own window.
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_WELCOME);
                    }
                    self.open_welcome();
                }
                TIMER_MINUTE => {
                    let obscured = self.app_under_island();
                    let changed = obscured != self.obscured;
                    self.obscured = obscured;
                    self.island.refresh();
                    self.schedule_minute();
                    // The clock only shows when the island is open.
                    if changed || !self.island.resting() {
                        self.need_frame = true;
                    }
                }
                _ => {}
            },
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                self.theme = Theme::resolve(self.settings.appearance, self.settings.accent);
                REDUCE_MOTION.with(|cell| cell.set(reduce_motion()));
                self.need_frame = true;
            }
            crate::welcome::WM_WELCOME_DONE => {
                self.reload_settings();
                // Instant notifications: register the identity. The script asks for
                // administrator approval once and restarts Glide with its identity.
                if wparam.0 == 1 {
                    if let Some(script) = crate::tray::register_script() {
                        let _ = std::process::Command::new("powershell.exe")
                            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
                            .arg(script)
                            .spawn();
                    }
                }
            }
            WM_DISPLAYCHANGE => {
                self.anchor = primary_anchor().0;
                let area = std::mem::take(&mut self.covered);
                self.cover(area);
                self.place_fx();
                self.need_frame = true;
            }
            WM_MEDIA => {
                // Only the newest state matters.
                if let Some(state) = self.inbox.media.try_iter().last() {
                    let key = (state.present, state.playing, state.title.clone(), state.art_serial);
                    let changed = key != self.media_key;
                    self.media_key = key;
                    self.island.dispatch(&Event::Media(state), &self.gfx, &self.theme);
                    // A position update while only the lip shows changes nothing on screen.
                    if changed || !self.island.resting() {
                        self.need_frame = true;
                    }
                }
            }
            WM_EVENT => {
                let events: Vec<Event> = self.inbox.events.try_iter().collect();
                for event in events {
                    if let Event::AudioDevice = event {
                        // Listen to the new device's volume; the old one is dropped.
                        self.inbox.volume = crate::modules::status::watch_volume(self.inbox.post.clone());
                    }
                    self.island.dispatch(&event, &self.gfx, &self.theme);
                }
                self.need_frame = true;
            }
            WM_SHOW_EXISTING => {
                // The island is normally a small, non-activating topmost window.
                // Restore its surface and bring it above other windows without
                // stealing keyboard focus from the user's current application.
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(HWND_TOPMOST),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                }
                self.need_frame = true;
            }
            WM_MOUSEHWHEEL => {
                self.input.wheel_x += ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
                self.need_frame = true;
            }
            WM_SHELL_SHOWED => {
                let _ = self.inbox.notify.send(());
            }
            WM_WINDOWS_CHANGED => {
                let obscured = self.app_under_island();
                if obscured != self.obscured {
                    self.obscured = obscured;
                    self.need_frame = true;
                }
            }
            WM_CLIPBOARDUPDATE => {
                if std::mem::take(&mut self.own_clipboard) {
                    return Some(LRESULT(0));
                }
                let items = super::ole::read_clipboard(self.hwnd);
                if !items.is_empty() {
                    self.island.dispatch(&Event::Clipboard(items), &self.gfx, &self.theme);
                    self.need_frame = true;
                }
            }
            WM_INPUT => {
                // Every key press arrives here; only a change of Caps Lock is acted on.
                let caps = crate::modules::status::caps_lock();
                if caps != self.caps {
                    self.caps = caps;
                    self.island.dispatch(&Event::Keyboard, &self.gfx, &self.theme);
                    self.need_frame = true;
                }
                return None;
            }
            WM_POWERBROADCAST => {
                self.island.dispatch(&Event::Power, &self.gfx, &self.theme);
                self.need_frame = true;
                return None;
            }
            WM_TRAY => {
                let event = (lparam.0 & 0xffff) as u32;
                if event == WM_RBUTTONUP || event == WM_CONTEXTMENU || event == WM_LBUTTONUP {
                    tray::menu(self.hwnd, self);
                }
            }
            WM_DESTROY => {
                tray::remove(self.hwnd);
                // SAFETY: ends the message loop.
                unsafe { PostQuitMessage(0) };
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

thread_local! {
    static REDUCE_MOTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn reduce_motion_cached() -> bool {
    REDUCE_MOTION.with(|cell| cell.get())
}

unsafe extern "system" fn fx_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(-1); // HTTRANSPARENT
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match with_app(|app| app.handle(msg, wparam, lparam)).flatten() {
        Some(result) => result,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Create the window and run until Glide quits.
pub fn run(build: impl FnOnce(&Settings, isize) -> (Island, Inbox)) -> Result<()> {
    // SAFETY: standard window-class registration, window creation and message loop.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // COM and drag-and-drop for this thread.
        let _ = windows::Win32::System::Ole::OleInitialize(None);
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("GlideIsland"),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassW(&class);
        let (anchor, scale) = primary_anchor();
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("GlideIsland"),
            w!("Glide"),
            WS_POPUP,
            anchor.0 - 100,
            anchor.1,
            200,
            40,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let gfx = Gfx::new(hwnd, SURFACE, scale)?;
        // The effects layer: layered and transparent to the mouse, so the glow and
        // rings never take a click from the window underneath.
        let fx_class = WNDCLASSW {
            lpfnWndProc: Some(fx_proc),
            hInstance: instance.into(),
            lpszClassName: w!("GlideIslandFx"),
            ..Default::default()
        };
        RegisterClassW(&fx_class);
        let fx_hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("GlideIslandFx"),
            w!("Glide effects"),
            WS_POPUP,
            anchor.0 - (SURFACE.0 * scale) as i32 / 2,
            anchor.1,
            (SURFACE.0 * scale).ceil() as i32,
            (SURFACE.1 * scale).ceil() as i32,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        let _ = SetLayeredWindowAttributes(fx_hwnd, windows::Win32::Foundation::COLORREF(0), 255, LWA_ALPHA);
        let fx = gfx.sibling(fx_hwnd, SURFACE)?;
        let settings = Settings::load();
        REDUCE_MOTION.with(|cell| cell.set(reduce_motion()));
        let (island, inbox) = build(&settings, hwnd.0 as isize);
        let app = App {
            hwnd,
            island,
            inbox,
            theme: Theme::resolve(settings.appearance, settings.accent),
            settings,
            gfx,
            fx_hwnd,
            fx,
            keyboard_from: None,
            input: Input::default(),
            last_frame: None,
            need_frame: true,
            covered: Rect::default(),
            anchor,
            tracking: false,
            own_clipboard: false,
            caps: crate::modules::status::caps_lock(),
            pending_drag: None,
            obscured: false,
            media_key: (false, false, String::new(), 0),
            power_read: None,
        };
        app.schedule_minute();
        let _ = windows::Win32::System::DataExchange::AddClipboardFormatListener(hwnd);
        // Foreground changes, moves, minimise and restore: all in the 0x03..0x17 range.
        ISLAND_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
        let _ = windows::Win32::UI::Accessibility::SetWinEventHook(0x0003, 0x0017, None, Some(on_win_event), 0, 0, 0);
        // Toasts are windows of the shell host: when it shows one, check for a new notification.
        if let Some(pid) = shell_host() {
            let _ = windows::Win32::UI::Accessibility::SetWinEventHook(0x8002, 0x8002, None, Some(on_shell_event), pid, 0, 0);
        }
        tray::add(hwnd);
        APP.with(|cell| *cell.borrow_mut() = Some(app));
        with_app(|app| {
            app.obscured = app.app_under_island();
            crate::modules::notify::PRIVATE.store(app.settings.privacy, std::sync::atomic::Ordering::Relaxed);
            // First run: say Namaste, then open setup.
            if !app.settings.welcomed {
                app.island.announce_named("hello", app.theme.accent);
                SetTimer(Some(app.hwnd), TIMER_WELCOME, 2800, None);
            }
        });

        let mut msg = MSG::default();
        loop {
            if with_app(|app| app.need_frame).unwrap_or(false) {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return Ok(());
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                // Blocks until the display's next refresh: one frame per refresh, no more.
                with_app(App::frame);
                if let Some(Some((hwnd, item))) = with_app(|app| app.pending_drag.take().map(|item| (app.hwnd, item))) {
                    super::ole::drag_out(hwnd, &item);
                    // The drop swallowed the button release.
                    with_app(|app| {
                        (app.input.down, app.input.released, app.need_frame) = (false, true, true);
                        app.own_clipboard = false;
                    });
                }
            } else {
                // Nothing is moving: sleep until Windows has something for us.
                if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    return Ok(());
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// Ask an already-running Glide window to reveal itself after a second launch.
pub fn show_existing() {
    // SAFETY: the class/title pair is owned by Glide; posting this message does
    // not transfer ownership or dereference the returned handle.
    unsafe {
        if let Ok(hwnd) = FindWindowW(w!("GlideIsland"), w!("Glide")) {
            let _ = PostMessageW(Some(hwnd), WM_SHOW_EXISTING, WPARAM(0), LPARAM(0));
        }
    }
}

/// Used by the tray menu to end the app.
pub fn quit(hwnd: HWND) {
    // SAFETY: destroys our own window, which posts the quit message.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}
