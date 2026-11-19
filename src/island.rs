//! The island: which mode is shown, in which state, and the springs that
//! move the shape between states.
//!
//! Every mode has three faces: an always-on pill while it has something live
//! (music playing, a timer running), the pill shown on hover, and the full
//! page. All pages share one size, and modes are changed by scrolling
//! sideways, one mode per swipe.

use crate::shell::render::{Gfx, Rect};
use crate::shell::shape::Shape;
use crate::shell::spring::{Preset, Spring, CALM};
use crate::shell::ui::{id, Action, Controls, Input, Ui};
use crate::theme::{Color, Theme};
use std::time::{Duration, Instant};

/// Size of every mode's full page.
pub const PAGE: (f32, f32) = (440.0, 184.0);
/// Height of the strip at the bottom of a page that belongs to the mode dots.
pub const PAGE_FOOTER: f32 = 24.0;

/// Something the island can show.
pub trait Activity {
    /// Stable name, used to remember the last mode.
    fn name(&self) -> &'static str;
    /// 0 = nothing live; higher values win the always-on pill.
    fn priority(&self) -> u8 {
        0
    }
    fn compact_width(&self, _g: &Gfx) -> f32 {
        220.0
    }
    /// The pill: key information only, readable in a moment.
    fn compact(&mut self, ui: &mut Ui, r: Rect);
    /// `Some` when this activity is a mode with a full page (the size is fixed by [`PAGE`]).
    fn page_size(&self) -> Option<(f32, f32)> {
        None
    }
    fn page(&mut self, _ui: &mut Ui, _r: Rect) {}
    /// Called once a minute and when the island opens.
    fn refresh(&mut self) {}
    /// Something happened in the system. Return a colour to announce it: the
    /// island shows this activity's pill with a wash of light in that colour.
    fn on_event(&mut self, _event: &Event, _g: &Gfx, _t: &Theme) -> Option<Color> {
        None
    }
    /// This activity's pill must stay out, over any app, until the user deals with it
    /// (a ringing call).
    fn sticky(&self) -> bool {
        false
    }
    /// The island should open this activity's page right now (a drag is hovering).
    fn demands_page(&self) -> bool {
        false
    }
    /// A small persistent dot beside the island (microphone or camera in use).
    fn badge(&self, _t: &Theme) -> Option<Color> {
        None
    }
    /// The moment this activity next needs attention (a timer ending), shown or not.
    fn deadline(&self) -> Option<Instant> {
        None
    }
    /// The deadline has arrived. Return a colour to announce it.
    fn due(&mut self, _now: Instant, _t: &Theme) -> Option<Color> {
        None
    }
    /// While on screen, redraw this often (a running clock).
    fn heartbeat(&self, _expanded: bool) -> Option<Duration> {
        None
    }
    /// Typing into a field: the island stays open and takes the keyboard.
    fn typing(&self) -> bool {
        false
    }
    /// While live, the pill may stay out on its own (music playing). A running
    /// timer instead peeks now and then and otherwise rests in the lip.
    fn stays(&self) -> bool {
        true
    }
    /// Rings of light pulsing out from the island's edge, and since when (a timer
    /// that ran out). They are drawn on a separate, click-through layer.
    fn rings(&self) -> Option<(Color, Instant)> {
        None
    }
    /// The full page was just closed. Return a colour to show the pill for a
    /// moment before it folds away (a timer that was just started).
    fn collapsed(&mut self, _t: &Theme) -> Option<Color> {
        None
    }
    /// The page paints right up to the island's curved edge (a cover photo), so it
    /// must be clipped to the exact shape. Others fit inside and clip cheaply.
    fn full_bleed(&self) -> bool {
        false
    }
    /// While on screen, redraw continuously (a live waveform).
    fn animating(&self, _expanded: bool) -> bool {
        false
    }
}

fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("GLIDE_TRACE").is_some())
}

/// `GLIDE_TRACE=<file>` appends one line per state change, for diagnosing behaviour.
pub fn trace(line: &str) {
    if !trace_enabled() {
        return;
    }
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let at = START.get_or_init(Instant::now).elapsed().as_millis();
    if let Some(path) = std::env::var_os("GLIDE_TRACE") {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{at:>7} ms  {line}");
        }
    }
}

/// News from the system, delivered to every activity on the UI thread.
pub enum Event {
    /// A shelf send or receive moved along.
    Share(crate::modules::share::News),
    /// What is playing changed (track, state or position).
    Media(crate::modules::media::State),
    /// Power source or battery level changed.
    Power,
    /// A fresh forecast arrived.
    Weather(crate::modules::weather::Report),
    /// The output volume or mute state changed.
    Volume { level: f32, muted: bool },
    /// A key was pressed somewhere (only used to notice Caps Lock).
    Keyboard,
    /// An app started or stopped using the microphone or camera.
    Privacy { camera: bool, on: bool },
    /// Something is being dragged over the island (or stopped being).
    DragOver(bool),
    /// Files or text were dropped on the island.
    Dropped(Vec<crate::modules::shelf::Item>),
    /// The clipboard changed.
    Clipboard(Vec<crate::modules::shelf::Item>),
    /// Another app posted a notification.
    Notification(crate::modules::notify::Note),
    /// A notification was removed (read, dismissed, or a call that ended).
    NotificationRemoved(u32),
    /// The default audio output changed (headphones connected, for example).
    AudioDevice,
    /// Bluetooth devices connected right now, with battery levels when reported.
    Bluetooth(Vec<(String, Option<u8>)>),
    /// This year's public holidays for the region arrived.
    Holidays(Vec<crate::modules::calendar::Holiday>),
}

/// Lets a worker thread hand an [`Event`] to the UI thread and wake it.
#[derive(Clone)]
pub struct Post {
    pub tx: std::sync::mpsc::Sender<Event>,
    pub hwnd: isize,
    pub message: u32,
}

impl Post {
    pub fn send(&self, event: Event) {
        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
        if self.tx.send(event).is_ok() {
            // SAFETY: posting to a window handle; a stale handle only makes the call fail.
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    Some(HWND(self.hwnd as *mut _)),
                    self.message,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// A thin lip: the island is there, and out of the way.
    Rest,
    Compact,
    Expanded,
}

const REST: (f32, f32) = (132.0, 7.0);
const COMPACT_H: f32 = 36.0;
/// How long the pointer may be away before the island folds back.
const GRACE: Duration = Duration::from_millis(420);
/// How long an arrival keeps the island open.
const PEEK: Duration = Duration::from_millis(3600);
/// After a mode change, further scrolling is ignored this long: one swipe, one mode.
const SCROLL_LOCK: Duration = Duration::from_millis(340);
/// Critically damped: arrives without overshoot.
const SMOOTH: Preset = Preset { stiffness: 250.0, damping: 1.0 };

pub struct Island {
    pub activities: Vec<Box<dyn Activity>>,
    mode: Mode,
    /// The mode shown on hover and when expanded.
    current: usize,
    /// What was on screen last frame, to restart the fade when it changes.
    shown: usize,
    /// An activity with news, and until when its pill is shown.
    announce: Option<(usize, Instant)>,
    last_live: Option<usize>,
    expanded: bool,
    hovered: bool,
    leave_at: Option<Instant>,
    hold_until: Option<Instant>,
    /// Partial sideways scroll, in swipes. Reaching 1 changes mode.
    scroll: f32,
    scroll_lock: Instant,
    w: Spring,
    h: Spring,
    r: Spring,
    slant: Spring,
    reveal: Spring,
    slide: Spring,
    /// Where the mode indicator sits among the dots (in dot positions).
    dot: Spring,
    glow: Spring,
    glow_color: Color,
    controls: Controls,
    actions: Vec<Action>,
    /// What the click-through layer last showed, to skip redrawing it.
    fx_drawn: Option<(i32, i32, i32, i32, Option<i32>, bool)>,
    /// The user moved to another mode: worth remembering.
    pub mode_changed: bool,
    /// Windows' "show animations" setting is off: change state without motion.
    pub reduce_motion: bool,
    /// 0 plugged in, 1 on battery, 2 Energy Saver on. Continuous animation
    /// (waveform, charge pulse) runs slower on battery and not at all in Energy Saver.
    pub power: u8,
}

/// What the shell needs to know after a frame.
pub struct Frame {
    /// The part of the surface the window must cover, in logical pixels.
    pub bounds: Rect,
    /// Still moving: draw again on the next refresh.
    pub animating: bool,
    /// Nothing is moving, but wake up after this long (a pending fold-back).
    pub wake_in: Option<Duration>,
    /// Requests from activities, for the shell to carry out.
    pub actions: Vec<Action>,
    /// Something is being typed: the window should have the keyboard.
    pub keyboard: bool,
}

impl Island {
    pub fn new(activities: Vec<Box<dyn Activity>>) -> Self {
        Self {
            activities,
            mode: Mode::Rest,
            current: 0,
            shown: 0,
            announce: None,
            last_live: None,
            expanded: false,
            hovered: false,
            leave_at: None,
            hold_until: None,
            scroll: 0.0,
            scroll_lock: Instant::now(),
            w: Spring::new(REST.0, SMOOTH),
            h: Spring::new(REST.1, SMOOTH),
            r: Spring::new(4.0, CALM),
            slant: Spring::new(0.0, CALM),
            reveal: Spring::new(0.0, CALM),
            slide: Spring::new(0.0, SMOOTH),
            dot: Spring::new(0.0, Preset { stiffness: 320.0, damping: 0.8 }),
            glow: Spring::new(0.0, CALM),
            glow_color: Color::rgb(0xffffff),
            controls: Controls::default(),
            actions: Vec::new(),
            fx_drawn: None,
            mode_changed: false,
            reduce_motion: false,
            power: 0,
        }
    }

    /// Only the lip is showing and nothing is about to change on its own, so
    /// background news that is not worth a glance needs no redraw.
    pub fn resting(&self) -> bool {
        self.mode == Mode::Rest && self.announce.is_none() && !self.hovered && !self.expanded
    }

    /// Name of the mode shown on hover.
    pub fn current_name(&self) -> &'static str {
        self.activities.get(self.current).map_or("", |a| a.name())
    }

    /// Make the named mode the current one (restoring the last session's choice).
    pub fn select(&mut self, name: &str) {
        if let Some(index) = self.activities.iter().position(|a| a.name() == name && a.page_size().is_some()) {
            self.current = index;
        }
    }

    /// An activity has news: show its pill, with a wash of light in its colour.
    pub fn announce(&mut self, index: usize, color: Color) {
        self.announce = Some((index, Instant::now() + PEEK));
        // A transparent colour means "show it, quietly": no light.
        if color.a > 0.0 {
            self.glow_color = color;
            self.glow.snap(1.0);
            self.glow.set(0.0);
        }
    }

    /// Show the named activity's pill for a moment.
    pub fn announce_named(&mut self, name: &str, color: Color) {
        if let Some(index) = self.activities.iter().position(|a| a.name() == name) {
            self.announce(index, color);
        }
    }

    pub fn refresh(&mut self) {
        for activity in &mut self.activities {
            activity.refresh();
        }
    }

    /// Hand an event to every activity; the first one that wants attention gets it.
    pub fn dispatch(&mut self, event: &Event, g: &Gfx, theme: &Theme) {
        let mut announce = None;
        for (index, activity) in self.activities.iter_mut().enumerate() {
            if let Some(color) = activity.on_event(event, g, theme) {
                announce.get_or_insert((index, color));
            }
        }
        if let Some((index, color)) = announce {
            self.announce(index, color);
        }
    }

    /// The live activity with the highest priority, if any.
    fn live(&self) -> Option<usize> {
        (0..self.activities.len()).filter(|i| self.activities[*i].priority() > 0).max_by_key(|i| self.activities[*i].priority())
    }

    /// The live activity whose pill stays out by itself, if any.
    fn resident(&self) -> Option<usize> {
        (0..self.activities.len())
            .filter(|i| self.activities[*i].priority() > 0 && self.activities[*i].stays())
            .max_by_key(|i| self.activities[*i].priority())
    }

    fn modes(&self) -> Vec<usize> {
        (0..self.activities.len()).filter(|i| self.activities[*i].page_size().is_some()).collect()
    }

    /// Where the pointer has to be for the island to count as hovered.
    pub fn hit_area(&self, cx: f32) -> Rect {
        let (w, h) = (self.w.target.max(self.w.value), self.h.target.max(self.h.value));
        if self.mode == Mode::Rest {
            Rect::new(cx - 90.0, 0.0, 180.0, 12.0)
        } else {
            Rect::new(cx - w / 2.0 - 12.0, 0.0, w + 24.0, h + 6.0)
        }
    }

    /// Draw one frame. `obscured` is true when another app's window sits under the island.
    /// `fx` is the click-through layer behind the island: shadow, glow and rings
    /// are drawn there, so nothing outside the island's shape takes clicks.
    pub fn frame(&mut self, g: &Gfx, fx: &Gfx, theme: &Theme, input: &Input, dt: f32, obscured: bool) -> Frame {
        let now = Instant::now();
        let cx = g.size().0 / 2.0;

        // --- deadlines ---------------------------------------------------------
        let mut rang = None;
        for (index, activity) in self.activities.iter_mut().enumerate() {
            if activity.deadline().is_some_and(|at| at <= now + Duration::from_millis(900)) {
                if let Some(color) = activity.due(now, theme) {
                    rang.get_or_insert((index, color));
                }
            }
        }
        if let Some((index, color)) = rang {
            self.announce(index, color);
        }

        // --- pointer and timers ------------------------------------------------
        // Changing mode must not count as leaving, even if the shape moves.
        if self.hold_until.is_some_and(|at| now >= at) {
            self.hold_until = None;
        }
        let typing = self.activities.iter().any(|a| a.typing());
        if input.inside || self.hold_until.is_some() || typing {
            if !self.hovered {
                self.refresh();
            }
            (self.hovered, self.leave_at) = (true, None);
        } else if self.hovered && self.leave_at.is_none() {
            self.leave_at = Some(now + GRACE);
        }
        if self.leave_at.is_some_and(|at| now >= at) {
            let was_open = self.expanded;
            (self.hovered, self.expanded, self.leave_at) = (false, false, None);
            if was_open {
                let current = self.current;
                if let Some(color) = self.activities.get_mut(current).and_then(|a| a.collapsed(theme)) {
                    self.announce(current, color);
                }
            }
        }
        if self.announce.is_some_and(|(_, until)| now >= until) {
            self.announce = None;
        }

        // --- which activity, which state ---------------------------------------
        let modes = self.modes();
        if modes.is_empty() {
            return Frame { bounds: Rect::default(), animating: false, wake_in: None, actions: Vec::new(), keyboard: false };
        }
        if !modes.contains(&self.current) {
            self.current = modes[0];
        }
        // Something that just went live (music starting) becomes the current mode,
        // so it is what a hover shows first.
        let live = self.live();
        if live != self.last_live {
            if let Some(index) = live.filter(|i| modes.contains(i)) {
                self.current = index;
            }
            self.last_live = live;
        }
        // A drag hovering over the island opens the shelf by itself.
        let demanded = self.activities.iter().position(|a| a.demands_page());

        // Sideways scrolling changes mode: one swipe, one mode.
        if self.hovered && demanded.is_none() && modes.len() > 1 && now >= self.scroll_lock {
            // Sideways always changes mode. Up and down do too on the pill; on an open
            // page they belong to the page (scrolling the shelf, setting a timer).
            self.scroll += input.wheel_x + if self.expanded { 0.0 } else { input.wheel };
            if self.scroll.abs() >= 1.0 {
                let step = self.scroll.signum() as i32;
                let at = modes.iter().position(|m| *m == self.current).unwrap_or(0) as i32;
                self.current = modes[(at + step).rem_euclid(modes.len() as i32) as usize];
                (self.scroll, self.scroll_lock) = (0.0, now + SCROLL_LOCK);
                (self.announce, self.mode_changed) = (None, true);
                self.hold_until = Some(now + Duration::from_millis(1400));
                if !self.reduce_motion {
                    // The new mode arrives from the side it was scrolled in from,
                    // and the shape gives a little.
                    self.slide.snap(step as f32 * 34.0);
                    self.w.kick(240.0);
                }
            }
        } else {
            self.scroll = 0.0;
        }
        // Resistance: a partial swipe pulls the content a little and lets go.
        self.scroll *= (1.0 - dt * 7.0).max(0.0);
        if self.scroll.abs() < 0.01 {
            self.scroll = 0.0;
        }

        let (shown, mode) = if let Some(index) = demanded {
            (index, Mode::Expanded)
        } else if self.expanded {
            (self.current, Mode::Expanded)
        } else if let Some(index) = self.activities.iter().position(|a| a.sticky()) {
            (index, Mode::Compact)
        } else if let Some((index, _)) = self.announce {
            (index, Mode::Compact)
        } else if self.hovered {
            (self.current, Mode::Compact)
        } else if let (Some(index), false) = (self.resident(), obscured) {
            // Always-on pill: only while nothing else is using that part of the screen.
            (index, Mode::Compact)
        } else {
            (self.current, Mode::Rest)
        };
        let (tw, th, tr, ts) = match mode {
            Mode::Rest => (REST.0, REST.1, 2.5, 0.0),
            Mode::Compact => (self.activities[shown].compact_width(g).clamp(150.0, PAGE.0), COMPACT_H, 18.0, 3.0),
            Mode::Expanded => (PAGE.0, PAGE.1, 30.0, 7.0),
        };
        if mode != self.mode || shown != self.shown {
            trace(&format!(
                "{:?} -> {mode:?} shown={shown} current={} hovered={} obscured={obscured}",
                self.mode, self.current, self.hovered
            ));
            (self.mode, self.shown) = (mode, shown);
            // Content fades and scales in; nothing bounces.
            self.reveal.snap(0.0);
        }
        self.w.set(tw);
        self.h.set(th);
        self.r.set(tr);
        self.slant.set(ts);
        self.reveal.set(1.0);
        self.slide.set(0.0);
        self.dot.set(modes.iter().position(|m| *m == shown).unwrap_or(0) as f32);

        let springs =
            [
            &mut self.w, &mut self.h, &mut self.r, &mut self.slant, &mut self.reveal, &mut self.slide, &mut self.glow,
            &mut self.dot,
        ];
        let mut moving = self.scroll != 0.0;
        for spring in springs {
            if self.reduce_motion {
                spring.snap(spring.target);
            } else {
                spring.step(dt);
            }
            moving |= !spring.settled();
        }

        // --- draw ----------------------------------------------------------------
        let shape = Shape {
            w: self.w.value.max(20.0),
            h: self.h.value.max(3.0),
            r: self.r.value.max(1.0),
            // The resting lip is a plain bar; the soft shoulders that join the
            // screen edge grow in only as it becomes a pill.
            flare: (self.h.value * 0.5).clamp(3.0, 13.0) * ((self.h.value - REST.1) / (COMPACT_H - REST.1)).clamp(0.0, 1.0),
            slant: self.slant.value.max(0.0),
            bulge: 0.0,
            lean: 0.0,
        };
        // --- the click-through layer: shadow, glow, rings, the mic/camera dot ---
        // Redrawn only when something on it changes.
        let lift = ((shape.h - REST.1) / (COMPACT_H - REST.1)).clamp(0.0, 1.0);
        let badge = self.activities.iter().find_map(|a| a.badge(theme));
        let rings = self.activities.iter().find_map(|a| a.rings());
        let quant = |v: f32| (v * 4.0).round() as i32;
        let key = (
            quant(shape.w),
            quant(shape.h),
            quant(lift * 10.0),
            quant(self.glow.value * 10.0),
            badge.map(|c| quant(c.r * 64.0) * 4096 + quant(c.g * 64.0) * 64 + quant(c.b * 64.0)),
            theme.dark,
        );
        if rings.is_some() || self.fx_drawn != Some(key) {
            self.fx_drawn = if rings.is_some() { None } else { Some(key) };
            fx.begin();
            if let Some(path) = shape.path(fx, cx) {
                // A soft shadow keeps the island readable over bright windows; a resting lip casts none.
                if lift > 0.01 {
                    fx.glow(cx, shape.h * 0.55, shape.w * 0.62 + 18.0, shape.h * 0.75 + 20.0, theme.shadow.alpha(lift));
                }
                if self.glow.value > 0.01 {
                    fx.glow(cx, shape.h, shape.w * 0.6, 34.0, self.glow_color.alpha(0.7 * self.glow.value));
                }
                // Rings ripple out from the edge and fade, two at a time.
                if let Some((color, since)) = rings {
                    let t = since.elapsed().as_secs_f32();
                    for k in 0..2 {
                        let phase = ((t - k as f32 * 0.55).max(0.0) % 1.1) / 1.1;
                        if t < k as f32 * 0.55 {
                            continue;
                        }
                        let d = 2.0 + phase * 16.0;
                        let ring = Shape { w: shape.w + 2.0 * d, h: shape.h + d, r: shape.r + d, ..shape };
                        if let Some(outline) = ring.path(fx, cx) {
                            fx.stroke_path(&outline, color.alpha(0.55 * (1.0 - phase).powi(2)), 2.0);
                        }
                    }
                    let _ = path;
                }
                // Ongoing microphone or camera use: a small dot that stays beside the island.
                if let Some(color) = badge {
                    let x = cx + shape.w / 2.0 + shape.flare + 9.0;
                    fx.glow(x, 7.0, 9.0, 9.0, color.alpha(0.5));
                    fx.fill_circle(x, 7.0, 3.2, color);
                }
            }
            // The layer never makes the island wait for the display.
            fx.end(0);
        }
        moving |= rings.is_some();

        g.begin();
        if let Some(path) = shape.path(g, cx) {
            g.fill_path(&path, theme.body);
            g.stroke_path(&path, theme.rim, 1.0);
            if self.glow.value > 0.01 {
                g.stroke_path(&path, self.glow_color.alpha(0.6 * self.glow.value), 1.5);
            }

            if mode != Mode::Rest {
                // Content is laid out at its final size, clipped by the shape, and
                // eased in with a fade and a slight scale.
                let reveal = self.reveal.value.clamp(0.0, 1.0);
                // A settled pill's content already fits inside it, so a plain rectangle
                // clips it; the exact outline (much dearer) is for pages and for motion.
                let settled = self.w.settled() && self.h.settled();
                let exact = !settled || (mode == Mode::Expanded && self.activities[shown].full_bleed());
                if exact {
                    g.push_clip(&path);
                } else {
                    // Inside the flanks and above the rounded bottom corners.
                    let inner = if mode == Mode::Expanded { shape.slant + 4.0 } else { 0.0 };
                    g.push_rect_clip(Rect::new(cx - shape.w / 2.0 + inner, 0.0, shape.w - 2.0 * inner, shape.h));
                }
                g.set_opacity(reveal);
                g.set_transform(0.94 + 0.06 * reveal, cx, th / 2.0, self.slide.value - self.scroll * 20.0);
                self.controls.begin_frame();
                let mut ui = Ui {
                    g,
                    t: theme,
                    input,
                    controls: &mut self.controls,
                    actions: &mut self.actions,
                    dt,
                    interactive: reveal > 0.6,
                };
                let area = Rect::new(cx - tw / 2.0 + ts, 0.0, tw - 2.0 * ts, th);
                let mut open = false;
                let mut go_to = None;
                match mode {
                    Mode::Compact => {
                        let before = ui.actions.len();
                        let clicked_before = ui.controls.clicks;
                        self.activities[shown].compact(&mut ui, area.inset(12.0, 0.0));
                        // Anywhere on the pill that is not one of its controls opens the page.
                        let own_control = ui.controls.clicks != clicked_before || ui.actions.len() != before;
                        // A sticky pill (a ringing call) has no page to open.
                        open = ui.hit(id("island.compact"), area).clicked && !own_control && !self.activities[shown].sticky();
                    }
                    Mode::Expanded => {
                        self.activities[shown].page(&mut ui, area.inset(16.0, 0.0));
                        // Mode dots: the current one is a short accent bar.
                        let gap = 13.0;
                        let start = cx - gap * (modes.len() as f32 - 1.0) / 2.0;
                        for (n, &index) in modes.iter().enumerate() {
                            let (x, y) = (start + gap * n as f32, th - PAGE_FOOTER / 2.0);
                            let hit = ui.hit(id("island.dot") ^ index as u32, Rect::new(x - 6.5, y - 9.0, 13.0, 18.0));
                            // The dots stay where they are; only the indicator travels.
                            ui.g.fill_circle(x, y, 2.5 + 0.8 * hit.hover, theme.tertiary.mix(theme.text, hit.hover * 0.6));
                            if hit.clicked && index != shown {
                                go_to = Some(index);
                            }
                        }
                        // The indicator: an accent blob that slides from dot to dot,
                        // stretching a little while it is on its way.
                        let stretch = (self.dot.velocity.abs() * 0.9).min(7.0);
                        let x = start + gap * self.dot.value;
                        ui.g.fill_rect(Rect::new(x - 5.5 - stretch / 2.0, th - PAGE_FOOTER / 2.0 - 2.5, 11.0 + stretch, 5.0), 2.5, theme.accent);
                    }
                    Mode::Rest => {}
                }
                moving |= ui.controls.moving;
                self.controls.end_frame(input);
                g.set_transform(1.0, 0.0, 0.0, 0.0);
                g.set_opacity(1.0);
                if exact {
                    g.pop_clip();
                } else {
                    g.pop_rect_clip();
                }
                if open {
                    // Clicking an announcement opens its own mode, when it has one.
                    if let Some((index, _)) = self.announce.take() {
                        if modes.contains(&index) {
                            self.current = index;
                        }
                    }
                    self.expanded = true;
                    self.refresh();
                    moving = true;
                }
                if let Some(index) = go_to {
                    (self.current, self.mode_changed) = (index, true);
                    self.hold_until = Some(now + Duration::from_millis(1400));
                    moving = true;
                }
            }
        }
        // A live waveform keeps drawing, at a third of the refresh rate: it still
        // reads as sound, for a third of the work.
        // Energy Saver stills it entirely; on battery it draws at a fifth of the refresh rate.
        let live_motion =
            self.power < 2 && mode != Mode::Rest && self.activities[shown].animating(mode == Mode::Expanded);
        g.end(if live_motion && !moving { if self.power == 1 { 5 } else { 3 } } else { 1 });
        moving |= live_motion;

        // Room for where the shape is and where it is going, and nothing more: the
        // glow and rings live on the click-through layer, so the window itself
        // never covers anything beside the island.
        let reach = Shape { w: shape.w.max(tw) * 1.04, h: shape.h.max(th) + 6.0, ..shape };
        let mut bounds = reach.bounds(cx).inset(-2.0, 0.0);
        // A resting lip covers only itself, so whatever is under the space a pill
        // would take (a browser tab, a button) can still be clicked.
        if mode == Mode::Rest && !moving && self.glow.value < 0.01 {
            bounds = self.hit_area(cx);
        }
        let deadlines = self.activities.iter().filter_map(|a| a.deadline()).min();
        let announce_end = self.announce.map(|(_, until)| until);
        let pending = [self.leave_at, announce_end, self.hold_until, deadlines].into_iter().flatten().min();
        let mut wake_in = pending.map(|at| at.saturating_duration_since(now) + Duration::from_millis(10));
        // A visible running clock needs a redraw now and then; a hidden one does not.
        if mode != Mode::Rest {
            if let Some(beat) = self.activities[shown].heartbeat(mode == Mode::Expanded) {
                wake_in = Some(wake_in.map_or(beat, |wait| wait.min(beat)));
            }
        }
        Frame { bounds, animating: moving, wake_in, actions: std::mem::take(&mut self.actions), keyboard: typing }
    }
}
