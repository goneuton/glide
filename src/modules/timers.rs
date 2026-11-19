//! Timer (with a pomodoro), stopwatch and one daily alarm.
//!
//! Nothing here ticks in the background: a running timer is just a deadline,
//! and the island asks Windows to wake it at that moment (and 10 and 5 seconds
//! before, for the warnings).

use crate::island::{Activity, PAGE};
use crate::settings::data_dir;
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::ui::{figures, figures_width, id, Magnet, Roll, Ui, COLON};
use crate::theme::{Color, Theme};
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_LOOP, SND_NODEFAULT};
use windows::Win32::System::SystemInformation::GetLocalTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Timer,
    Stopwatch,
    Alarm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ringing {
    Timer,
    Alarm,
}

/// A pomodoro is one focus block of this length.
const POMODORO: Duration = Duration::from_secs(25 * 60);
/// How long the icon shakes for a warning.
const SHAKE: Duration = Duration::from_millis(1200);

pub struct Timers {
    tab: Tab,
    /// Length the timer was last set to.
    length: Duration,
    /// Running: when it ends. Paused: what is left.
    timer_end: Option<Instant>,
    timer_left: Duration,
    /// The timer ran out at this moment and has not been stopped: overtime counts from here.
    overdue_since: Option<Instant>,
    /// The timer is a pomodoro: tomato in the pill, everything in red.
    pomodoro: bool,
    /// Warnings already given for this run: 1 after 10 s left, 2 after 5 s.
    warned: u8,
    /// Rings pulsing out from the pill: colour, start, how long.
    pulse: Option<(Color, Instant, Duration)>,
    /// The icon shakes like a ringing bell, since then.
    shake: Option<Instant>,
    /// Started from the open page: the pill shows once when the page closes.
    fresh: bool,
    /// Scrolling over the timer or alarm: a magnetic strip of values, and
    /// whether it is the right-hand part (seconds, minutes) that moves.
    magnet: Magnet,
    magnet_right: bool,
    /// The two parts of the timer or alarm either side of the colon.
    left: Roll,
    right: Roll,
    stopwatch_start: Option<Instant>,
    stopwatch_banked: Duration,
    /// (hour, minute, on)
    alarm: (u16, u16, bool),
    /// The minute the alarm last rang in, so it rings once.
    alarm_rang: Option<(u16, u16, u16)>,
    ringing: Option<Ringing>,
    /// The big number on the page, and the value on the pill.
    big: Roll,
    small: Roll,
}

fn clock(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

fn ring(on: bool) {
    // SAFETY: plays (or stops) a system sound asynchronously.
    unsafe {
        if on {
            let _ = PlaySoundW(w!("C:\\Windows\\Media\\Alarm01.wav"), None, SND_FILENAME | SND_ASYNC | SND_LOOP | SND_NODEFAULT);
        } else {
            let _ = PlaySoundW(None, None, SND_ASYNC);
        }
    }
}

/// A small tomato: red body, a green star of a calyx and a stem.
fn tomato(g: &Gfx, cx: f32, cy: f32, s: f32, red: Color) {
    let green = Color::rgb(0x34c759);
    g.fill_circle(cx, cy + s * 0.14, s * 0.84, red);
    g.fill_circle(cx - s * 0.32, cy - s * 0.06, s * 0.2, Color::rgb(0xffffff).alpha(0.32));
    let (lx, ly) = (cx, cy - s * 0.6);
    let star: Vec<(f32, f32)> = (0..10)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
            let r = if i % 2 == 0 { s * 0.5 } else { s * 0.16 };
            (lx + r * a.cos(), ly + r * a.sin() * 0.55)
        })
        .collect();
    g.fill_poly(&star, green);
    g.line((lx, ly), (lx + s * 0.14, ly - s * 0.42), green, (s * 0.2).max(1.2));
}

impl Timers {
    pub fn new() -> Self {
        // The alarm survives restarts: "07:30 on" in a one-line file.
        let saved = data_dir().and_then(|d| std::fs::read_to_string(d.join("alarm.txt")).ok()).unwrap_or_default();
        let mut parts = saved.split_whitespace();
        let time = parts.next().and_then(|t| t.split_once(':')).and_then(|(h, m)| Some((h.parse().ok()?, m.parse().ok()?)));
        let (hour, minute) = time.unwrap_or((7, 0));
        let length = Duration::from_secs(25 * 60);
        Self {
            tab: Tab::Timer,
            length,
            timer_end: None,
            timer_left: length,
            overdue_since: None,
            pomodoro: false,
            warned: 0,
            pulse: None,
            shake: None,
            fresh: false,
            magnet: Magnet::default(),
            magnet_right: false,
            left: Roll::new("25"),
            right: Roll::new("00"),
            stopwatch_start: None,
            stopwatch_banked: Duration::ZERO,
            alarm: (hour % 24, minute % 60, parts.next() == Some("on")),
            alarm_rang: None,
            ringing: None,
            big: Roll::new(&clock(length)),
            small: Roll::new(&clock(length)),
        }
    }

    fn save_alarm(&self) {
        if let Some(dir) = data_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let text = format!("{:02}:{:02} {}", self.alarm.0, self.alarm.1, if self.alarm.2 { "on" } else { "off" });
            let _ = std::fs::write(dir.join("alarm.txt"), text);
        }
    }

    /// The timer's colour: red for a pomodoro, the accent otherwise.
    fn tint(&self, t: &Theme) -> Color {
        if self.pomodoro { t.red } else { t.accent }
    }

    fn remaining(&self) -> Duration {
        self.timer_end.map_or(self.timer_left, |end| end.saturating_duration_since(Instant::now()))
    }

    fn overtime(&self) -> Duration {
        self.overdue_since.map_or(Duration::ZERO, |at| at.elapsed())
    }

    fn elapsed(&self) -> Duration {
        self.stopwatch_banked + self.stopwatch_start.map_or(Duration::ZERO, |start| start.elapsed())
    }

    fn set_timer(&mut self, length: Duration) {
        (self.length, self.timer_left, self.timer_end) = (length, length, None);
    }

    /// Run (or resume) the timer from what is left.
    fn start(&mut self) {
        if self.timer_left.is_zero() {
            return;
        }
        let left = self.timer_left.as_secs_f32();
        // Warnings already behind us are not given.
        self.warned = if left <= 5.5 { 2 } else if left <= 10.5 { 1 } else { 0 };
        (self.timer_end, self.fresh) = (Some(Instant::now() + self.timer_left), true);
    }

    fn dismiss(&mut self) {
        if self.ringing.take().is_some() {
            ring(false);
        }
        (self.overdue_since, self.pulse) = (None, None);
    }

    fn pulse(&mut self, color: Color, length: Duration) {
        self.pulse = Some((color, Instant::now(), length));
    }

    /// When the alarm next goes off, as a point in monotonic time.
    fn alarm_deadline(&self) -> Option<Instant> {
        if !self.alarm.2 {
            return None;
        }
        // SAFETY: plain value-returning call.
        let now = unsafe { GetLocalTime() };
        let now_secs = i64::from(now.wHour) * 3600 + i64::from(now.wMinute) * 60 + i64::from(now.wSecond);
        let at = i64::from(self.alarm.0) * 3600 + i64::from(self.alarm.1) * 60;
        let mut wait = at - now_secs;
        if wait < 0 || (wait == 0 && self.alarm_rang == Some((now.wDay, self.alarm.0, self.alarm.1))) {
            wait += 24 * 3600;
        }
        Some(Instant::now() + Duration::from_secs(wait.max(0) as u64))
    }

    /// The next warning, 10 and then 5 seconds before the end.
    fn warning_at(&self) -> Option<Instant> {
        let end = self.timer_end?;
        match self.warned {
            0 => end.checked_sub(Duration::from_secs(10)),
            1 => end.checked_sub(Duration::from_secs(5)),
            _ => None,
        }
    }

    /// The pill's icon: a tomato for a pomodoro, else `glyph`. While a warning
    /// is fresh it shakes about its bottom centre like a ringing bell.
    fn glyph(&self, ui: &Ui, glyph: char, x: f32, y: f32, size: f32, color: Color, timer: bool) {
        let angle = self.shake.map_or(0.0, |at| {
            let s = at.elapsed().as_secs_f32();
            let left = 1.0 - s / SHAKE.as_secs_f32();
            if left <= 0.0 { 0.0 } else { 16.0 * (s * 38.0).sin() * left }
        });
        ui.g.rotated(angle, x, y + size * 0.55, || {
            if timer && self.pomodoro {
                tomato(ui.g, x, y, size * 0.5, color);
            } else {
                ui.g.icon(glyph, x, y, size, color);
            }
        });
    }

    /// The accent card on the right: a ring of progress around one round button.
    /// Returns true when the button was clicked.
    fn dial(&self, ui: &mut Ui, card: Rect, fraction: f32, glyph: Result<char, bool>, active: bool, tint: Color) -> bool {
        let t = ui.t;
        ui.g.fill_rect(card, 24.0, if active { tint } else { t.control });
        let (cx, cy) = card.center();
        let radius = card.w.min(card.h) / 2.0 - 16.0;
        let ink = if active { self.on(t) } else { t.secondary };
        ui.g.ring(cx, cy, radius, 9.0, 1.0, ink.alpha(0.2));
        ui.g.ring(cx, cy, radius, 9.0, fraction, ink);
        let hit = ui.hit(id("timers.dial"), card);
        let scale = 1.0 + 0.05 * hit.hover - 0.1 * hit.press;
        ui.g.fill_circle(cx, cy, (radius - 14.0) * scale, t.body);
        let ink = if active { tint } else { t.text };
        match glyph {
            Ok(glyph) => ui.g.icon(glyph, cx, cy, 17.0 * scale, ink),
            // Running or not: a solid triangle that becomes two bars.
            Err(running) => ui.play_pause(id("timers.dial.glyph"), cx, cy, 10.0 * scale, running, ink),
        }
        hit.clicked
    }

    /// Ink on top of the tint.
    fn on(&self, t: &Theme) -> Color {
        if self.pomodoro { Color::rgb(0xffffff) } else { t.on_accent }
    }

    fn chip(ui: &mut Ui, name: u32, label: &str, r: Rect, on: bool, tint: Color, ink: Color) -> bool {
        let t = ui.t;
        let hit = ui.hit(name, r);
        let r = r.scaled(1.0 + 0.03 * hit.hover - 0.06 * hit.press);
        ui.g.fill_rect(r, r.h / 2.0, if on { tint } else { t.control.mix(t.control_hover, hit.hover) });
        ui.label(label, r, 12.0, 600, Align::Center, if on { ink } else { t.text });
        hit.clicked
    }

    /// Scrolling over `big` while it may be changed: which part (left or right
    /// of `split`) follows the swipe, and how many whole steps it clicked over.
    fn scrolled(&mut self, ui: &mut Ui, big: Rect, split: f32, enabled: bool) -> i32 {
        let over = enabled && ui.input.inside && big.contains(ui.input.x, ui.input.y);
        let delta = if over { -ui.input.wheel } else { 0.0 };
        // The part under the pointer is chosen when a swipe starts, not halfway through.
        if delta != 0.0 && !self.magnet.active() {
            self.magnet_right = ui.input.x > split;
        }
        self.magnet.update(ui, delta)
    }

    /// Soft top and bottom edges over the number, so digits melt away.
    fn edges(ui: &Ui, clip: Rect) {
        let clear = ui.t.body.alpha(0.0);
        let top = Rect::new(clip.x, clip.y, clip.w, 14.0);
        ui.g.fill_rect_gradient_v(top, 0.0, top.y, top.bottom(), ui.t.body, clear);
        let bottom = Rect::new(clip.x, clip.bottom() - 14.0, clip.w, 14.0);
        ui.g.fill_rect_gradient_v(bottom, 0.0, bottom.y, bottom.bottom(), clear, ui.t.body);
    }

    /// The big number, rolling; digits slide in and out under soft edges.
    fn draw_big(&mut self, ui: &mut Ui, big: Rect, text: &str, up: bool, color: Color) {
        self.big.set(text, up);
        let clip = Rect::new(big.x - 4.0, big.y - 6.0, big.w + 8.0, big.h + 12.0);
        self.big.draw(ui, big, 44.0, true, Align::Left, color, 40.0, clip, false);
        Self::edges(ui, clip);
    }

    /// Timer and alarm: two parts either side of a colon. `at(left, right)` is
    /// the text of both parts with each moved that many steps (None past a limit).
    /// The part being scrolled is a strip of neighbouring values that follows
    /// the swipe; the other rolls when it changes.
    fn draw_split(&mut self, ui: &mut Ui, big: Rect, at: &dyn Fn(i32, i32) -> Option<(String, String)>, up: bool, color: Color) {
        const SIZE: f32 = 44.0;
        const TRAVEL: f32 = 46.0;
        let Some((left, right)) = at(0, 0) else { return };
        let left_w = ui.g.measure_number(&left, SIZE);
        // The monospaced colon is closed up to part of a cell, glyph centred in it.
        let cell = ui.g.measure_number(":", SIZE);
        let colon_w = cell * COLON;
        let clip = Rect::new(big.x - 4.0, big.y - 6.0, big.w + 8.0, big.h + 12.0);
        ui.number(":", Rect::new(big.x + left_w - (cell - colon_w) / 2.0, big.y, cell + 4.0, big.h), SIZE, true, Align::Left, color);
        let strip = self.magnet.active().then_some((self.magnet_right, self.magnet.pos));
        for is_right in [false, true] {
            let (text, x) = if is_right { (&right, big.x + left_w + colon_w) } else { (&left, big.x) };
            let rect = Rect::new(x, big.y, SIZE * 3.0, big.h);
            let roll = if is_right { &mut self.right } else { &mut self.left };
            match strip {
                Some((side, pos)) if side == is_right => {
                    roll.snap(text);
                    ui.g.push_rect_clip(clip);
                    for k in -2..=2 {
                        let dy = (k as f32 - pos) * TRAVEL;
                        if dy.abs() > TRAVEL * 1.4 {
                            continue;
                        }
                        let moved = if is_right { at(0, k) } else { at(k, 0) };
                        if let Some((l, r)) = moved {
                            let rect = Rect::new(rect.x, rect.y + dy, rect.w, rect.h);
                            ui.number(if is_right { &r } else { &l }, rect, SIZE, true, Align::Left, color);
                        }
                    }
                    ui.g.pop_rect_clip();
                }
                _ => {
                    roll.set(text, up);
                    roll.draw(ui, rect, SIZE, true, Align::Left, color, 40.0, clip, false);
                }
            }
        }
        Self::edges(ui, clip);
    }
}

impl Activity for Timers {
    fn name(&self) -> &'static str {
        "timers"
    }

    fn priority(&self) -> u8 {
        if self.ringing.is_some() {
            90
        } else if self.timer_end.is_some() || self.stopwatch_start.is_some() {
            40
        } else {
            0
        }
    }

    fn stays(&self) -> bool {
        // A ringing alarm and a running stopwatch keep the pill out; a timer
        // (running or over time) only peeks, then rests in the lip.
        self.ringing == Some(Ringing::Alarm) || (self.ringing.is_none() && self.timer_end.is_none())
    }

    fn compact_width(&self, _g: &Gfx) -> f32 {
        if self.ringing.is_some() { 250.0 } else { 186.0 }
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let tint = self.tint(t);
        let mid = r.y + r.h / 2.0;
        match self.ringing {
            Some(what) => {
                let timer = what == Ringing::Timer;
                self.glyph(ui, '\u{E916}', r.x + 8.0, mid, 15.0, tint, timer);
                if timer {
                    // Over time: counting up, below zero.
                    self.small.set(&format!("-{}", clock(self.overtime())), true);
                    let value = Rect::new(r.x + 26.0, r.y, r.w - 100.0, r.h);
                    self.small.draw(ui, value, 13.5, true, Align::Left, tint, 20.0, r, true);
                } else {
                    ui.label("Alarm", Rect::new(r.x + 26.0, r.y, r.w - 100.0, r.h), 13.5, 600, Align::Left, t.text);
                }
                if Self::chip(ui, id("timers.dismiss.compact"), "Stop", Rect::new(r.right() - 58.0, mid - 12.0, 58.0, 24.0), true, tint, self.on(t)) {
                    self.dismiss();
                }
            }
            None => {
                // Whatever is running; otherwise what the page would show.
                let (glyph, value, up, timer) = if self.timer_end.is_some() || (self.stopwatch_start.is_none() && self.tab == Tab::Timer) {
                    ('\u{E916}', clock(self.remaining()), false, true)
                } else if self.stopwatch_start.is_some() || self.tab == Tab::Stopwatch {
                    ('\u{E917}', clock(self.elapsed()), true, false)
                } else {
                    ('\u{EA8F}', format!("{:02}:{:02}", self.alarm.0, self.alarm.1), true, false)
                };
                let color = if timer { tint } else { t.accent };
                self.glyph(ui, glyph, r.x + 8.0, mid, 15.0, color, timer);
                self.small.set(&value, up);
                self.small.draw(ui, r, 13.5, true, Align::Right, color, 20.0, r, true);
            }
        }
    }

    fn page_size(&self) -> Option<(f32, f32)> {
        Some(PAGE)
    }

    fn page(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let (tint, ink) = (self.tint(t), self.on(t));
        let top = r.y + 12.0;
        let card = Rect::new(r.right() - 134.0, top, 134.0, 134.0);
        let left = Rect::new(r.x, top, r.w - card.w - 14.0, 134.0);

        // Which tool: three small chips.
        let tabs = [(Tab::Timer, "Timer", 58.0), (Tab::Stopwatch, "Stopwatch", 84.0), (Tab::Alarm, "Alarm", 58.0)];
        let mut x = left.x;
        for (i, (tab, label, w)) in tabs.into_iter().enumerate() {
            if Self::chip(ui, id("timers.tab") ^ i as u32, label, Rect::new(x, left.y, w, 24.0), self.tab == tab, t.accent, t.on_accent) {
                self.tab = tab;
            }
            x += w + 6.0;
        }
        let big = Rect::new(left.x, left.y + 32.0, left.w, 56.0);
        let row = left.y + 106.0;

        if self.ringing == Some(Ringing::Alarm) {
            ui.label("Alarm", big, 36.0, 600, Align::Left, t.accent);
            if Self::chip(ui, id("timers.dismiss"), "Stop", Rect::new(left.x, row, 110.0, 28.0), true, t.accent, t.on_accent)
                | self.dial(ui, card, 1.0, Ok('\u{EA8F}'), true, t.accent)
            {
                self.dismiss();
            }
            return;
        }
        if self.ringing == Some(Ringing::Timer) {
            self.tab = Tab::Timer;
            let text = format!("-{}", clock(self.overtime()));
            self.draw_big(ui, big, &text, true, tint);
            if Self::chip(ui, id("timers.dismiss"), "Stop", Rect::new(left.x, row, 110.0, 28.0), true, tint, ink)
                | self.dial(ui, card, 1.0, Ok('\u{EA8F}'), true, tint)
            {
                self.dismiss();
            }
            return;
        }
        match self.tab {
            Tab::Timer => {
                let running = self.timer_end.is_some();
                // A pomodoro: the tomato beside the tabs.
                let button = Rect::new(left.right() - 30.0, left.y, 30.0, 24.0);
                let hit = ui.hit(id("timers.pomodoro"), button);
                let b = button.scaled(1.0 + 0.04 * hit.hover - 0.08 * hit.press);
                ui.g.fill_rect(b, b.h / 2.0, if self.pomodoro { t.red.alpha(0.22) } else { t.control.mix(t.control_hover, hit.hover) });
                tomato(ui.g, b.center().0, b.center().1 + 0.5, 7.0, t.red);
                if hit.clicked {
                    if self.pomodoro {
                        self.pomodoro = false;
                    } else {
                        self.pomodoro = true;
                        self.set_timer(POMODORO);
                        self.start();
                    }
                }
                // Up and down over the number: minutes on the left, seconds (by ten) on the right.
                let shown = clock(self.remaining());
                let colon = shown.rfind(':').unwrap_or(0);
                let split = big.x + figures_width(ui.g, &shown[..colon], 44.0) + 6.0;
                let steps = self.scrolled(ui, big, split, !running);
                if steps != 0 {
                    let unit = if self.magnet_right { 10 } else { 60 };
                    let secs = (self.timer_left.as_secs() as i64 + i64::from(steps) * unit).clamp(10, 10 * 3600);
                    self.pomodoro = false;
                    self.set_timer(Duration::from_secs(secs as u64));
                }
                let color = if self.pomodoro { tint } else { t.text };
                let base = self.remaining().as_secs() as i64;
                let at = move |kl: i32, kr: i32| {
                    let secs = base + i64::from(kl) * 60 + i64::from(kr) * 10;
                    if (kl != 0 || kr != 0) && !(10..=10 * 3600).contains(&secs) {
                        return None;
                    }
                    let text = clock(Duration::from_secs(secs.max(0) as u64));
                    let colon = text.rfind(':')?;
                    Some((text[..colon].to_string(), text[colon + 1..].to_string()))
                };
                self.draw_split(ui, big, &at, false, color);
                // Presets: a short break, a focus block, a long one.
                let mut x = left.x;
                for (i, minutes) in [5u64, 15, 25, 45].into_iter().enumerate() {
                    let on = !running && !self.pomodoro && self.length == Duration::from_secs(minutes * 60);
                    if Self::chip(ui, id("timers.preset") ^ i as u32, &format!("{minutes}m"), Rect::new(x, row, 44.0, 28.0), on, tint, ink) {
                        self.pomodoro = false;
                        self.set_timer(Duration::from_secs(minutes * 60));
                    }
                    x += 50.0;
                }
                if Self::chip(ui, id("timers.reset"), "Reset", Rect::new(x, row, 58.0, 28.0), false, tint, ink) {
                    self.set_timer(self.length);
                }
                let left_fraction = self.remaining().as_secs_f32() / self.length.as_secs_f32().max(1.0);
                if self.dial(ui, card, left_fraction, Err(running), true, tint) {
                    match self.timer_end.take() {
                        Some(end) => self.timer_left = end.saturating_duration_since(Instant::now()),
                        None => self.start(),
                    }
                }
            }
            Tab::Stopwatch => {
                let running = self.stopwatch_start.is_some();
                let elapsed = self.elapsed();
                // The seconds roll; the tenths just change, they are too quick to roll.
                let whole = clock(elapsed);
                self.draw_big(ui, big, &whole, true, t.text);
                let x = big.x + figures_width(ui.g, &whole, 44.0);
                figures(ui.g, &format!(".{}", elapsed.subsec_millis() / 100), Rect::new(x, big.y, 60.0, big.h), 44.0, t.text);
                if Self::chip(ui, id("timers.sw.reset"), "Reset", Rect::new(left.x, row, 70.0, 28.0), false, t.accent, t.on_accent) {
                    (self.stopwatch_start, self.stopwatch_banked) = (None, Duration::ZERO);
                }
                // The ring sweeps once a minute, like a second hand.
                let sweep = (elapsed.as_secs_f32() % 60.0) / 60.0;
                if self.dial(ui, card, sweep, Err(running), true, t.accent) {
                    match self.stopwatch_start.take() {
                        Some(start) => self.stopwatch_banked += start.elapsed(),
                        None => self.stopwatch_start = Some(Instant::now()),
                    }
                }
            }
            Tab::Alarm => {
                let (hour, minute, on) = self.alarm;
                let mut changed = false;
                // Up and down over the number: hours on the left, minutes on the right.
                let split = big.x + ui.g.measure_number(&format!("{hour:02}"), 44.0) + 6.0;
                let steps = self.scrolled(ui, big, split, true);
                if steps != 0 {
                    if self.magnet_right {
                        self.alarm.1 = (i32::from(minute) + steps).rem_euclid(60) as u16;
                    } else {
                        self.alarm.0 = (i32::from(hour) + steps).rem_euclid(24) as u16;
                    }
                    changed = true;
                }
                let (hour, minute, _) = self.alarm;
                let at = move |kl: i32, kr: i32| {
                    let h = (i32::from(hour) + kl).rem_euclid(24);
                    let m = (i32::from(minute) + kr).rem_euclid(60);
                    Some((format!("{h:02}"), format!("{m:02}")))
                };
                self.draw_split(ui, big, &at, true, if on { t.text } else { t.tertiary });
                let steps: [(&str, i32, i32); 4] = [("-1h", -1, 0), ("+1h", 1, 0), ("-5m", 0, -5), ("+5m", 0, 5)];
                let mut x = left.x;
                for (i, (label, dh, dm)) in steps.into_iter().enumerate() {
                    if Self::chip(ui, id("alarm.step") ^ i as u32, label, Rect::new(x, row, 48.0, 28.0), false, t.accent, t.on_accent) {
                        self.alarm.0 = (i32::from(self.alarm.0) + dh).rem_euclid(24) as u16;
                        self.alarm.1 = (i32::from(self.alarm.1) + dm).rem_euclid(60) as u16;
                        changed = true;
                    }
                    x += 54.0;
                }
                // The ring shows how much of the wait is left, out of a day.
                let wait = self.alarm_deadline().map_or(0.0, |at| at.saturating_duration_since(Instant::now()).as_secs_f32());
                if self.dial(ui, card, 1.0 - wait / 86_400.0, Ok('\u{EA8F}'), on, t.accent) {
                    self.alarm.2 = !on;
                    changed = true;
                }
                if changed {
                    self.alarm_rang = None;
                    self.save_alarm();
                }
            }
        }
    }

    fn deadline(&self) -> Option<Instant> {
        [self.warning_at(), self.timer_end, self.alarm_deadline()].into_iter().flatten().min()
    }

    fn due(&mut self, now: Instant, t: &Theme) -> Option<Color> {
        let tint = self.tint(t);
        if let Some(end) = self.timer_end {
            if now >= end {
                // Time is up: ring, pulse, and count on below zero until stopped.
                (self.timer_end, self.timer_left, self.warned) = (None, self.length, 2);
                self.overdue_since = Some(end);
                self.ringing = Some(Ringing::Timer);
                (self.shake, self.fresh) = (Some(now), false);
                self.pulse(tint, Duration::from_millis(3400));
                ring(true);
                return Some(tint);
            }
            // 10 and 5 seconds left: the pill peeks with a shaking icon and a pulse.
            if self.warning_at().is_some_and(|at| now + Duration::from_millis(400) >= at) {
                self.warned += 1;
                self.shake = Some(now);
                self.pulse(tint, Duration::from_millis(1600));
                return Some(tint.alpha(0.0));
            }
        }
        // SAFETY: plain value-returning call.
        let clock = unsafe { GetLocalTime() };
        let key = (clock.wDay, self.alarm.0, self.alarm.1);
        if self.alarm.2 && (clock.wHour, clock.wMinute) == (self.alarm.0, self.alarm.1) && self.alarm_rang != Some(key) {
            self.alarm_rang = Some(key);
            self.ringing = Some(Ringing::Alarm);
            ring(true);
            return Some(t.accent);
        }
        None
    }

    fn rings(&self) -> Option<(Color, Instant)> {
        self.pulse.filter(|(_, at, length)| at.elapsed() < *length).map(|(color, at, _)| (color, at))
    }

    fn collapsed(&mut self, t: &Theme) -> Option<Color> {
        // A timer was just started: the pill shows it with a pulse, then rests.
        if std::mem::take(&mut self.fresh) && self.timer_end.is_some() {
            self.pulse(self.tint(t), Duration::from_millis(2200));
            return Some(self.tint(t).alpha(0.0));
        }
        None
    }

    fn heartbeat(&self, expanded: bool) -> Option<Duration> {
        // Wake right when the shown value changes, not "a second after the last
        // frame": that drifts with timer slack and every so often skips a second.
        // A few ms late on purpose, so the value has certainly ticked over.
        const LATE: u32 = 8;
        let up = |elapsed: Duration, step_ms: u32| Duration::from_millis(u64::from(step_ms - (elapsed.subsec_millis() % step_ms) + LATE));
        if self.stopwatch_start.is_some() && expanded && self.tab == Tab::Stopwatch {
            // Tenths on the page.
            Some(up(self.elapsed(), 100))
        } else if self.overdue_since.is_some() {
            Some(up(self.overtime(), 1000))
        } else if self.timer_end.is_some() {
            // Counting down: the whole seconds change when the fraction runs out.
            let left = self.remaining();
            Some(Duration::from_millis(u64::from(left.subsec_millis() + LATE)))
        } else if self.stopwatch_start.is_some() {
            Some(up(self.elapsed(), 1000))
        } else {
            None
        }
    }

    fn animating(&self, _expanded: bool) -> bool {
        self.rings().is_some()
            || self.shake.is_some_and(|at| at.elapsed() < SHAKE)
            || self.big.moving()
            || self.small.moving()
    }
}
