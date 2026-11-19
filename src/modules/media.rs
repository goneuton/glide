//! What is playing anywhere on the PC, through Windows' media session API
//! (the same source as the volume flyout): any player that reports to
//! Windows shows up, with artwork and transport controls.
//!
//! A worker thread owns the session objects and sleeps until Windows reports
//! a change, so nothing is polled.

use crate::island::{Activity, Event, PAGE};
use crate::shell::render::{Align, Gfx, Image, Rect};
use crate::shell::ui::{id, Ui};
use crate::theme::{Color, Theme};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{Result, HSTRING};
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Clone, Default)]
pub struct State {
    /// A player is known to Windows (even if paused).
    pub present: bool,
    /// Which app the session belongs to (its application id).
    pub app: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub position_ms: i64,
    pub duration_ms: i64,
    /// When `position_ms` was true.
    pub at: Option<Instant>,
    /// Encoded artwork, sent only when it changes (see `art_serial`).
    pub art: Option<Arc<Vec<u8>>>,
    pub art_serial: u64,
}

pub enum Command {
    Toggle,
    Next,
    Previous,
    /// Jump to a position in milliseconds.
    Seek(i64),
}

/// What Windows said changed. Each kind needs a different (and differently
/// expensive) question asked of the player.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Session,
    Track,
    Playback,
    Position,
}

pub enum Signal {
    Changed(Kind),
    Do(Command),
}

fn ms(ticks: i64) -> i64 {
    ticks / 10_000
}

fn read_art(session: &Session) -> Result<Vec<u8>> {
    let properties = session.TryGetMediaPropertiesAsync()?.get()?;
    let stream = properties.Thumbnail()?.OpenReadAsync()?.get()?;
    let size = stream.Size()? as u32;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(size)?.get()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}

/// Players report position many times a second; the island extrapolates it
/// anyway, so one reading every few seconds is enough.
const POSITION_EVERY: Duration = Duration::from_secs(3);

/// Start the worker. It posts `message` to `hwnd` whenever a new [`State`] is waiting.
pub fn spawn(hwnd: isize, message: u32) -> (Receiver<State>, Sender<Signal>) {
    let (state_tx, state_rx) = channel::<State>();
    let (signal_tx, signal_rx) = channel::<Signal>();
    let wake = signal_tx.clone();
    let _ = std::thread::Builder::new().name("glide-media".into()).spawn(move || {
        let Ok(manager) = Manager::RequestAsync().and_then(|op| op.get()) else { return };
        let changed = |tx: &Sender<Signal>, kind: Kind| {
            let tx = tx.clone();
            move || {
                let _ = tx.send(Signal::Changed(kind));
            }
        };
        let notify = changed(&wake, Kind::Session);
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
            notify();
            Ok(())
        }));
        let mut watched: Option<(Session, HSTRING)> = None;
        let mut state = State::default();
        let (mut art_key, mut art, mut art_serial, mut art_tries) = (String::new(), None::<Arc<Vec<u8>>>, 0u64, 0u8);
        // What needs asking on this pass. Everything, the first time.
        let (mut session_changed, mut track, mut playback, mut position) = (true, true, true, true);
        let mut position_read = Instant::now();
        let mut position_owed = false;
        loop {
            let session = manager.GetCurrentSession().ok();
            let app = session.as_ref().and_then(|s| s.SourceAppUserModelId().ok()).unwrap_or_default();
            // Subscribe to the session once; the registrations die with the old object.
            if watched.as_ref().map(|(_, id)| id) != session.as_ref().map(|_| &app) {
                if let Some(session) = &session {
                    // Position reports are not subscribed to: players send them many times
                    // a second and merely receiving them costs more than everything else
                    // Glide does. The position is read on a slow timer while playing.
                    let (a, b) = (changed(&wake, Kind::Track), changed(&wake, Kind::Playback));
                    let _ = session.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                        a();
                        Ok(())
                    }));
                    let _ = session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                        b();
                        Ok(())
                    }));
                }
                watched = session.clone().map(|s| (s, app.clone()));
                session_changed = true;
            }
            match &session {
                None => state = State::default(),
                Some(session) => {
                    state.present = true;
                    state.app = app.to_string();
                    if session_changed || track {
                        if let Ok(properties) = session.TryGetMediaPropertiesAsync().and_then(|op| op.get()) {
                            state.title = properties.Title().map(|t| t.to_string()).unwrap_or_default();
                            state.artist = properties.Artist().map(|a| a.to_string()).unwrap_or_default();
                        }
                        // Artwork is fetched once per track, not on every update.
                        let key = format!("{}\n{}", state.title, state.artist);
                        if key != art_key {
                            (art_key, art_tries, art) = (key, 0, None);
                            art_serial += 1;
                        }
                    }
                    // Players often publish the title first and the picture a moment later:
                    // ask a few more times, then accept that this track has none.
                    if art.is_none() && art_tries < 4 {
                        art_tries += 1;
                        art = read_art(session).ok().filter(|b| !b.is_empty()).map(Arc::new);
                        if art.is_some() {
                            art_serial += 1;
                        }
                    }
                    if session_changed || playback {
                        state.playing =
                            session.GetPlaybackInfo().and_then(|info| info.PlaybackStatus()).is_ok_and(|s| s == Status::Playing);
                    }
                    if session_changed || position || playback || track {
                        if let Ok(timeline) = session.GetTimelineProperties() {
                            state.position_ms = timeline.Position().map_or(0, |p| ms(p.Duration));
                            let (start, end) = (timeline.StartTime().map_or(0, |t| t.Duration), timeline.EndTime().map_or(0, |t| t.Duration));
                            state.duration_ms = ms(end - start);
                            state.at = Some(Instant::now());
                        }
                        position_read = Instant::now();
                    }
                    (state.art, state.art_serial) = (art.clone(), art_serial);
                }
            }
            // While something plays, come back for the position every few seconds.
            position_owed = state.present && state.playing;
            if state_tx.send(state.clone()).is_err() {
                return;
            }
            // SAFETY: posting to a window handle; a stale handle only makes the call fail.
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), message, WPARAM(0), LPARAM(0));
            }

            // Sleep until Windows or the UI has something worth acting on.
            loop {
                (session_changed, track, playback, position) = (false, false, false, false);
                let first = if position_owed {
                    // A position report was set aside: read it once the interval is up.
                    match signal_rx.recv_timeout(POSITION_EVERY.saturating_sub(position_read.elapsed())) {
                        Ok(signal) => Some(signal),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                } else {
                    match signal_rx.recv() {
                        Ok(signal) => Some(signal),
                        Err(_) => return,
                    }
                };
                if first.is_none() {
                    position = true;
                }
                for signal in first.into_iter().chain(signal_rx.try_iter()) {
                    match signal {
                        Signal::Changed(Kind::Session) => session_changed = true,
                        Signal::Changed(Kind::Track) => track = true,
                        Signal::Changed(Kind::Playback) => playback = true,
                        Signal::Changed(Kind::Position) => position = true,
                        Signal::Do(command) => {
                            if let Some(session) = &session {
                                let _ = match command {
                                    Command::Toggle => session.TryTogglePlayPauseAsync().map(|_| ()),
                                    Command::Next => session.TrySkipNextAsync().map(|_| ()),
                                    Command::Previous => session.TrySkipPreviousAsync().map(|_| ()),
                                    Command::Seek(to) => session.TryChangePlaybackPositionAsync(to * 10_000).map(|_| ()),
                                };
                            }
                            // The player will report the result; make sure it is read.
                            playback = true;
                        }
                    }
                }
                let only_position = position && !(session_changed || track || playback);
                if only_position && position_read.elapsed() < POSITION_EVERY {
                    continue;
                }
                break;
            }
        }
    });
    (state_rx, signal_tx)
}

pub fn format_time(ms: i64) -> String {
    let total = (ms.max(0) / 1000) as u64;
    let (h, m, s) = (total / 3600, (total / 60) % 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// How loud the PC's output is right now, from Windows' own level meter.
/// This is what makes the waveform follow the music instead of faking it.
struct Meter {
    info: Option<windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation>,
}

impl Meter {
    fn new() -> Self {
        use windows::Win32::Media::Audio::{eMultimedia, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
        // SAFETY: standard COM activation on the UI thread.
        let info = unsafe {
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .and_then(|devices| devices.GetDefaultAudioEndpoint(eRender, eMultimedia))
                .and_then(|device| device.Activate(CLSCTX_ALL, None))
                .ok()
        };
        Self { info }
    }

    fn peak(&self) -> f32 {
        // SAFETY: a value-returning call on a live interface.
        self.info.as_ref().and_then(|info| unsafe { info.GetPeakValue() }.ok()).unwrap_or(0.0)
    }
}

/// How strongly the page's cover backdrop is blurred: 0 off, 1 light, 2 medium, 3 heavy.
pub static COVER_BLUR: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

/// The backdrop is the cover decoded at this many pixels across and stretched:
/// the fewer pixels, the softer the picture, at no cost per frame.
fn backdrop_size(blur: u8) -> f32 {
    match blur {
        0 => 220.0,
        1 => 30.0,
        2 => 12.0,
        _ => 6.0,
    }
}

/// Number of recent level samples kept: one per line of the large waveform.
const HISTORY: usize = 56;

pub struct Media {
    state: State,
    art: Option<Image>,
    /// The artwork decoded at a few pixels across: stretched, it is a free blur.
    backdrop: Option<Image>,
    /// The encoded cover, kept so the backdrop can be rebuilt when the blur setting changes.
    art_bytes: Option<Arc<Vec<u8>>>,
    blur: u8,
    born: Instant,
    art_serial: u64,
    commands: Sender<Signal>,
    meter: Meter,
    levels: std::collections::VecDeque<f32>,
    smoothed: f32,
    /// Recent maximum level, for automatic gain.
    loudest: f32,
    sampled: Option<Instant>,
}

impl Media {
    pub fn new(commands: Sender<Signal>) -> Self {
        Self {
            state: State::default(),
            art: None,
            backdrop: None,
            art_bytes: None,
            blur: 2,
            born: Instant::now(),
            art_serial: 0,
            commands,
            meter: Meter::new(),
            levels: std::iter::repeat_n(0.0, HISTORY).collect(),
            smoothed: 0.0,
            loudest: 0.004,
            sampled: None,
        }
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(Signal::Do(command));
    }

    /// Position right now: the last report plus the time that has passed while playing.
    fn position(&self) -> i64 {
        let elapsed = match (self.state.playing, self.state.at) {
            (true, Some(at)) => at.elapsed().as_millis() as i64,
            _ => 0,
        };
        let now = self.state.position_ms + elapsed;
        if self.state.duration_ms > 0 { now.min(self.state.duration_ms) } else { now }
    }

    fn toggle(&mut self) {
        self.send(Command::Toggle);
        // Show the new state at once; Windows confirms a moment later.
        self.state.position_ms = self.position();
        self.state.at = Some(Instant::now());
        self.state.playing = !self.state.playing;
    }

    /// Take one level sample per drawn frame (about 30 a second) into the history.
    fn sample(&mut self) {
        if self.sampled.is_some_and(|at| at.elapsed() < Duration::from_millis(28)) {
            return;
        }
        self.sampled = Some(Instant::now());
        let raw = if self.state.playing { self.meter.peak().clamp(0.0, 1.0) } else { 0.0 };
        // Quiet or loud, the shape should fill the space: follow the recent maximum.
        self.loudest = (self.loudest * 0.995).max(raw).max(0.004);
        let peak = (raw / self.loudest).powf(0.8);
        // Rise fast, fall slower: reads as sound, not as noise.
        self.smoothed = if peak > self.smoothed { peak } else { self.smoothed * 0.82 + peak * 0.18 };
        self.levels.pop_front();
        self.levels.push_back(self.smoothed);
    }

    fn cover(&self, ui: &Ui, r: Rect, radius: f32) {
        match &self.art {
            Some(image) => ui.g.image(image, r, radius),
            None => {
                ui.g.fill_rect(r, radius, ui.t.control_hover);
                let (cx, cy) = r.center();
                ui.g.icon('\u{E8D6}', cx, cy, r.w * 0.42, ui.t.secondary);
            }
        }
    }

    /// Bars centred on `mid`, newest level in the middle, fading towards the ends.
    fn waveform(&self, ui: &Ui, x: f32, mid: f32, width: f32, height: f32, bars: usize, thickness: f32, color: Color) {
        let step = width / bars as f32;
        for i in 0..bars {
            // Distance from the centre, 0..1: the middle shows the newest sample.
            let from_centre = ((i as f32 + 0.5) / bars as f32 - 0.5).abs() * 2.0;
            let age = (from_centre * (HISTORY as f32 - 1.0)) as usize;
            let level = self.levels[HISTORY - 1 - age.min(HISTORY - 1)];
            let envelope = 1.0 - from_centre * from_centre * 0.75;
            let h = (thickness + level * envelope * height).min(height);
            let bar = Rect::new(x + step * i as f32 + (step - thickness) / 2.0, mid - h / 2.0, thickness, h);
            ui.g.fill_rect(bar, thickness / 2.0, color.alpha(1.0 - from_centre * 0.55));
        }
    }
}

impl Activity for Media {
    fn name(&self) -> &'static str {
        "media"
    }

    fn priority(&self) -> u8 {
        if self.state.present && self.state.playing { 50 } else { 0 }
    }

    fn compact_width(&self, g: &Gfx) -> f32 {
        let _ = g;
        196.0
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let mid = r.y + r.h / 2.0;
        self.sample();
        // Cover, the sound itself, and the one control that matters.
        self.cover(ui, Rect::new(r.x - 3.0, mid - 12.0, 24.0, 24.0), 12.0);
        if self.state.present {
            self.waveform(ui, r.x + 34.0, mid, r.w - 76.0, 20.0, 15, 2.6, t.accent);
        } else {
            ui.label("Nothing playing", Rect::new(r.x + 30.0, r.y, r.w - 60.0, r.h), 12.5, 500, Align::Left, t.secondary);
        }
        let (bx, radius) = (r.right() - 9.0, 12.0);
        let hit = ui.hit(id("media.pill.play"), Rect::new(bx - radius, mid - radius, radius * 2.0, radius * 2.0));
        let scale = 1.0 + 0.08 * hit.hover - 0.12 * hit.press;
        ui.g.fill_circle(bx, mid, radius * scale, t.accent);
        ui.play_pause(id("media.pill.glyph"), bx, mid, 6.0 * scale, self.state.playing, t.on_accent);
        if hit.clicked && self.state.present {
            self.toggle();
        }
    }

    fn page_size(&self) -> Option<(f32, f32)> {
        Some(PAGE)
    }

    fn full_bleed(&self) -> bool {
        // The blurred cover fills the page edge to edge.
        self.state.present
    }

    fn page(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        if !self.state.present {
            ui.g.icon('\u{E8D6}', r.x + r.w / 2.0, r.y + 62.0, 30.0, t.tertiary);
            ui.label("Nothing playing", Rect::new(r.x, r.y + 90.0, r.w, 22.0), 15.0, 600, Align::Center, t.secondary);
            return;
        }
        self.sample();
        // Follow the blur setting.
        let blur = COVER_BLUR.load(std::sync::atomic::Ordering::Relaxed);
        if blur != self.blur {
            self.blur = blur;
            self.backdrop = self.art_bytes.as_ref().and_then(|bytes| ui.g.load_image(bytes, backdrop_size(blur)));
        }
        // The cover, blurred, fills the whole page under a dark tinted veil, so
        // white text reads on any artwork and in either theme.
        let (white, dim) = (Color::rgb(0xffffff), Color::rgb(0xffffff).alpha(0.72));
        let full = Rect::new(r.x - 60.0, -4.0, r.w + 120.0, PAGE.1 + 8.0);
        if let Some(backdrop) = &self.backdrop {
            ui.g.image(backdrop, full, 0.0);
        }
        ui.g.fill_rect(full, 0.0, Color::rgb(0x0a0a0c).mix(t.accent, 0.10).alpha(if self.backdrop.is_some() { 0.62 } else { 1.0 }));

        // Two slow lights drift behind everything while it plays: the page breathes.
        if self.state.playing {
            let s = self.born.elapsed().as_secs_f32();
            let (mx, my) = (r.x + r.w / 2.0, PAGE.1 / 2.0);
            ui.g.glow(mx + (s * 0.31).sin() * 130.0, my + (s * 0.23).cos() * 34.0, 150.0, 96.0, t.accent.alpha(0.16));
            ui.g.glow(mx - (s * 0.19).sin() * 150.0, my - (s * 0.29).cos() * 40.0, 130.0, 84.0, white.alpha(0.07));
        }

        // Who: a small chip, top left.
        let artist = if self.state.artist.is_empty() { "Unknown artist" } else { &self.state.artist };
        let chip_w = (ui.g.measure(artist, 12.0, 600) + 48.0).min(r.w * 0.6);
        let chip = Rect::new(r.x, r.y + 12.0, chip_w, 28.0);
        ui.g.fill_rect(chip, 14.0, Color::rgb(0x000000).alpha(0.38));
        self.cover(ui, Rect::new(chip.x + 3.0, chip.y + 3.0, 22.0, 22.0), 11.0);
        ui.label(artist, Rect::new(chip.x + 32.0, chip.y, chip.w - 40.0, chip.h), 12.0, 600, Align::Left, white);

        // The sound: fine lines, tallest where the newest sample is.
        let wave_mid = r.y + 64.0;
        if self.state.playing {
            ui.g.glow(r.x + r.w / 2.0, wave_mid, r.w * 0.36, 30.0, white.alpha(0.10));
        }
        self.waveform(ui, r.x + 8.0, wave_mid, r.w - 16.0, 40.0, HISTORY, 1.6, white);

        let title = if self.state.title.is_empty() { "Unknown title" } else { &self.state.title };
        ui.label(title, Rect::new(r.x, r.y + 88.0, r.w, 20.0), 14.5, 600, Align::Left, white);

        // Timeline: elapsed, a line, time remaining.
        let (position, duration) = (self.position(), self.state.duration_ms);
        let line_y = r.y + 120.0;
        ui.number(&format_time(position), Rect::new(r.x, line_y - 8.0, 40.0, 16.0), 10.0, false, Align::Left, dim);
        let remaining = if duration > 0 { format!("-{}", format_time(duration - position)) } else { "--:--".into() };
        ui.number(&remaining, Rect::new(r.right() - 46.0, line_y - 8.0, 46.0, 16.0), 10.0, false, Align::Right, dim);
        let bar = Rect::new(r.x + 40.0, line_y - 2.0, r.w - 88.0, 4.0);
        let fraction = if duration > 0 { position as f32 / duration as f32 } else { 0.0 };
        let hit = ui.hit(id("media.seek"), bar.inset(0.0, -8.0));
        let thick = 4.0 + 2.0 * hit.hover;
        let line = Rect::new(bar.x, line_y - thick / 2.0, bar.w, thick);
        ui.g.fill_rect(line, thick / 2.0, white.alpha(0.28));
        ui.g.fill_rect(Rect::new(line.x, line.y, (line.w * fraction.clamp(0.0, 1.0)).max(thick), thick), thick / 2.0, white);
        if hit.clicked && duration > 0 {
            let to = ((ui.input.x - bar.x) / bar.w).clamp(0.0, 1.0);
            self.send(Command::Seek((to * duration as f32) as i64));
            (self.state.position_ms, self.state.at) = ((to * duration as f32) as i64, Some(Instant::now()));
        }

        // Transport: three dark discs.
        let (cx, cy) = (r.x + r.w / 2.0, r.y + 144.0);
        let playing = self.state.playing;
        let disc = |ui: &mut Ui, name: u32, glyph: Option<char>, x: f32, radius: f32| -> bool {
            let hit = ui.hit(name, Rect::new(x - radius, cy - radius, radius * 2.0, radius * 2.0));
            let scale = 1.0 + 0.08 * hit.hover - 0.12 * hit.press;
            ui.g.fill_circle(x, cy, radius * scale, Color::rgb(0x000000).alpha(0.42 + 0.2 * hit.hover));
            match glyph {
                Some(glyph) => ui.g.icon(glyph, x, cy, radius * 0.86 * scale, white),
                None => ui.play_pause(name ^ 1, x, cy, radius * 0.5 * scale, playing, white),
            }
            hit.clicked
        };
        if disc(ui, id("media.prev"), Some('\u{E892}'), cx - 46.0, 13.0) {
            self.send(Command::Previous);
        }
        if disc(ui, id("media.play"), None, cx, 15.0) {
            self.toggle();
        }
        if disc(ui, id("media.next"), Some('\u{E893}'), cx + 46.0, 13.0) {
            self.send(Command::Next);
        }
    }

    fn on_event(&mut self, event: &Event, g: &Gfx, t: &Theme) -> Option<Color> {
        if let Event::AudioDevice = event {
            // Sound moved to another device (headphones connected): meter that one.
            self.meter = Meter::new();
            return None;
        }
        let Event::Media(state) = event else { return None };
        // A WhatsApp call registers with Windows as if it were a player. It is not
        // music: the call module shows it, and media carries on as if nothing played.
        if crate::modules::call::is_whatsapp(&state.app) {
            if crate::modules::call::is_whatsapp(&self.state.app) {
                self.state = State::default();
            }
            return None;
        }
        let new_track = state.present && !state.title.is_empty() && state.title != self.state.title;
        if state.art_serial != self.art_serial {
            self.art = state.art.as_ref().and_then(|bytes| g.load_image(bytes, 64.0));
            self.art_bytes = state.art.clone();
            self.blur = COVER_BLUR.load(std::sync::atomic::Ordering::Relaxed);
            self.backdrop = state.art.as_ref().and_then(|bytes| g.load_image(bytes, backdrop_size(self.blur)));
            self.art_serial = state.art_serial;
        }
        self.state = state.clone();
        // A new track announces itself; position updates do not.
        (new_track && state.playing).then_some(t.accent)
    }

    fn heartbeat(&self, expanded: bool) -> Option<Duration> {
        // A paused page still shows a time; a playing one redraws anyway.
        (expanded && self.state.present && !self.state.playing).then_some(Duration::from_secs(5))
    }

    fn animating(&self, _expanded: bool) -> bool {
        // The waveform runs while there is sound, and lets its last bars settle after.
        self.state.present && (self.state.playing || self.smoothed > 0.01)
    }
}
