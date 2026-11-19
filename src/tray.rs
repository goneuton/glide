//! Notification-area icon: appearance, start with Windows, quit.

use crate::shell::window::{quit, App, WM_TRAY};
use crate::theme::Appearance;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ,
};
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, SetForegroundWindow, TrackPopupMenu,
    IDI_APPLICATION, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_BOTTOMALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        // SAFETY: loads Glide's own icon (resource 1) at the size the tray uses.
        hIcon: unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, LoadImageW, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, SM_CXSMICON};
            let size = GetSystemMetrics(SM_CXSMICON);
            let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
            LoadImageW(Some(instance.into()), PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
                .map(|handle| HICON(handle.0))
                .or_else(|_| LoadIconW(None, IDI_APPLICATION))
                .unwrap_or_default()
        },
        ..Default::default()
    };
    for (slot, unit) in data.szTip.iter_mut().zip("Glide".encode_utf16()) {
        *slot = unit;
    }
    data
}

pub fn add(hwnd: HWND) {
    // SAFETY: the structure is fully initialised.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data(hwnd));
    }
}

pub fn remove(hwnd: HWND) {
    // SAFETY: as above.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
    }
}

/// Register or remove Glide from the programs Windows starts at sign-in.
pub fn set_autostart(on: bool) {
    let Ok(exe) = std::env::current_exe() else { return };
    let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the value buffer is NUL-terminated and its byte length is passed.
    unsafe {
        if on {
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                w!("Glide"),
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Glide"));
        }
    }
}

pub fn menu(hwnd: HWND, app: &mut App) {
    const SYSTEM: usize = 1;
    const LIGHT: usize = 2;
    const DARK: usize = 3;
    const AUTOSTART: usize = 4;
    const EDIT: usize = 5;
    const RELOAD: usize = 6;
    const QUIT: usize = 9;
    const ACCENT: usize = 100;
    const NOTIFY_FIX: usize = 7;
    const PRIVACY: usize = 8;
    const WELCOME: usize = 10;
    const BLUR: usize = 200;
    let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
    let appearance = app.settings.appearance;
    // SAFETY: a short-lived popup menu owned by this function.
    let command = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING | check(appearance == Appearance::System), SYSTEM, w!("Match Windows"));
        let _ = AppendMenuW(menu, MF_STRING | check(appearance == Appearance::Light), LIGHT, w!("Light"));
        let _ = AppendMenuW(menu, MF_STRING | check(appearance == Appearance::Dark), DARK, w!("Dark"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        // Accent colours: each has its own shade for light and for dark.
        for (index, (name, _, _)) in crate::theme::ACCENTS.iter().enumerate() {
            let label = windows::core::HSTRING::from(format!("Accent: {name}"));
            let _ = AppendMenuW(menu, MF_STRING | check(app.settings.accent == index), ACCENT + index, &label);
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        for (level, label) in [w!("Cover blur: Off"), w!("Cover blur: Light"), w!("Cover blur: Medium"), w!("Cover blur: Heavy")].into_iter().enumerate() {
            let _ = AppendMenuW(menu, MF_STRING | check(usize::from(app.settings.cover_blur) == level), BLUR + level, label);
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING | check(app.settings.autostart), AUTOSTART, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        // How notifications arrive, and a way to fix it when they are late.
        let mode = crate::modules::notify::MODE.load(std::sync::atomic::Ordering::Relaxed);
        let status = match mode {
            1 => w!("Notifications: instant"),
            2 => w!("Notifications: a few seconds late"),
            3 => w!("Notifications: blocked by Windows"),
            4 => w!("Notifications: off"),
            _ => w!("Notifications: starting"),
        };
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, status);
        match mode {
            2 if register_script().is_some() => {
                let _ = AppendMenuW(menu, MF_STRING, NOTIFY_FIX, w!("Make Notifications Instant..."));
            }
            3 => {
                let _ = AppendMenuW(menu, MF_STRING, NOTIFY_FIX, w!("Allow Notification Access in Windows..."));
            }
            _ => {}
        }
        let _ = AppendMenuW(menu, MF_STRING | check(app.settings.privacy), PRIVACY, w!("Privacy Mode (hide message text)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, WELCOME, w!("Set Up Glide Again..."));
        let _ = AppendMenuW(menu, MF_STRING, EDIT, w!("Edit Settings (city, region)..."));
        let _ = AppendMenuW(menu, MF_STRING, RELOAD, w!("Apply Settings and Refresh Weather"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, QUIT, w!("Quit Glide"));
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, at.x, at.y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        chosen.0 as usize
    };
    match command {
        SYSTEM => app.apply_appearance(Appearance::System),
        LIGHT => app.apply_appearance(Appearance::Light),
        DARK => app.apply_appearance(Appearance::Dark),
        AUTOSTART => {
            app.settings.autostart = !app.settings.autostart;
            set_autostart(app.settings.autostart);
            app.settings.save();
        }
        EDIT => {
            // Make sure the file exists, then open it in the default editor.
            app.settings.save();
            if let Some(path) = crate::settings::data_dir().map(|d| d.join("settings.json")) {
                let _ = std::process::Command::new("notepad.exe").arg(path).spawn();
            }
        }
        RELOAD => app.reload_settings(),
        PRIVACY => {
            app.settings.privacy = !app.settings.privacy;
            crate::modules::notify::PRIVATE.store(app.settings.privacy, std::sync::atomic::Ordering::Relaxed);
            app.settings.save();
            app.request_frame();
        }
        WELCOME => app.open_welcome(),
        NOTIFY_FIX => match register_script() {
            // Registers the identity (one administrator prompt) and restarts Glide.
            Some(script) if crate::modules::notify::MODE.load(std::sync::atomic::Ordering::Relaxed) == 2 => {
                let _ = std::process::Command::new("powershell.exe")
                    .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
                    .arg(script)
                    .spawn();
            }
            _ => {
                let _ = std::process::Command::new("explorer.exe").arg("ms-settings:privacy-notifications").spawn();
            }
        },
        QUIT => quit(hwnd),
        blur if (BLUR..BLUR + 4).contains(&blur) => {
            app.settings.cover_blur = (blur - BLUR) as u8;
            crate::modules::media::COVER_BLUR.store(app.settings.cover_blur, std::sync::atomic::Ordering::Relaxed);
            app.settings.save();
            app.request_frame();
        }
        accent if accent >= ACCENT && accent < ACCENT + crate::theme::ACCENTS.len() => {
            app.settings.accent = accent - ACCENT;
            app.apply_appearance(app.settings.appearance);
        }
        _ => {}
    }
}

/// The script that gives an installed Glide its identity, if it is next to glide.exe.
pub fn register_script() -> Option<std::path::PathBuf> {
    let script = std::env::current_exe().ok()?.parent()?.join("identity").join("register-identity.ps1");
    script.is_file().then_some(script)
}
