//! Battery level and charging, and the batteries of connected Bluetooth
//! devices. Windows tells us when power changes; Bluetooth levels are read
//! when the island is opened, at most once a minute.

use crate::island::{Activity, Event, Post, PAGE};
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::ui::Ui;
use crate::theme::{Color, Theme};
use std::time::{Duration, Instant};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Level {
    /// The PC has a battery at all.
    present: bool,
    plugged: bool,
    percent: u8,
    /// Seconds of use left, when Windows has an estimate.
    remaining: Option<u32>,
}

fn read() -> Level {
    let mut status = SYSTEM_POWER_STATUS::default();
    // SAFETY: `status` is a valid out-value.
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        return Level::default();
    }
    Level {
        present: status.BatteryFlag & 128 == 0 && status.BatteryLifePercent <= 100,
        plugged: status.ACLineStatus == 1,
        percent: status.BatteryLifePercent.min(100),
        remaining: (status.BatteryLifeTime != u32::MAX).then_some(status.BatteryLifeTime),
    }
}

/// Battery level of every device that reports one, by name (from the same
/// device properties Windows' Bluetooth settings read). Slow: off the UI thread.
fn battery_levels() -> Vec<(String, u8)> {
    use windows::core::GUID;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDevicePropertyW,
        DIGCF_ALLCLASSES, DIGCF_PRESENT, SP_DEVINFO_DATA,
    };
    use windows::Win32::Foundation::DEVPROPKEY;
    const BATTERY: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5), pid: 2 };
    const NAME: DEVPROPKEY = DEVPROPKEY { fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0), pid: 14 };
    let mut found = Vec::new();
    // SAFETY: the device list is destroyed before returning; buffers outlive their calls.
    unsafe {
        let Ok(devices) = SetupDiGetClassDevsW(None, None, None, DIGCF_ALLCLASSES | DIGCF_PRESENT) else { return found };
        let mut data = SP_DEVINFO_DATA { cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32, ..Default::default() };
        let mut index = 0;
        while SetupDiEnumDeviceInfo(devices, index, &mut data).is_ok() {
            index += 1;
            let mut kind = Default::default();
            let mut level = [0u8; 4];
            if SetupDiGetDevicePropertyW(devices, &data, &BATTERY, &mut kind, Some(&mut level), None, 0).is_err() || level[0] > 100 {
                continue;
            }
            let mut bytes = [0u8; 512];
            if SetupDiGetDevicePropertyW(devices, &data, &NAME, &mut kind, Some(&mut bytes), None, 0).is_ok() {
                let wide: Vec<u16> = bytes.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).take_while(|c| *c != 0).collect();
                found.push((String::from_utf16_lossy(&wide), level[0]));
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(devices);
    }
    found
}

/// Watch which Bluetooth devices are connected right now, classic and low-energy,
/// with their battery levels. Windows reports each connection change; send `()`
/// on the returned channel to read the levels again (they change on their own).
pub fn watch_bluetooth(post: Post) -> std::sync::mpsc::Sender<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let wake = tx.clone();
    use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice};
    use windows::Devices::Enumeration::{DeviceInformation, DeviceWatcher};
    use windows::Foundation::TypedEventHandler;
    let _ = std::thread::Builder::new().name("glide-bluetooth".into()).spawn(move || {
        let selectors = || -> windows::core::Result<[windows::core::HSTRING; 2]> {
            Ok([
                BluetoothDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected)?,
                BluetoothLEDevice::GetDeviceSelectorFromConnectionStatus(BluetoothConnectionStatus::Connected)?,
            ])
        };
        let Ok(selectors) = selectors() else { return };
        let read = |selectors: &[windows::core::HSTRING; 2]| -> Vec<(String, Option<u8>)> {
            let mut names: Vec<String> = Vec::new();
            for selector in selectors {
                let Ok(found) = DeviceInformation::FindAllAsyncAqsFilter(selector).and_then(|op| op.get()) else { continue };
                for device in found {
                    let name = device.Name().map(|n| n.to_string()).unwrap_or_default();
                    // One device can appear as classic and as low-energy: list it once.
                    if !name.trim().is_empty() && !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            names.sort_by_key(|n| n.to_lowercase());
            // A device's battery is reported on its own node, named after it
            // ("Rockerz 255 Pro+ Hands-Free AG"): match by name.
            let levels = if names.is_empty() { Vec::new() } else { battery_levels() };
            names
                .into_iter()
                .map(|name| {
                    let level = levels.iter().filter(|(node, _)| node.starts_with(&name) || name.starts_with(node.as_str())).map(|(_, l)| *l).max();
                    (name, level)
                })
                .collect()
        };
        let tx = wake;
        let mut watchers: Vec<DeviceWatcher> = Vec::new();
        for selector in &selectors {
            let Ok(watcher) = DeviceInformation::CreateWatcherAqsFilter(selector) else { continue };
            let (a, r, u) = (tx.clone(), tx.clone(), tx.clone());
            let _ = watcher.Added(&TypedEventHandler::new(move |_, _| {
                let _ = a.send(());
                Ok(())
            }));
            let _ = watcher.Removed(&TypedEventHandler::new(move |_, _| {
                let _ = r.send(());
                Ok(())
            }));
            let _ = watcher.Updated(&TypedEventHandler::new(move |_, _| {
                let _ = u.send(());
                Ok(())
            }));
            if watcher.Start().is_ok() {
                watchers.push(watcher);
            }
        }
        let mut last = None;
        loop {
            let names = read(&selectors);
            if last.as_ref() != Some(&names) {
                post.send(Event::Bluetooth(names.clone()));
                last = Some(names);
            }
            // Sleep until Windows reports a change, then let the burst settle.
            if rx.recv().is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(600));
            while rx.try_recv().is_ok() {}
        }
    });
    tx
}

/// A glyph for a device, guessed from its name.
fn device_glyph(name: &str) -> char {
    let name = name.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if any(&["bud", "pod", "head", "ear", "wh-", "wf-", "jbl", "boat", "beats", "sound", "audio", "speaker"]) {
        '\u{E7F6}'
    } else if any(&["mouse", "mx master", "trackpad"]) {
        '\u{E962}'
    } else if any(&["keyboard", "keys"]) {
        '\u{E765}'
    } else if any(&["controller", "gamepad", "xbox", "dualsense", "joy"]) {
        '\u{E7FC}'
    } else if any(&["phone", "iphone", "galaxy", "pixel", "redmi", "oneplus"]) {
        '\u{E8EA}'
    } else if any(&["watch", "band"]) {
        '\u{E916}'
    } else {
        '\u{E702}'
    }
}
/// The green of a charging light.
const NEON: Color = Color::rgb(0x39ff6a);

pub struct Battery {
    level: Level,
    /// Bluetooth devices connected right now, with battery levels when they report one.
    devices: Vec<(String, Option<u8>)>,
    /// Ask the Bluetooth worker to read the levels again.
    rescan: std::sync::mpsc::Sender<()>,
    scanned: Option<Instant>,
    /// When the battery last came on screen, and when it was last drawn.
    appeared: Instant,
    drawn: Option<Instant>,
}

impl Battery {
    pub fn new(rescan: std::sync::mpsc::Sender<()>) -> Self {
        Self { level: read(), devices: Vec::new(), rescan, scanned: None, appeared: Instant::now(), drawn: None }
    }

    fn charging(&self) -> bool {
        self.level.plugged && self.level.percent < 100
    }

    /// Neon green while charging, plain white (or ink) on battery, amber when low.
    fn color(&self, t: &Theme) -> Color {
        if self.level.plugged {
            if t.dark { NEON } else { t.green }
        } else if self.level.percent <= 20 {
            t.amber
        } else {
            t.text
        }
    }

    /// How far the charge light has faded in, 0..1. It eases in each time the
    /// battery comes on screen and then holds still.
    fn entry(&mut self) -> f32 {
        let now = Instant::now();
        if self.drawn.is_none_or(|at| now.duration_since(at) > Duration::from_millis(400)) {
            self.appeared = now;
        }
        self.drawn = Some(now);
        1.0 - (-now.duration_since(self.appeared).as_secs_f32() * 3.2).exp()
    }

    /// While charging, a pulse of light runs along the filled part of the bar,
    /// from empty up to the current level, then starts again: energy flowing in.
    /// It stays inside the fill and fades out exponentially to both sides.
    fn charge_light(&self, ui: &Ui, track: Rect, radius: f32, edge: f32, strength: f32) {
        if !self.charging() || strength <= 0.01 || edge <= track.x + 2.0 {
            return;
        }
        const RUN: f32 = 1.7;
        const REST: f32 = 0.7;
        let phase = self.appeared.elapsed().as_secs_f32() % (RUN + REST);
        if phase > RUN {
            return;
        }
        // Ease in and out, so the pulse gathers speed and settles at the level.
        let p = phase / RUN;
        let eased = p * p * (3.0 - 2.0 * p);
        let span = edge - track.x;
        let center = track.x + span * eased;
        // Softer as it starts and as it arrives.
        let fade = (p * 6.0).min(1.0) * ((1.0 - p) * 5.0).min(1.0);
        // Inside the bar's rounded shape, and only on the filled part of it.
        ui.g.push_round_clip(track, radius);
        ui.g.push_rect_clip(Rect::new(track.x, track.y, span, track.h));
        // One soft light, drawn with a cached gradient brush: fading out to both sides.
        let reach = (track.h * 2.6).clamp(14.0, 60.0);
        let (_, cy) = track.center();
        ui.g.glow(center, cy, reach, track.h * 2.4, Color::rgb(0xffffff).alpha(strength * fade * 0.85));
        ui.g.pop_rect_clip();
        ui.g.pop_clip();
    }
    fn eyebrow(&self) -> String {
        match (self.level.plugged, self.level.percent, self.level.remaining) {
            (true, 100, _) => "Charged".into(),
            (true, ..) => "Charging...".into(),
            (false, _, Some(secs)) if secs >= 3600 => format!("On battery \u{00B7} {} h {} min left", secs / 3600, (secs / 60) % 60),
            (false, _, Some(secs)) => format!("On battery \u{00B7} {} min left", (secs / 60).max(1)),
            _ => "On battery".into(),
        }
    }
}

impl Activity for Battery {
    fn name(&self) -> &'static str {
        "battery"
    }

    fn compact_width(&self, _g: &Gfx) -> f32 {
        214.0
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let color = self.color(ui.t);
        let mid = r.y + r.h / 2.0;
        let glyph = if self.level.plugged { '\u{E945}' } else { '\u{E83F}' };
        ui.g.icon(glyph, r.x + 7.0, mid, 14.0, color);
        // A thin line for the level; while charging, a pulse runs along it.
        let bar = Rect::new(r.x + 26.0, mid - 1.5, r.w - 70.0, 3.0);
        let fill = (bar.w * f32::from(self.level.percent) / 100.0).max(3.0);
        ui.g.fill_rect(bar, 1.5, ui.t.track);
        ui.g.fill_rect(Rect::new(bar.x, bar.y, fill, bar.h), 1.5, color);
        let entry = self.entry();
        self.charge_light(ui, bar, 1.5, bar.x + fill, 0.9 * entry);
        ui.number(&format!("{}%", self.level.percent), r, 12.5, false, Align::Right, color);
    }

    fn page_size(&self) -> Option<(f32, f32)> {
        self.level.present.then_some(PAGE)
    }

    fn page(&mut self, ui: &mut Ui, r: Rect) {
        let (t, color) = (ui.t, self.color(ui.t));
        let top = r.y + 12.0;
        // What is happening, in one quiet line.
        ui.g.icon(if self.level.plugged { '\u{E945}' } else { '\u{E83F}' }, r.x + 8.0, top + 10.0, 14.0, t.tertiary);
        ui.label(&self.eyebrow(), Rect::new(r.x + 24.0, top, r.w - 24.0, 20.0), 13.5, 500, Align::Left, t.tertiary);
        // The level, as large as the page allows.
        ui.number(&format!("{}%", self.level.percent), Rect::new(r.x - 2.0, top + 22.0, 190.0, 56.0), 50.0, true, Align::Left, t.text);

        // Up to three connected Bluetooth devices, with their battery when they report it.
        let list = Rect::new(r.x + 200.0, top, r.w - 200.0, 76.0);
        let row_h = 19.0;
        ui.label("Connected", Rect::new(list.x, list.y, list.w, 16.0), 11.0, 600, Align::Left, t.tertiary);
        if self.devices.is_empty() {
            ui.label("No Bluetooth devices", Rect::new(list.x, list.y + 18.0, list.w, row_h), 12.0, 500, Align::Left, t.secondary);
        }
        for (row, (name, level)) in self.devices.iter().take(3).enumerate() {
            let y = list.y + 18.0 + row as f32 * row_h;
            ui.g.icon(device_glyph(name), list.x + 7.0, y + row_h / 2.0, 11.0, t.accent);
            ui.label(name, Rect::new(list.x + 20.0, y, list.w - 66.0, row_h), 12.0, 500, Align::Left, t.text);
            if let Some(level) = level {
                let low = *level <= 20;
                ui.number(&format!("{level}%"), Rect::new(list.right() - 44.0, y, 44.0, row_h), 11.5, false, Align::Right, if low { t.amber } else { t.secondary });
            }
        }        // A scale, and a bar of light filling it.
        let track = Rect::new(r.x, top + 104.0, r.w, 26.0);
        for (label, x, align) in
            [("0", track.x, Align::Left), ("50", track.x + track.w / 2.0 - 20.0, Align::Center), ("100", track.right() - 40.0, Align::Right)]
        {
            ui.number(label, Rect::new(x, top + 86.0, 40.0, 14.0), 9.5, false, align, t.tertiary);
        }
        ui.g.fill_rect(track, 10.0, t.control);
        let fill = Rect::new(track.x, track.y, (track.w * f32::from(self.level.percent) / 100.0).max(20.0), track.h);
        // The fill's right end is cut square, a level rather than a pill: draw it
        // wider than needed and clip off the rounding at the edge.
        // At 100% the cut edge meets the track's own rounded end, so the fill is
        // also clipped to the track's shape.
        let entry = self.entry();
        ui.g.push_round_clip(track, 10.0);
        ui.g.push_rect_clip(fill);
        let body = Rect::new(fill.x, fill.y, fill.w + 12.0, fill.h);
        if self.charging() {
            // Charge flows in from the left: deep green rising to a lit neon edge.
            let deep = if t.dark { Color::rgb(0x0c3a1b) } else { Color::rgb(0xbfeccb) };
            let lit = color.mix(deep, 1.0 - (entry * 16.0).round() / 16.0);
            ui.g.fill_rect_gradient(body, 10.0, fill.x, fill.right(), deep, lit);
        } else {
            ui.g.fill_rect(body, 10.0, color);
        }
        ui.g.pop_rect_clip();
        ui.g.pop_clip();
        // Energy flowing in: a pulse runs from empty up to the level, inside the fill.
        self.charge_light(ui, track, 10.0, fill.right(), 0.55 * entry);
    }

    fn refresh(&mut self) {
        self.level = read();
        // Device batteries change on their own: read them again whenever the island
        // opens, but not more than every twenty seconds.
        if self.scanned.is_none_or(|at| at.elapsed() > Duration::from_secs(20)) {
            self.scanned = Some(Instant::now());
            let _ = self.rescan.send(());
        }
    }

    fn on_event(&mut self, event: &Event, _g: &Gfx, t: &Theme) -> Option<Color> {
        if let Event::Bluetooth(devices) = event {
            self.devices = devices.clone();
            return None;
        }
        let Event::Power = event else { return None };
        let (before, now) = (self.level, read());
        self.level = now;
        if !now.present {
            None
        } else if now.plugged != before.plugged {
            // Plugged in or unplugged: worth a glance either way.
            Some(if now.plugged { NEON } else { t.text.alpha(0.6) })
        } else if !now.plugged && now.percent <= 20 && before.percent > 20 {
            Some(t.amber)
        } else {
            None
        }
    }

    fn animating(&self, _expanded: bool) -> bool {
        // The charge pulse runs while charging and on screen.
        self.charging()
    }
}
