//! A very small immediate-mode toolkit: content is drawn from scratch on every
//! animated frame, and each interactive area carries two springs (hover and
//! press) so feedback is physical rather than a state swap.

use super::render::{Align, Gfx, Rect};
use super::spring::{Spring, SNAPPY};
use crate::theme::{Color, Theme};
use std::collections::HashMap;

/// Pointer state in surface coordinates, plus the edges that happened since the last frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input {
    pub x: f32,
    pub y: f32,
    /// The pointer is over the island.
    pub inside: bool,
    pub down: bool,
    pub pressed: bool,
    pub released: bool,
    /// Vertical wheel notches since the last frame (positive = towards the user, scrolling down).
    pub wheel: f32,
    /// Sideways wheel or two-finger swipe notches (positive = to the right).
    pub wheel_x: f32,
    /// Where the current press began.
    pub press_x: f32,
    pub press_y: f32,
    /// Characters typed since the last frame (backspace, enter and escape included),
    /// only while the island has the keyboard.
    pub typed: [char; 16],
    pub typed_len: usize,
    /// The island's window has the keyboard.
    pub keyboard: bool,
}

impl Input {
    pub fn typed(&self) -> &[char] {
        &self.typed[..self.typed_len]
    }

    pub fn type_char(&mut self, c: char) {
        if self.typed_len < self.typed.len() {
            self.typed[self.typed_len] = c;
            self.typed_len += 1;
        }
    }
}

/// Something an activity asks the shell to do after the frame.
pub enum Action {
    /// Put the item on the clipboard.
    Copy(crate::modules::shelf::Item),
    /// Start dragging the item out to another app.
    Drag(crate::modules::shelf::Item),
    /// Open a link or file with its default app.
    Open(String),
}

#[derive(Default)]
pub struct Controls {
    springs: HashMap<u32, (Spring, Spring)>,
    /// The control the current press started on.
    active: Option<u32>,
    /// Something is still moving: another frame is needed.
    pub moving: bool,
    /// Number of clicks delivered to controls so far.
    pub clicks: u32,
    /// Shape morphs (play to pause), one spring per glyph.
    morphs: HashMap<u32, Spring>,
}

impl Controls {
    pub fn begin_frame(&mut self) {
        self.moving = false;
    }
    pub fn end_frame(&mut self, input: &Input) {
        if input.released || !input.down {
            self.active = None;
        }
        // Controls that were not drawn this frame relax back to rest.
        self.springs.retain(|_, (hover, press)| !(hover.settled() && press.settled() && hover.target == 0.0));
    }
}

/// Result of offering an area to the pointer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hit {
    /// 0..1, eased.
    pub hover: f32,
    /// 0..1, eased.
    pub press: f32,
    pub clicked: bool,
}

pub struct Ui<'a> {
    pub g: &'a Gfx,
    pub t: &'a Theme,
    pub input: &'a Input,
    pub controls: &'a mut Controls,
    pub actions: &'a mut Vec<Action>,
    pub dt: f32,
    /// Interaction is ignored while content is fading out.
    pub interactive: bool,
}

/// Stable id for a control from its name (FNV-1a).
pub const fn id(name: &str) -> u32 {
    let bytes = name.as_bytes();
    let mut hash = 0x811c_9dc5u32;
    let mut i = 0;
    while i < bytes.len() {
        hash = (hash ^ bytes[i] as u32).wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

impl Ui<'_> {
    pub fn hit(&mut self, id: u32, r: Rect) -> Hit {
        let over = self.interactive && self.input.inside && r.contains(self.input.x, self.input.y);
        // The first control drawn under a press owns it, so a button on the pill
        // is not also a click on the pill behind it.
        if over && self.input.pressed && self.controls.active.is_none() {
            self.controls.active = Some(id);
        }
        let held = self.controls.active == Some(id) && self.input.down && over;
        let clicked = over && self.input.released && self.controls.active == Some(id);
        let (hover, press) =
            self.controls.springs.entry(id).or_insert_with(|| (Spring::new(0.0, SNAPPY), Spring::new(0.0, SNAPPY)));
        hover.set(if over { 1.0 } else { 0.0 });
        press.set(if held { 1.0 } else { 0.0 });
        hover.step(self.dt);
        press.step(self.dt);
        if !hover.settled() || !press.settled() {
            self.controls.moving = true;
        }
        if clicked {
            self.controls.clicks += 1;
        }
        Hit { hover: hover.value.clamp(0.0, 1.2), press: press.value.clamp(0.0, 1.2), clicked }
    }

    /// Round icon button: a soft disc appears under the pointer and the glyph
    /// compresses while held.
    pub fn icon_button(&mut self, id: u32, glyph: char, cx: f32, cy: f32, size: f32, color: Color) -> bool {
        let radius = size * 0.95;
        let hit = self.hit(id, Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0));
        if hit.hover > 0.01 {
            self.g.fill_circle(cx, cy, radius * (0.9 + 0.1 * hit.hover), self.t.control_hover.alpha(hit.hover));
        }
        self.g.icon(glyph, cx, cy, size * (1.0 - 0.14 * hit.press), color);
        hit.clicked
    }

    /// The one filled control of a view (play/pause, answer).
    pub fn primary_button(&mut self, id: u32, glyph: char, cx: f32, cy: f32, radius: f32) -> bool {
        let hit = self.hit(id, Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0));
        let scale = 1.0 + 0.06 * hit.hover - 0.12 * hit.press;
        self.g.fill_circle(cx, cy, radius * scale, self.t.primary);
        self.g.icon(glyph, cx, cy, radius * 0.9 * scale, self.t.on_primary);
        hit.clicked
    }

    /// Capsule button with a label.
    pub fn pill_button(&mut self, id: u32, label: &str, r: Rect, filled: bool) -> bool {
        let hit = self.hit(id, r);
        let r = r.scaled(1.0 + 0.03 * hit.hover - 0.06 * hit.press);
        let (bg, fg) = if filled {
            (self.t.primary, self.t.on_primary)
        } else {
            (self.t.control.mix(self.t.control_hover, hit.hover), self.t.text)
        };
        self.g.fill_rect(r, r.h / 2.0, bg);
        self.g.text(label, r, 12.5, 600, Align::Center, fg);
        hit.clicked
    }

    /// Thin progress line with a rounded cap. Returns the clicked fraction, if any.
    pub fn progress(&mut self, id: u32, r: Rect, fraction: f32, fill: Color) -> Option<f32> {
        let hit = self.hit(id, r.inset(0.0, -8.0));
        let thick = 4.0 + 2.0 * hit.hover;
        let line = Rect::new(r.x, r.y + (r.h - thick) / 2.0, r.w, thick);
        self.g.fill_rect(line, thick / 2.0, self.t.track);
        let filled = Rect::new(line.x, line.y, (line.w * fraction.clamp(0.0, 1.0)).max(thick), thick);
        self.g.fill_rect(filled, thick / 2.0, fill);
        hit.clicked.then(|| ((self.input.x - r.x) / r.w).clamp(0.0, 1.0))
    }

    pub fn label(&self, text: &str, r: Rect, size: f32, weight: u32, align: Align, color: Color) {
        self.g.text(text, r, size, weight, align, color);
    }

    /// A solid play triangle that reshapes into two pause bars and back.
    /// `size` is half the glyph's height.
    pub fn play_pause(&mut self, id: u32, cx: f32, cy: f32, size: f32, playing: bool, color: Color) {
        let target = if playing { 1.0 } else { 0.0 };
        let spring = self.controls.morphs.entry(id).or_insert_with(|| Spring::new(target, SNAPPY));
        spring.set(target);
        spring.step(self.dt);
        if !spring.settled() {
            self.controls.moving = true;
        }
        let t = spring.value.clamp(0.0, 1.0);
        // Each half of the triangle becomes one bar: corners move in straight lines.
        let mix = |play: (f32, f32), pause: (f32, f32)| {
            (cx + (play.0 + (pause.0 - play.0) * t) * size, cy + (play.1 + (pause.1 - play.1) * t) * size)
        };
        let left = [
            mix((-0.55, -0.85), (-0.68, -0.8)),
            mix((0.12, -0.42), (-0.2, -0.8)),
            mix((0.12, 0.42), (-0.2, 0.8)),
            mix((-0.55, 0.85), (-0.68, 0.8)),
        ];
        let right = [
            mix((0.12, -0.42), (0.2, -0.8)),
            mix((0.82, 0.0), (0.68, -0.8)),
            mix((0.82, 0.0), (0.68, 0.8)),
            mix((0.12, 0.42), (0.2, 0.8)),
        ];
        self.g.fill_poly(&left, color);
        self.g.fill_poly(&right, color);
    }

    /// Figures (times, levels, temperatures) in the number face.
    pub fn number(&self, text: &str, r: Rect, size: f32, heavy: bool, align: Align, color: Color) {
        self.g.number(text, r, size, heavy, align, color);
    }
}

/// One step of a critically damped spring toward `goal`: the fastest motion
/// that never overshoots. Solved exactly, so it is smooth at any frame time
/// and comes to rest without the tiny back-and-forth of a bouncy spring.
pub fn settle(x: &mut f32, v: &mut f32, goal: f32, omega: f32, dt: f32) {
    let d = *x - goal;
    let decay = (-omega * dt).exp();
    let k = *v + omega * d;
    *x = goal + (d + k * dt) * decay;
    *v = (*v - omega * k * dt) * decay;
}

/// A number whose digits roll, odometer style, when it changes: each changed
/// digit slides out as the new one slides in, a little after the digit to its
/// right. Digits that stay the same do not move. The motion is critically
/// damped: quick at first, then easing straight into place, no bounce.
pub struct Roll {
    from: String,
    to: String,
    since: std::time::Instant,
    /// Rolling up (the number grew) or down.
    up: bool,
}

/// Stiffness of the roll (rad/s): settles to within a third of a pixel in `ROLL_DIGIT`.
const ROLL_OMEGA: f32 = 17.0;
const ROLL_DIGIT: f32 = 0.44;
/// How much later each digit to the left starts.
const ROLL_STAGGER: f32 = 0.045;
/// Share of a digit cell a colon or point takes: the figure face is
/// monospaced, and in a full cell they would sit in wide gaps.
pub const COLON: f32 = 0.55;

fn is_point(c: char) -> bool {
    matches!(c, ':' | '.')
}

/// Advance of one figure, with colons and points closed up.
pub fn figure_width(g: &Gfx, c: char, size: f32) -> f32 {
    g.measure_number(&c.to_string(), size) * if is_point(c) { COLON } else { 1.0 }
}

/// Width of `text` as `figures` draws it.
pub fn figures_width(g: &Gfx, text: &str, size: f32) -> f32 {
    text.chars().map(|c| figure_width(g, c, size)).sum()
}

/// One figure whose closed-up cell starts at `x` (`r` gives the rest of the box).
fn draw_figure(g: &Gfx, c: char, x: f32, r: Rect, size: f32, color: Color) {
    // A point's glyph is centred in its full cell: pull it back by the trimmed half.
    let x = if is_point(c) { x - g.measure_number(&c.to_string(), size) * (1.0 - COLON) / 2.0 } else { x };
    g.number(&c.to_string(), Rect::new(x, r.y, r.w, r.h), size, true, Align::Left, color);
}

/// Static figures from `r.x`, colons and points closed up as in a `Roll`.
pub fn figures(g: &Gfx, text: &str, r: Rect, size: f32, color: Color) {
    let mut x = r.x;
    for c in text.chars() {
        draw_figure(g, c, x, Rect::new(x, r.y, size * 1.2, r.h), size, color);
        x += figure_width(g, c, size);
    }
}

impl Roll {
    pub fn new(text: &str) -> Self {
        let long_ago = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(10)).unwrap_or_else(std::time::Instant::now);
        Self { from: text.to_string(), to: text.to_string(), since: long_ago, up: true }
    }

    /// Show `text`; if it differs from what is shown, roll to it.
    pub fn set(&mut self, text: &str, up: bool) {
        if text != self.to {
            self.from = std::mem::replace(&mut self.to, text.to_string());
            (self.since, self.up) = (std::time::Instant::now(), up);
        }
    }

    /// Show `text` at once, without rolling (it already arrived some other way).
    pub fn snap(&mut self, text: &str) {
        if text != self.to || self.moving() {
            *self = Self::new(text);
        }
    }

    pub fn moving(&self) -> bool {
        self.since.elapsed().as_secs_f32() < ROLL_DIGIT + ROLL_STAGGER * self.to.len().max(self.from.len()) as f32
    }

    /// Critically damped step response, 0 to 1.
    fn ease(t: f32) -> f32 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= ROLL_DIGIT {
            return 1.0;
        }
        let w = ROLL_OMEGA * t;
        1.0 - (1.0 + w) * (-w).exp()
    }

    /// Draw in `r`, digits travelling `travel` px, clipped to `clip` (which can
    /// reach under neighbouring labels so digits vanish beneath them). With
    /// `fade`, a leaving digit fades out as it goes and the new one fades in
    /// (where there is nothing to slide under, as on the pill).
    #[allow(clippy::too_many_arguments)]
    pub fn draw(&self, ui: &mut Ui, r: Rect, size: f32, _heavy: bool, align: Align, color: Color, travel: f32, clip: Rect, fade: bool) {
        let g = ui.g;
        let elapsed = self.since.elapsed().as_secs_f32();
        if self.moving() {
            // Foreground motion: drawn on every refresh, not at the slow live rate.
            ui.controls.moving = true;
        }
        let layout = |text: &str| -> Vec<(char, f32)> {
            // The figure face is monospaced; its colon would sit in a full
            // digit cell with wide gaps, so times close it up.
            let widths: Vec<f32> = text.chars().map(|c| figure_width(g, c, size)).collect();
            let total: f32 = widths.iter().sum();
            let mut x = match align {
                Align::Left => r.x,
                Align::Center => r.x + (r.w - total) / 2.0,
                Align::Right => r.right() - total,
            };
            text.chars()
                .zip(widths)
                .map(|(c, w)| {
                    let at = x;
                    x += w;
                    (c, at)
                })
                .collect()
        };
        let (old, new) = (layout(&self.from), layout(&self.to));
        let travel = if self.up { travel } else { -travel };
        g.push_rect_clip(clip);
        // Pair digits from the right, as an odometer does: ones with ones.
        let mut changed = 0;
        for k in 0..old.len().max(new.len()) {
            let o = old.len().checked_sub(k + 1).map(|i| old[i]);
            let m = new.len().checked_sub(k + 1).map(|i| new[i]);
            let draw = |(c, x): (char, f32), dy: f32, alpha: f32| {
                if alpha > 0.01 {
                    draw_figure(g, c, x, Rect::new(x, r.y + dy, size * 1.2, r.h), size, color.alpha(alpha));
                }
            };
            // The same digit stays put. Digits are proportional, so when the
            // number's width changes it may have to shift: it glides sideways
            // to its new place rather than rolling as if it had changed.
            if let (Some(a), Some(b)) = (o, m) {
                if a.0 == b.0 {
                    let x = a.1 + (b.1 - a.1) * Self::ease(elapsed);
                    draw((b.0, x), 0.0, 1.0);
                    continue;
                }
            }
            // Only digits that change wait their turn.
            let p = Self::ease(elapsed - ROLL_STAGGER * changed as f32);
            changed += 1;
            // Fading: out quickly (gone by two thirds of the way), in a little behind.
            let (out, inn) = if fade { ((1.0 - p * 1.5).max(0.0), p.powf(0.8)) } else { (1.0, 1.0) };
            if let Some(o) = o.filter(|_| p < 1.0) {
                draw(o, -travel * p, out);
            }
            if let Some(m) = m {
                draw(m, travel * (1.0 - p), inn);
            }
        }
        g.pop_rect_clip();
    }
}

/// A number set by scrolling, with a magnetic feel: it follows a touchpad
/// swipe continuously, clicks over to the next value halfway, and when the
/// fingers stop it springs into the nearest whole position. A mouse wheel
/// notch rolls one value.
#[derive(Default)]
pub struct Magnet {
    /// Where the strip is, in values relative to the current one (+ = towards the next).
    pub pos: f32,
    vel: f32,
    goal: f32,
    last: Option<std::time::Instant>,
}

impl Magnet {
    /// Feed this frame's scrolling (+ = up, towards larger); returns whole steps
    /// to apply to the value now.
    pub fn update(&mut self, ui: &mut Ui, delta: f32) -> i32 {
        let now = std::time::Instant::now();
        if delta != 0.0 {
            self.goal += delta;
            self.last = Some(now);
        }
        let mut steps = 0;
        // Past halfway, the value clicks over; the strip keeps its place on screen.
        while self.goal >= 0.5 {
            (self.goal, self.pos, steps) = (self.goal - 1.0, self.pos - 1.0, steps + 1);
        }
        while self.goal <= -0.5 {
            (self.goal, self.pos, steps) = (self.goal + 1.0, self.pos + 1.0, steps - 1);
        }
        // Fingers stopped: pull into place.
        let idle = self.last.is_none_or(|at| now.duration_since(at).as_millis() > 90);
        if idle {
            self.goal = 0.0;
        }
        // Tight while following a swipe, a touch softer when settling.
        settle(&mut self.pos, &mut self.vel, self.goal, if idle { 19.0 } else { 34.0 }, ui.dt.min(0.05));
        if self.pos.abs() < 0.001 && self.vel.abs() < 0.01 && self.goal == 0.0 {
            (self.pos, self.vel) = (0.0, 0.0);
        } else {
            ui.controls.moving = true;
        }
        steps
    }

    pub fn active(&self) -> bool {
        self.pos != 0.0 || self.goal != 0.0
    }
}
