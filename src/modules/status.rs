//! Small system signals: volume changes, Caps Lock, and the microphone or
//! camera being in use. Each one is reported by Windows; none is polled.

use crate::island::{Activity, Event, Post};
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::ui::Ui;
use crate::theme::{Color, Theme};
use windows::core::{implement, w, Result, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
    MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, REG_NOTIFY_CHANGE_LAST_SET, REG_NOTIFY_CHANGE_NAME, RRF_RT_REG_QWORD,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CAPITAL};
use windows::Win32::UI::Input::{RegisterRawInputDevices, RAWINPUTDEVICE, RIDEV_INPUTSINK};

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeListener {
    post: Post,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeListener_Impl {
    fn OnNotify(&self, data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        // SAFETY: Windows passes a valid structure for the duration of the call.
        if let Some(data) = unsafe { data.as_ref() } {
            self.post.send(Event::Volume { level: data.fMasterVolume, muted: data.bMuted.as_bool() });
        }
        Ok(())
    }
}

/// Keeps the volume subscription alive; drop it to unsubscribe.
pub struct VolumeWatch {
    _endpoint: IAudioEndpointVolume,
    _callback: IAudioEndpointVolumeCallback,
}

/// Watch the default output device. Call on the UI thread (COM is initialised there).
pub fn watch_volume(post: Post) -> Option<VolumeWatch> {
    // SAFETY: standard COM activation; the callback is kept alive by the returned value.
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eMultimedia).ok()?;
        let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        let callback: IAudioEndpointVolumeCallback = VolumeListener { post }.into();
        endpoint.RegisterControlChangeNotify(&callback).ok()?;
        Some(VolumeWatch { _endpoint: endpoint, _callback: callback })
    }
}

/// Tells the island when the default audio output changes, so the level meter
/// and the volume listener can move to the new device (headphones connecting).
#[implement(IMMNotificationClient)]
struct DeviceListener {
    post: Post,
}

impl IMMNotificationClient_Impl for DeviceListener_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eMultimedia {
            self.post.send(Event::AudioDevice);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

/// Keeps the device subscription alive; drop it to unsubscribe.
pub struct DeviceWatch {
    devices: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl Drop for DeviceWatch {
    fn drop(&mut self) {
        // SAFETY: unregisters the client registered in `watch_audio_device`.
        unsafe {
            let _ = self.devices.UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}

/// Watch for the default output device changing. Call on the UI thread.
pub fn watch_audio_device(post: Post) -> Option<DeviceWatch> {
    // SAFETY: standard COM activation; the client lives as long as the returned value.
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let client: IMMNotificationClient = DeviceListener { post }.into();
        devices.RegisterEndpointNotificationCallback(&client).ok()?;
        Some(DeviceWatch { devices, client })
    }
}

/// Ask Windows to tell this window about key presses even in the background.
/// Only the Caps Lock state is ever looked at; the keys themselves are not read.
pub fn watch_keyboard(hwnd: isize) {
    let device =
        RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: HWND(hwnd as *mut _) };
    // SAFETY: one correctly sized registration entry.
    unsafe {
        let _ = RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32);
    }
}

pub fn caps_lock() -> bool {
    // SAFETY: stateless query.
    unsafe { GetKeyState(i32::from(VK_CAPITAL.0)) & 1 != 0 }
}

/// Is any app using this capability right now? Windows records a start time
/// and clears the stop time for every app that currently holds it.
fn in_use(root: HKEY) -> bool {
    fn scan(key: HKEY, depth: u32) -> bool {
        let (mut stop, mut size) = (1u64, 8u32);
        // SAFETY: `stop`/`size` describe a valid 8-byte buffer; names are NUL-terminated.
        unsafe {
            let status = RegGetValueW(
                key,
                PCWSTR::null(),
                w!("LastUsedTimeStop"),
                RRF_RT_REG_QWORD,
                None,
                Some((&mut stop as *mut u64).cast()),
                Some(&mut size),
            );
            if status.is_ok() && stop == 0 {
                return true;
            }
            if depth == 0 {
                return false;
            }
            for index in 0.. {
                let mut name = [0u16; 512];
                let mut len = name.len() as u32;
                if RegEnumKeyExW(key, index, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None)
                    .is_err()
                {
                    break;
                }
                let mut child = HKEY::default();
                if RegOpenKeyExW(key, PCWSTR(name.as_ptr()), None, KEY_READ, &mut child).is_ok() {
                    let found = scan(child, depth - 1);
                    let _ = RegCloseKey(child);
                    if found {
                        return true;
                    }
                }
            }
        }
        false
    }
    // Packaged apps sit directly under the key, desktop apps under "NonPackaged".
    scan(root, 2)
}

/// Watch microphone and camera use. One sleeping thread per capability.
pub fn watch_privacy(post: Post) {
    for (name, camera) in [("microphone", false), ("webcam", true)] {
        let post = post.clone();
        let _ = std::thread::Builder::new().name(format!("glide-{name}")).spawn(move || {
            let path: Vec<u16> = format!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore\\{name}\0"
            )
            .encode_utf16()
            .collect();
            let mut key = HKEY::default();
            // SAFETY: NUL-terminated path; the key stays open for the life of the thread.
            if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), None, KEY_READ, &mut key) }.is_err() {
                return;
            }
            let mut last = None;
            loop {
                let now = in_use(key);
                if last != Some(now) {
                    last = Some(now);
                    post.send(Event::Privacy { camera, on: now });
                }
                // Blocks until something under the key changes.
                // SAFETY: synchronous wait on an open key.
                let waited = unsafe {
                    RegNotifyChangeKeyValue(key, true, REG_NOTIFY_CHANGE_LAST_SET | REG_NOTIFY_CHANGE_NAME, None, false)
                };
                if waited.is_err() {
                    return;
                }
            }
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shown {
    Nothing,
    Volume(f32, bool),
    Caps(bool),
    Microphone,
    Camera,
}

/// Shows the most recent signal for a moment; marks ongoing mic/camera use with a dot.
pub struct Status {
    shown: Shown,
    microphone: bool,
    camera: bool,
    caps: bool,
}

impl Status {
    pub fn new() -> Self {
        Self { shown: Shown::Nothing, microphone: false, camera: false, caps: caps_lock() }
    }
}

impl Activity for Status {
    fn name(&self) -> &'static str {
        "status"
    }

    fn compact_width(&self, _g: &Gfx) -> f32 {
        match self.shown {
            Shown::Volume(..) => 236.0,
            Shown::Microphone | Shown::Camera => 232.0,
            _ => 190.0,
        }
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let mid = r.y + r.h / 2.0;
        match self.shown {
            Shown::Volume(level, muted) => {
                let glyph = if muted || level <= 0.001 { '\u{E74F}' } else { '\u{E767}' };
                ui.g.icon(glyph, r.x + 8.0, mid, 15.0, t.accent);
                let bar = Rect::new(r.x + 28.0, mid - 2.5, r.w - 72.0, 5.0);
                ui.g.fill_rect(bar, 2.5, t.track);
                if !muted && level > 0.001 {
                    ui.g.fill_rect(Rect::new(bar.x, bar.y, (bar.w * level).max(5.0), bar.h), 2.5, t.accent);
                }
                let text = if muted { "Mute".to_string() } else { format!("{:.0}", level * 100.0) };
                ui.number(&text, r, 13.0, true, Align::Right, t.text);
            }
            Shown::Caps(on) => {
                ui.g.icon('\u{E84A}', r.x + 8.0, mid, 15.0, if on { t.accent } else { t.secondary });
                ui.label(if on { "Caps Lock on" } else { "Caps Lock off" }, r, 13.5, 600, Align::Right, t.text);
            }
            Shown::Microphone => {
                ui.g.icon('\u{E720}', r.x + 8.0, mid, 15.0, t.amber);
                ui.label("Microphone in use", r, 13.5, 600, Align::Right, t.text);
            }
            Shown::Camera => {
                ui.g.icon('\u{E714}', r.x + 8.0, mid, 15.0, t.green);
                ui.label("Camera in use", r, 13.5, 600, Align::Right, t.text);
            }
            Shown::Nothing => {}
        }
    }

    fn badge(&self, t: &Theme) -> Option<Color> {
        // Camera wins: it is the more sensitive of the two.
        if self.camera {
            Some(t.green)
        } else if self.microphone {
            Some(t.amber)
        } else {
            None
        }
    }

    fn on_event(&mut self, event: &Event, _g: &Gfx, t: &Theme) -> Option<Color> {
        // A fully transparent colour opens the island quietly: no glow, no wobble.
        let quiet = Color::rgb(0).alpha(0.0);
        match *event {
            Event::Volume { level, muted } => {
                self.shown = Shown::Volume(level, muted);
                Some(quiet)
            }
            Event::Keyboard => {
                let now = caps_lock();
                if now == self.caps {
                    return None;
                }
                self.caps = now;
                self.shown = Shown::Caps(now);
                Some(quiet)
            }
            Event::Privacy { camera, on } => {
                let was = if camera { self.camera } else { self.microphone };
                if camera {
                    self.camera = on;
                } else {
                    self.microphone = on;
                }
                // Starting is worth a look; stopping just removes the dot.
                if on && !was {
                    self.shown = if camera { Shown::Camera } else { Shown::Microphone };
                    Some(if camera { t.green } else { t.amber })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}
