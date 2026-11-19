//! First-run setup: one calm question per step, in the spirit of a browser's
//! welcome flow. The island hangs from the top of the window and changes with
//! each step. Choices take effect when setup finishes; Esc skips it.

use crate::settings::Settings;
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::shape::Shape;
use crate::shell::spring::{Preset, Spring};
use crate::shell::ui::{id, Action, Controls, Input, Ui};
use crate::theme::{system_transparency, Appearance, Color, Theme, ACCENTS};
use std::cell::RefCell;
use std::time::Instant;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, ValidateRect, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Sent to the island when setup ends. `wparam` is 1 when instant notifications were chosen.
pub const WM_WELCOME_DONE: u32 = WM_APP + 9;

const SIZE: (f32, f32) = (600.0, 440.0);
const STEPS: usize = 5;
const TIMER_FRAME: usize = 1;
const TIMER_CARET: usize = 2;
/// WM_MOUSELEAVE: the pointer left the window (asked for with TrackMouseEvent).
const MOUSE_LEFT: u32 = 0x02A3;
const SMOOTH: Preset = Preset { stiffness: 230.0, damping: 1.0 };
const GENTLE: Preset = Preset { stiffness: 190.0, damping: 0.82 };

struct Choices {
    appearance: Appearance,
    accent: usize,
    city: String,
    instant: bool,
    privacy: bool,
    autostart: bool,
}

struct Welcome {
    hwnd: HWND,
    island: HWND,
    gfx: Gfx,
    controls: Controls,
    input: Input,
    step: usize,
    choices: Choices,
    /// Whether instant notifications can be set up from here (the installed app).
    can_instant: bool,
    reveal: Spring,
    slide: Spring,
    lip_w: Spring,
    lip_h: Spring,
    last_frame: Option<Instant>,
    caret_on: bool,
    /// The backdrop's colours, easing toward the chosen look and accent.
    wash: Wash,
    /// Windows' transparency effects are on: surfaces are frosted glass.
    clear: bool,
    /// Windows' animation effects are off: colours change at once.
    still: bool,
    /// The drifting lights behind the page.
    field: Field,
}

/// One soft light in the backdrop. It wanders around its home, is pushed and
/// pulled by its neighbours, and is drawn as a few overlapping, turning lobes
/// so its outline keeps changing instead of being a circle.
struct Blob {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    home: (f32, f32),
    radius: f32,
    /// Which wash colour: 0 = a, 1 = b, 2 = c, 3 = between a and c.
    tint: u8,
    phase: f32,
}

/// The backdrop's lights and the clock that drives them.
struct Field {
    blobs: [Blob; 4],
    time: f32,
}

impl Field {
    fn new() -> Self {
        let blob = |x: f32, y: f32, radius: f32, tint: u8, phase: f32| Blob { x, y, vx: 0.0, vy: 0.0, home: (x, y), radius, tint, phase };
        Self {
            blobs: [
                blob(60.0, 40.0, 470.0, 0, 0.0),
                blob(600.0, 250.0, 430.0, 1, 1.9),
                blob(220.0, 500.0, 480.0, 2, 3.7),
                blob(500.0, -20.0, 360.0, 3, 5.2),
            ],
            time: 0.0,
        }
    }

    /// Advance the motion: each light follows a slow drifting target near its
    /// home, presses away from a neighbour that comes too close and leans
    /// toward one a little further off, so they gather, touch and part.
    fn step(&mut self, dt: f32) {
        // Slow: the colours drift rather than move.
        let dt = dt * 0.5;
        self.time += dt;
        let t = self.time;
        let n = self.blobs.len();
        let mut force = [(0.0f32, 0.0f32); 4];
        for (i, b) in self.blobs.iter().enumerate() {
            let tx = b.home.0 + 80.0 * (t * 0.11 + b.phase).sin() + 30.0 * (t * 0.23 + b.phase * 1.3).sin();
            let ty = b.home.1 + 60.0 * (t * 0.09 + b.phase * 1.7).cos() + 25.0 * (t * 0.19 + b.phase).sin();
            force[i].0 += (tx - b.x) * 0.9;
            force[i].1 += (ty - b.y) * 0.9;
            for j in 0..n {
                if i == j {
                    continue;
                }
                let o = &self.blobs[j];
                let (dx, dy) = (o.x - b.x, o.y - b.y);
                let d = (dx * dx + dy * dy).sqrt().max(1.0);
                let touch = (b.radius + o.radius) * 0.55;
                // Too close: push apart. Near: a gentle pull together.
                let pull = if d < touch { -(touch - d) * 2.2 } else { ((touch * 2.2 - d) / touch).clamp(0.0, 1.0) * 14.0 };
                force[i].0 += dx / d * pull;
                force[i].1 += dy / d * pull;
            }
        }
        let damping = (-1.6 * dt).exp();
        for (b, (fx, fy)) in self.blobs.iter_mut().zip(force) {
            b.vx = (b.vx + fx * dt) * damping;
            b.vy = (b.vy + fy * dt) * damping;
            b.x += b.vx * dt;
            b.y += b.vy * dt;
        }
    }

    fn draw(&self, g: &Gfx, wash: &Wash, light: f32) {
        let t = self.time;
        let colour = |tint: u8| match tint {
            0 => wash.a,
            1 => wash.b,
            2 => wash.c,
            _ => wash.a.mix(wash.c, 0.5),
        };
        // Where two lights come near, a stretched light joins them in a mix of
        // both colours, so they blend into one shape rather than overlap as discs.
        for i in 0..self.blobs.len() {
            for j in i + 1..self.blobs.len() {
                let (a, b) = (&self.blobs[i], &self.blobs[j]);
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let d = (dx * dx + dy * dy).sqrt().max(1.0);
                let near = (1.0 - d / ((a.radius + b.radius) * 1.15)).clamp(0.0, 1.0);
                if near <= 0.0 {
                    continue;
                }
                // Mixes come in steps, so the drawing cache keeps a few brushes, not one per frame.
                let share = (a.radius / (a.radius + b.radius) * 6.0).round() / 6.0;
                let c = colour(a.tint).mix(colour(b.tint), 1.0 - share);
                let at = share.clamp(0.3, 0.7);
                let (mx, my) = (a.x + dx * at, a.y + dy * at);
                let angle = dy.atan2(dx).to_degrees();
                let thick = a.radius.min(b.radius) * (0.45 + 0.35 * near);
                g.rotated(angle, mx, my, || g.blob(mx, my, d * 0.6, thick, c.alpha(light * near * 0.8)));
            }
        }
        for (i, b) in self.blobs.iter().enumerate() {
            let c = colour(b.tint);
            // Three lobes turning at different speeds and breathing in size.
            for k in 0..3 {
                let kf = k as f32;
                let spin = b.phase + kf * 2.1 + t * (0.12 + 0.07 * kf) * if k % 2 == 0 { 1.0 } else { -1.0 };
                let off = b.radius * (0.22 + 0.08 * (t * 0.31 + kf + b.phase).sin());
                let (lx, ly) = (b.x + spin.cos() * off, b.y + spin.sin() * off);
                let rx = b.radius * (0.78 + 0.16 * (t * 0.43 + kf * 1.7 + b.phase).sin());
                let ry = b.radius * (0.52 + 0.14 * (t * 0.37 + kf * 2.3 + b.phase).cos());
                g.rotated(spin.to_degrees(), lx, ly, || g.blob(lx, ly, rx, ry, c.alpha(light * 0.5)));
            }
            // The light reaches toward its nearest neighbour.
            let nearest = self
                .blobs
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, o)| (o.x - b.x, o.y - b.y))
                .min_by(|p, q| (p.0 * p.0 + p.1 * p.1).total_cmp(&(q.0 * q.0 + q.1 * q.1)));
            if let Some((dx, dy)) = nearest {
                let d = (dx * dx + dy * dy).sqrt().max(1.0);
                let reach = (1.0 - d / (b.radius * 2.6)).clamp(0.0, 1.0);
                if reach > 0.0 {
                    let (lx, ly) = (b.x + dx / d * b.radius * 0.45 * reach, b.y + dy / d * b.radius * 0.45 * reach);
                    g.rotated(dy.atan2(dx).to_degrees(), lx, ly, || g.blob(lx, ly, b.radius * 0.8, b.radius * 0.4, c.alpha(light * 0.4 * reach)));
                }
            }
        }
    }
}

/// The colours behind everything: a base and three soft lights drawn from the
/// accent. When the look or accent changes, each eases into its new colour, so
/// the whole window morphs rather than switches.
#[derive(Clone, Copy)]
struct Wash {
    base: Color,
    a: Color,
    b: Color,
    c: Color,
}

impl Wash {
    fn of(t: &Theme) -> Self {
        Self { base: Color { a: 1.0, ..t.body }, a: t.accent, b: hue_shift(t.accent, 40.0), c: hue_shift(t.accent, -50.0) }
    }

    /// Move a share `k` of the way to `to`; true while still visibly apart.
    fn approach(&mut self, to: &Self, k: f32) -> bool {
        let mut apart = false;
        for (from, to) in [(&mut self.base, to.base), (&mut self.a, to.a), (&mut self.b, to.b), (&mut self.c, to.c)] {
            *from = from.mix(to, k);
            let gap = (from.r - to.r).abs() + (from.g - to.g).abs() + (from.b - to.b).abs();
            if gap < 0.004 {
                *from = to;
            } else {
                apart = true;
            }
        }
        apart
    }
}

/// The same colour with its hue turned by `degrees`, lightness and saturation kept.
fn hue_shift(c: Color, degrees: f32) -> Color {
    let (max, min) = (c.r.max(c.g).max(c.b), c.r.min(c.g).min(c.b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-4 {
        return c;
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == c.r {
        ((c.g - c.b) / d).rem_euclid(6.0)
    } else if max == c.g {
        (c.b - c.r) / d + 2.0
    } else {
        (c.r - c.g) / d + 4.0
    } * 60.0;
    let h = (h + degrees).rem_euclid(360.0) / 60.0;
    let chroma = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = chroma * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let m = l - chroma / 2.0;
    Color { r: r + m, g: g + m, b: b + m, a: c.a }
}

/// A frosted-glass surface: a translucent, top-lit fill with a bright hairline
/// rim, so the coloured light behind shows through, softened. No shadow: the
/// surfaces float in the light, nothing dark gathers under them.
/// `lift` (0 to 1) brightens it under the pointer. Solid when Windows'
/// transparency effects are off.
fn glass(g: &Gfx, r: Rect, radius: f32, t: &Theme, clear: bool, lift: f32) {
    glass_face(g, r, radius, t, clear, lift);
}

/// The glass itself, without the shadow beneath it.
fn glass_face(g: &Gfx, r: Rect, radius: f32, t: &Theme, clear: bool, lift: f32) {
    let white = Color::rgb(0xffffff);
    if !clear {
        g.fill_rect(r, radius, if t.dark { Color::rgb(0x1d1d21).mix(Color::rgb(0x2a2a30), lift) } else { white });
        g.stroke_rect(r.inset(0.5, 0.5), radius, t.rim, 1.0);
        return;
    }
    let (top, bottom) = if t.dark { (0.12 + 0.05 * lift, 0.05 + 0.03 * lift) } else { (0.74 + 0.12 * lift, 0.52 + 0.12 * lift) };
    g.fill_rect_gradient_v(r, radius, r.y, r.bottom(), white.alpha(top), white.alpha(bottom));
    if t.dark {
        g.stroke_rect(r.inset(0.5, 0.5), radius, white.alpha(0.14 + 0.10 * lift), 1.0);
    } else {
        g.stroke_rect(r.inset(0.5, 0.5), radius, Color::rgb(0x000000).alpha(0.07), 1.0);
        g.stroke_rect(r.inset(1.5, 1.5), radius - 1.0, white.alpha(0.9), 1.0);
    }
}

/// A button that lights up under the pointer. The primary one is solid dark,
/// and its light runs only along its border: a bright arc in the accent's
/// colours that follows the pointer around the edge. The other is glass, with
/// a soft spotlight inside it. Nothing spills outside either one.
#[allow(clippy::too_many_arguments)]
fn glow_button(ui: &mut Ui, name: u32, label: &str, r: Rect, wash: &Wash, t: &Theme, primary: bool, clear: bool) -> bool {
    let hit = ui.hit(name, r);
    let (hover, press) = (hit.hover.clamp(0.0, 1.0), hit.press.clamp(0.0, 1.0));
    let r = r.scaled(1.0 + 0.02 * hover - 0.04 * press);
    let (cx, cy) = r.center();
    let g = ui.g;
    let white = Color::rgb(0xffffff);
    let radius = r.h / 2.0;
    let (px, py) = (ui.input.x.clamp(r.x, r.right()), ui.input.y.clamp(r.y, r.bottom()));
    if primary {
        const BORDER: f32 = 1.5;
        let dark = Color::rgb(0x0b0b0e);
        // The rim: a faint resting line, then the pointer's light, then the
        // dark face drawn over everything but the outer edge.
        g.fill_rect(r, radius, white.alpha(0.16));
        if hover > 0.01 {
            g.push_round_clip(r, radius);
            g.glow(px, py, 90.0, 60.0, wash.a.alpha(hover));
            g.glow(px, py, 46.0, 34.0, wash.b.mix(white, 0.3).alpha(hover));
            g.pop_clip();
        }
        g.fill_rect(r.inset(BORDER, BORDER), radius - BORDER, dark);
        g.text(label, r, 13.5, 600, Align::Center, white);
    } else {
        glass_face(g, r, radius, t, clear, hover);
        if hover > 0.01 {
            g.push_round_clip(r, radius);
            g.glow(cx, r.bottom(), r.w * 0.6, r.h * 0.7, wash.a.alpha(0.16 * hover));
            g.glow(px, cy, 54.0, r.h * 1.1, wash.a.alpha(0.22 * hover));
            g.pop_clip();
        }
        g.text(label, r, 13.5, 600, Align::Center, t.text);
    }
    hit.clicked
}

/// What a frame asks the window procedure to do afterwards.
enum After {
    Nothing,
    Finish,
}

thread_local! {
    static WELCOME: RefCell<Option<Welcome>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Welcome) -> R) -> Option<R> {
    WELCOME.with(|cell| cell.try_borrow_mut().ok().and_then(|mut w| w.as_mut().map(f)))
}

/// Label and short line under it for each step.
fn heading(step: usize) -> (&'static str, &'static str) {
    match step {
        0 => ("Namaste.", "Glide lives at the top of your screen. Here's what it brings."),
        1 => ("Pick a look", "Glide follows it everywhere, from the lip to the pages."),
        2 => ("Where are you?", "For the weather. Leave it empty to use your PC's location."),
        3 => ("A few choices", "You can change all of these later from the tray icon."),
        _ => ("You're all set", "A few things to try first.")
    }
}

/// What the island at the top of the window says.
fn island_word(_step: usize) -> &'static str {
    "Glide"
}

fn switch(ui: &mut Ui, name: u32, r: Rect, on: bool, wash: &Wash) -> bool {
    let t = ui.t;
    let hit = ui.hit(name, r);
    let track = Rect::new(r.right() - 44.0, r.y + (r.h - 26.0) / 2.0, 44.0, 26.0);
    if on {
        ui.g.fill_rect_gradient(track, 13.0, track.x, track.right(), wash.a, wash.b);
    } else {
        ui.g.fill_rect(track, 13.0, t.track.mix(t.control_hover, hit.hover));
    }
    let knob_x = if on { track.right() - 13.0 } else { track.x + 13.0 };
    ui.g.fill_circle(knob_x, track.y + 13.0, 10.5 - 1.0 * hit.press, Color::rgb(0xffffff));
    hit.clicked
}

impl Welcome {
    fn theme(&self) -> Theme {
        Theme::resolve(self.choices.appearance, self.choices.accent)
    }

    fn go(&mut self, step: usize) {
        crate::island::trace(&format!("welcome: step {} -> {step}", self.step));
        let forward = step > self.step;
        self.step = step.min(STEPS - 1);
        self.reveal.snap(0.0);
        self.slide.snap(if forward { 36.0 } else { -36.0 });
        self.kick();
    }

    /// Something will move: draw at the display's pace until it settles.
    fn kick(&self) {
        // SAFETY: a timer on our own window.
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_FRAME, 15, None);
        }
    }

    fn frame(&mut self) -> After {
        let now = Instant::now();
        let dt = self.last_frame.map_or(1.0 / 60.0, |at| (now - at).as_secs_f32().clamp(0.001, 0.05));
        self.last_frame = Some(now);
        let t = self.theme();
        let (w, h) = SIZE;
        let cx = w / 2.0;

        let word = island_word(self.step);
        self.lip_w.set(self.gfx.measure(word, 14.0, 700) + 70.0);
        self.lip_h.set(34.0);
        self.reveal.set(1.0);
        self.slide.set(0.0);
        let mut moving = false;
        for spring in [&mut self.lip_w, &mut self.lip_h, &mut self.reveal, &mut self.slide] {
            spring.step(dt);
            moving |= !spring.settled();
        }
        // The backdrop eases toward the chosen look and accent.
        let target = Wash::of(&t);
        if self.still {
            self.wash = target;
        } else {
            moving |= self.wash.approach(&target, 1.0 - (-7.0 * dt).exp());
        }
        let wash = self.wash;

        let g = &self.gfx;
        g.begin();
        // The page: the theme's surface lit by three soft lights in the accent's
        // family. The glass surfaces above pick them up.
        g.fill_rect(Rect::new(0.0, 0.0, w, h), 0.0, wash.base);
        if !self.still {
            self.field.step(dt);
            moving = true;
        }
        self.field.draw(g, &wash, if t.dark { 0.26 } else { 0.22 });

        // The island, hanging from the top edge, as it does on the screen.
        let lip = Shape { w: self.lip_w.value, h: self.lip_h.value, r: 17.0, flare: 12.0, slant: 3.0, bulge: 0.0, lean: 0.0 };
        if let Some(path) = lip.path(g, cx) {
            g.fill_path(&path, Color::rgb(0x000000));
        }
        g.text(word, Rect::new(cx - 100.0, 0.0, 200.0, lip.h), 14.0, 700, Align::Center, Color::rgb(0xffffff).mix(t.accent, 0.0));

        let mut actions: Vec<Action> = Vec::new();
        self.controls.begin_frame();
        let mut after = After::Nothing;
        let mut go_to = None;
        {
            let reveal = self.reveal.value.clamp(0.0, 1.0);
            let mut ui = Ui {
                g,
                t: &t,
                input: &self.input,
                controls: &mut self.controls,
                actions: &mut actions,
                dt,
                interactive: reveal > 0.5,
            };
            // Heading and content ease in from the side they were reached from.
            g.set_opacity(reveal);
            g.set_transform(1.0, 0.0, 0.0, self.slide.value);
            let (title, line) = heading(self.step);
            if self.step == 0 {
                // The greeting is handwritten, as it is on the island.
                ui.g.script(title, Rect::new(40.0, 84.0, w - 80.0, 60.0), 40.0, Align::Center, t.text);
            } else {
                ui.label(title, Rect::new(40.0, 96.0, w - 80.0, 44.0), 34.0, 700, Align::Center, t.text);
            }
            ui.label(line, Rect::new(40.0, 142.0, w - 80.0, 22.0), 14.0, 500, Align::Center, t.secondary);
            Self::content(self.step, &mut self.choices, self.can_instant, self.caret_on, self.clear, &wash, &mut ui, &t);
            g.set_transform(1.0, 0.0, 0.0, 0.0);
            g.set_opacity(1.0);

            // Footer: progress dots, a quiet way back, and one clear way on.
            let y = h - 58.0;
            let gap = 14.0;
            for i in 0..STEPS {
                let x = cx - gap * (STEPS as f32 - 1.0) / 2.0 + gap * i as f32;
                if i == self.step {
                    ui.g.fill_rect_gradient(Rect::new(x - 8.0, y + 16.0, 16.0, 5.0), 2.5, x - 8.0, x + 8.0, wash.a, wash.b);
                } else {
                    ui.g.fill_circle(x, y + 18.5, 2.5, t.tertiary);
                }
            }
            let back = if self.step == 0 { "Skip setup" } else { "Back" };
            let back_w = ui.g.measure(back, 13.5, 600) + 40.0;
            if glow_button(&mut ui, id("welcome.back"), back, Rect::new(36.0, y, back_w, 38.0), &wash, &t, false, self.clear) {
                if self.step == 0 {
                    after = After::Finish;
                } else {
                    go_to = Some(self.step - 1);
                }
            }
            let next = match self.step {
                0 => "Let's go",
                s if s == STEPS - 1 => "Start using Glide",
                _ => "Continue",
            };
            let next_w = ui.g.measure(next, 13.5, 600) + 52.0;
            if glow_button(&mut ui, id("welcome.next"), next, Rect::new(w - 36.0 - next_w, y, next_w, 38.0), &wash, &t, true, self.clear) {
                if self.step == STEPS - 1 {
                    after = After::Finish;
                } else {
                    go_to = Some(self.step + 1);
                }
            }
            moving |= ui.controls.moving;
        }
        self.controls.end_frame(&self.input);
        g.end(1);
        (self.input.pressed, self.input.released, self.input.wheel) = (false, false, 0.0);
        if let Some(step) = go_to {
            self.go(step);
            moving = true;
        }
        // SAFETY: a timer on our own window.
        unsafe {
            if !moving {
                let _ = KillTimer(Some(self.hwnd), TIMER_FRAME);
                self.last_frame = None;
            }
        }
        after
    }

    /// The middle of each step. A free function, so it can borrow the choices
    /// while the toolkit borrows the rest of the window.
    #[allow(clippy::too_many_arguments)]
    fn content(step: usize, choices: &mut Choices, can_instant: bool, caret_on: bool, clear: bool, wash: &Wash, ui: &mut Ui, t: &Theme) {
        let (w, _) = SIZE;
        let cx = w / 2.0;
        // A plain line icon in the text colour: no disc, no glow.
        let badge = |ui: &mut Ui, glyph: char, x: f32, y: f32, r: f32| {
            ui.g.icon(glyph, x, y, r * 1.15, t.text);
        };
        match step {
            0 => {
                // Everything on the island, one glass tile each.
                let items = [
                    ('\u{E8D6}', "Music", "Play, pause, skip"),
                    ('\u{EA8F}', "Notifications", "Latest at a glance"),
                    ('\u{E787}', "Calendar", "With your holidays"),
                    ('\u{E753}', "Weather", "Today and the week"),
                    ('\u{E916}', "Timers", "Pomodoro and laps"),
                    ('\u{E8B7}', "Shelf", "Share files by code"),
                ];
                let (tile_w, tile_h, gap) = (170.0, 84.0, 12.0);
                let x0 = cx - (tile_w * 3.0 + gap * 2.0) / 2.0;
                for (i, (glyph, title, line)) in items.into_iter().enumerate() {
                    let tile = Rect::new(x0 + (tile_w + gap) * (i % 3) as f32, 182.0 + (tile_h + gap) * (i / 3) as f32, tile_w, tile_h);
                    let hit = ui.hit(id("welcome.feature") ^ i as u32, tile);
                    glass(ui.g, tile, 18.0, t, clear, hit.hover.clamp(0.0, 1.0));
                    badge(ui, glyph, tile.x + 28.0, tile.y + tile.h / 2.0, 16.0);
                    ui.label(title, Rect::new(tile.x + 52.0, tile.y + 22.0, tile.w - 58.0, 20.0), 13.5, 600, Align::Left, t.text);
                    ui.label(line, Rect::new(tile.x + 52.0, tile.y + 42.0, tile.w - 58.0, 18.0), 11.5, 500, Align::Left, t.secondary);
                }
            }
            1 => {
                // Appearance: three small previews, each on glass.
                let looks = [(Appearance::System, "Match Windows"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")];
                let (card_w, gap) = (136.0, 16.0);
                let x0 = cx - (card_w * 3.0 + gap * 2.0) / 2.0;
                for (i, (look, label)) in looks.into_iter().enumerate() {
                    let card = Rect::new(x0 + (card_w + gap) * i as f32, 182.0, card_w, 84.0);
                    let hit = ui.hit(id("welcome.look") ^ i as u32, card);
                    let on = choices.appearance == look;
                    let preview = match look {
                        Appearance::Light => Theme::light(),
                        Appearance::Dark => Theme::dark(),
                        Appearance::System => if crate::theme::system_prefers_dark() { Theme::dark() } else { Theme::light() },
                    };
                    glass(ui.g, card, 16.0, t, clear, hit.hover.clamp(0.0, 1.0));
                    let inner = card.inset(6.0, 6.0);
                    ui.g.fill_rect(inner, 11.0, preview.body.mix(preview.control, 0.6).alpha(0.92));
                    // A tiny island in each preview.
                    ui.g.fill_rect(Rect::new(inner.x + inner.w / 2.0 - 28.0, inner.y + 6.0, 56.0, 13.0), 6.5, Color::rgb(0x000000));
                    ui.g.fill_rect_gradient(Rect::new(inner.x + 12.0, inner.y + 32.0, inner.w - 24.0, 6.0), 3.0, inner.x, inner.right(), wash.a, wash.b);
                    ui.g.fill_rect(Rect::new(inner.x + 12.0, inner.y + 46.0, inner.w - 56.0, 6.0), 3.0, preview.secondary.alpha(0.35));
                    if on {
                        ui.g.stroke_rect(card.inset(0.5, 0.5), 16.0, wash.a, 2.0);
                    }
                    ui.label(label, Rect::new(card.x, card.bottom() + 6.0, card.w, 20.0), 12.5, 600, Align::Center, if on { t.text } else { t.secondary });
                    if hit.clicked {
                        choices.appearance = look;
                    }
                }
                // Accent colours, on a glass strip.
                let gap = 38.0;
                let strip_w = gap * ACCENTS.len() as f32 + 12.0;
                glass(ui.g, Rect::new(cx - strip_w / 2.0, 312.0, strip_w, 40.0), 20.0, t, clear, 0.0);
                let x0 = cx - gap * (ACCENTS.len() as f32 - 1.0) / 2.0;
                for (i, (_, dark, light)) in ACCENTS.iter().enumerate() {
                    let x = x0 + gap * i as f32;
                    let hit = ui.hit(id("welcome.accent") ^ i as u32, Rect::new(x - 15.0, 317.0, 30.0, 30.0));
                    let on = choices.accent == i;
                    let swatch = Color::rgb(if t.dark { *dark } else { *light });
                    let hover = hit.hover.clamp(0.0, 1.0);
                    if on {
                        ui.g.fill_circle(x, 332.0, 15.0, t.text);
                        ui.g.fill_circle(x, 332.0, 12.5, wash.base);
                    }
                    ui.g.fill_circle(x, 332.0, 10.0 + 1.5 * hover - 1.5 * hit.press, swatch);
                    if hit.clicked {
                        choices.accent = i;
                    }
                }
            }
            2 => {
                // The city: one glass field, typed straight into, glowing while it has focus.
                let field = Rect::new(cx - 190.0, 202.0, 380.0, 50.0);
                glass(ui.g, field, 16.0, t, clear, 0.3);
                ui.g.stroke_rect(field.inset(0.75, 0.75), 16.0, wash.a, 1.5);
                ui.g.icon('\u{E707}', field.x + 24.0, field.y + 25.0, 16.0, wash.a);
                let text_r = Rect::new(field.x + 46.0, field.y, field.w - 64.0, field.h);
                if choices.city.is_empty() {
                    ui.label("Type your city, like Chennai", text_r, 15.0, 500, Align::Left, t.tertiary);
                } else {
                    ui.label(&choices.city, text_r, 15.0, 600, Align::Left, t.text);
                }
                if caret_on {
                    let x = text_r.x + ui.g.measure(&choices.city, 15.0, 600) + 1.0;
                    ui.g.fill_rect(Rect::new(x, field.y + 14.0, 2.0, 22.0), 1.0, wash.a);
                }
                ui.label("Press Enter to continue", Rect::new(40.0, 266.0, SIZE.0 - 80.0, 18.0), 12.0, 500, Align::Center, t.secondary);
            }
            3 => {
                let rows: [(&str, &str, u8); 3] = [
                    ("Instant notifications", "Asks Windows for permission once, as administrator", 0),
                    ("Privacy mode", "Show who a message is from, never what it says", 1),
                    ("Start with Windows", "Glide is there whenever you sign in", 2),
                ];
                for (i, (label, detail, which)) in rows.into_iter().enumerate() {
                    let row = Rect::new(cx - 230.0, 184.0 + 58.0 * i as f32, 460.0, 50.0);
                    let already = which == 0 && crate::modules::notify::MODE.load(std::sync::atomic::Ordering::Relaxed) == 1;
                    let available = which != 0 || can_instant;
                    let on = match which {
                        0 => choices.instant && available,
                        1 => choices.privacy,
                        _ => choices.autostart,
                    };
                    glass(ui.g, row, 16.0, t, clear, 0.0);
                    let detail = if already {
                        "Already on: notifications arrive as they happen"
                    } else if available {
                        detail
                    } else {
                        "Available in the installed app"
                    };
                    ui.label(label, Rect::new(row.x + 18.0, row.y + 7.0, row.w - 90.0, 20.0), 14.0, 600, Align::Left, if available || already { t.text } else { t.tertiary });
                    ui.label(detail, Rect::new(row.x + 18.0, row.y + 26.0, row.w - 90.0, 18.0), 11.5, 500, Align::Left, t.secondary);
                    if already {
                        ui.g.icon('\u{E73E}', row.right() - 32.0, row.y + row.h / 2.0, 16.0, t.green);
                    } else if available && switch(ui, id("welcome.opt") ^ i as u32, row.inset(14.0, 0.0), on, wash) {
                        match which {
                            0 => choices.instant = !choices.instant,
                            1 => choices.privacy = !choices.privacy,
                            _ => choices.autostart = !choices.autostart,
                        }
                    }
                }
            }
            _ => {
                let tips = [
                    ('\u{E70E}', "Point at the top of your screen to see the island"),
                    ('\u{E8AB}', "Click it to open, swipe sideways to switch pages"),
                    ('\u{E916}', "Scroll on the timer's numbers to set it"),
                    ('\u{E8B7}', "Drop files on the shelf, share them with a code"),
                    ('\u{E713}', "Right-click the tray icon for settings"),
                ];
                let panel = Rect::new(cx - 230.0, 178.0, 460.0, 184.0);
                glass(ui.g, panel, 20.0, t, clear, 0.0);
                for (i, (glyph, tip)) in tips.into_iter().enumerate() {
                    let y = panel.y + 12.0 + 32.0 * i as f32;
                    badge(ui, glyph, panel.x + 30.0, y + 16.0, 11.0);
                    ui.label(tip, Rect::new(panel.x + 52.0, y + 4.0, panel.w - 64.0, 24.0), 13.5, 500, Align::Left, t.text);
                }
            }
        }
    }

    /// Save the choices and tell the island.
    fn finish(&self) {
        crate::island::trace(&format!("welcome: finish at step {} (instant {})", self.step, self.choices.instant));
        let mut settings = Settings::load();
        settings.appearance = self.choices.appearance;
        settings.accent = self.choices.accent;
        settings.city = self.choices.city.trim().to_string();
        settings.privacy = self.choices.privacy;
        settings.autostart = self.choices.autostart;
        settings.welcomed = true;
        settings.save();
        crate::tray::set_autostart(self.choices.autostart);
        let instant = self.choices.instant && self.can_instant;
        // SAFETY: posting to the island's window.
        unsafe {
            let _ = PostMessageW(Some(self.island), WM_WELCOME_DONE, WPARAM(usize::from(instant)), LPARAM(0));
        }
    }

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<(LRESULT, After)> {
        let point = || {
            let (x, y) = ((lparam.0 & 0xffff) as i16 as f32, ((lparam.0 >> 16) & 0xffff) as i16 as f32);
            (x / self.gfx.scale, y / self.gfx.scale)
        };
        let done = |after| Some((LRESULT(0), after));
        match msg {
            WM_PAINT => {
                let after = self.frame();
                // SAFETY: our own window.
                unsafe {
                    let _ = ValidateRect(Some(self.hwnd), None);
                }
                done(after)
            }
            WM_MOUSEMOVE => {
                (self.input.x, self.input.y) = point();
                if !self.input.inside {
                    // Hear when the pointer leaves, so hover glows fade out.
                    let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.hwnd, dwHoverTime: 0 };
                    // SAFETY: a valid request for our own window.
                    unsafe {
                        let _ = TrackMouseEvent(&mut track);
                    }
                }
                self.input.inside = true;
                done(self.frame())
            }
            MOUSE_LEFT => {
                self.input.inside = false;
                (self.input.x, self.input.y) = (-1000.0, -1000.0);
                self.kick();
                done(self.frame())
            }
            WM_LBUTTONDOWN => {
                (self.input.x, self.input.y) = point();
                (self.input.down, self.input.pressed) = (true, true);
                // SAFETY: capture on our own window.
                unsafe {
                    SetCapture(self.hwnd);
                }
                done(self.frame())
            }
            WM_LBUTTONUP => {
                crate::island::trace(&format!("welcome: click at {:?} inside {}", point(), self.input.inside));
                (self.input.x, self.input.y) = point();
                (self.input.down, self.input.released) = (false, true);
                // SAFETY: releases the capture taken above.
                unsafe {
                    let _ = ReleaseCapture();
                }
                done(self.frame())
            }
            WM_NCHITTEST => {
                // The top band and the empty page drag the window; controls stay clickable.
                let mut p = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
                // SAFETY: converts in place for our own window.
                unsafe {
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.hwnd, &mut p);
                }
                let y = p.y as f32 / self.gfx.scale;
                Some((LRESULT(if y < 80.0 { HTCAPTION } else { HTCLIENT } as isize), After::Nothing))
            }
            WM_CHAR => {
                let c = wparam.0 as u32;
                match c {
                    13 => {
                        if self.step == STEPS - 1 {
                            return done(After::Finish);
                        }
                        self.go(self.step + 1);
                    }
                    27 => return done(After::Finish),
                    8 if self.step == 2 => {
                        self.choices.city.pop();
                    }
                    22 if self.step == 2 => {
                        // Ctrl+V: paste the first line of text on the clipboard.
                        if let Some(crate::modules::shelf::Item::Text(text)) =
                            crate::shell::ole::read_clipboard(self.hwnd).into_iter().next()
                        {
                            let line = text.lines().next().unwrap_or_default().trim().to_string();
                            self.choices.city.push_str(&line.chars().take(60).collect::<String>());
                        }
                    }
                    c if c >= 32 && self.step == 2 && self.choices.city.chars().count() < 60 => {
                        if let Some(ch) = char::from_u32(c) {
                            self.choices.city.push(ch);
                        }
                    }
                    _ => return done(After::Nothing),
                }
                self.caret_on = true;
                done(self.frame())
            }
            WM_TIMER => {
                if wparam.0 == TIMER_CARET {
                    self.caret_on = self.step == 2 && !self.caret_on;
                }
                done(self.frame())
            }
            WM_ACTIVATE => {
                // SAFETY: keyboard focus to our own window.
                unsafe {
                    let _ = SetFocus(Some(self.hwnd));
                }
                None
            }
            _ => None,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DESTROY {
        // Drop the drawing objects once nothing is borrowing them.
        WELCOME.with(|cell| cell.borrow_mut().take());
        return LRESULT(0);
    }
    match with(|w| w.handle(msg, wparam, lparam)).flatten() {
        Some((result, After::Finish)) => {
            with(|w| w.finish());
            let _ = DestroyWindow(hwnd);
            result
        }
        Some((result, After::Nothing)) => result,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Open the setup window, centred on the primary display.
pub fn open(island: HWND, scale: f32, can_instant: bool) {
    let settings = Settings::load();
    // SAFETY: standard window creation on the UI thread.
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: w!("GlideWelcome"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassW(&class);
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let area = info.rcWork;
        let (w, h) = ((SIZE.0 * scale) as i32, (SIZE.1 * scale) as i32);
        let (x, y) = ((area.left + area.right - w) / 2, (area.top + area.bottom - h) / 2);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_APPWINDOW,
            w!("GlideWelcome"),
            w!("Welcome to Glide"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            crate::island::trace("welcome: window not created");
            return;
        };
        // Rounded corners and a shadow, as Windows draws them for its own windows.
        #[link(name = "dwmapi")]
        extern "system" {
            fn DwmSetWindowAttribute(hwnd: isize, attribute: u32, value: *const u32, size: u32) -> i32;
        }
        DwmSetWindowAttribute(hwnd.0 as isize, 33, &2u32, 4);
        crate::island::trace("welcome: window created");
        let Ok(gfx) = Gfx::new(hwnd, SIZE, scale).inspect_err(|e| crate::island::trace(&format!("welcome: no graphics: {e:?}"))) else {
            let _ = DestroyWindow(hwnd);
            return;
        };
        gfx.set_origin(0.0, 0.0);
        let mut lip_w = Spring::new(60.0, GENTLE);
        lip_w.snap(60.0);
        let welcome = Welcome {
            hwnd,
            island,
            gfx,
            controls: Controls::default(),
            input: Input::default(),
            step: 0,
            choices: Choices {
                appearance: settings.appearance,
                accent: settings.accent,
                city: settings.city.clone(),
                instant: can_instant,
                privacy: settings.privacy,
                autostart: true,
            },
            can_instant,
            reveal: Spring::new(0.0, SMOOTH),
            slide: Spring::new(24.0, SMOOTH),
            lip_w,
            lip_h: Spring::new(4.0, GENTLE),
            last_frame: None,
            caret_on: true,
            wash: Wash::of(&Theme::resolve(settings.appearance, settings.accent)),
            clear: system_transparency(),
            still: reduce_motion(),
            field: Field::new(),
        };
        WELCOME.with(|cell| *cell.borrow_mut() = Some(welcome));
        let _ = ShowWindow(hwnd, SW_SHOW);
        bring_forward(hwnd);
        SetTimer(Some(hwnd), TIMER_CARET, 530, None);
        with(|w| {
            w.kick();
            w.frame()
        });
    }
}

/// Windows' "Animation effects" setting is off.
fn reduce_motion() -> bool {
    let mut on = windows::core::BOOL(1);
    // SAFETY: `on` is a valid BOOL out-value for this query.
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some((&mut on as *mut windows::core::BOOL).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    !on.as_bool()
}

/// Put the setup window in front of whatever is open. Windows refuses a plain
/// request from a program the user did not just click, so: lift it above all
/// windows and back (this always works), then claim focus with the same
/// keyboard nudge Windows itself accepts as user activity.
unsafe fn bring_forward(hwnd: HWND) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_MENU};
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW;
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
    let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
    keybd_event(VK_MENU.0 as u8, 0, Default::default(), 0);
    keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
    let _ = SetForegroundWindow(hwnd);
}