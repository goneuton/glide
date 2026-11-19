//! Incoming WhatsApp calls.
//!
//! A call reaches Glide two ways: as a notification ("Incoming voice call")
//! and, on some versions, as a media session WhatsApp opens while it rings.
//! Either one starts the call pill. It stays out, over any app, until the call
//! is picked up from it, or the call stops ringing: the caller hangs up (the
//! notification is removed, or the session closes), it is answered elsewhere,
//! or a minute passes.

use crate::island::{trace, Activity, Event};
use crate::shell::render::{Align, Gfx, Image, Rect};
use crate::shell::ui::{id, Action, Ui};
use crate::theme::{Color, Theme};
use std::time::{Duration, Instant};

pub fn is_whatsapp(app: &str) -> bool {
    app.to_lowercase().contains("whatsapp")
}

/// Does this notification announce a call rather than a message?
pub fn is_call_note(app: &str, title: &str, body: &str) -> bool {
    // One of the two lines must be a short call label ("Incoming voice call",
    // "Video call", "Priya is calling..."), so a message that merely mentions a
    // call ("can you call me?") is not taken for one.
    let label = |line: &str| {
        let line = line.trim().trim_end_matches(['.', '\u{2026}']).to_lowercase();
        line.len() <= 40
            && !line.contains("missed")
            && ((line.contains("incoming") && line.contains("call"))
                || line.contains("voice call")
                || line.contains("video call")
                || line.ends_with("calling")
                || line.ends_with("ringing"))
    };
    is_whatsapp(app) && (label(title) || label(body))
}

/// A ring nobody answers or ends is let go after this long.
const UNANSWERED: Duration = Duration::from_secs(60);
/// After Pick, the same ring is not shown again for this long.
const PICKED_QUIET: Duration = Duration::from_secs(8);
/// One ring: the phone shakes for the first part, then rests.
const RING: f32 = 1.3;

const WHATSAPP_GREEN: Color = Color::rgb(0x25d366);

struct Ringing {
    caller: String,
    video: bool,
    since: Instant,
    /// The notification that announced it, if any: its removal ends the ring.
    note: Option<u32>,
}

pub struct Call {
    ringing: Option<Ringing>,
    logo: Option<Image>,
    /// WhatsApp currently has a media session open.
    session: bool,
    /// When the user last picked a call up from Glide.
    picked: Option<Instant>,
}

/// The caller, from a call notification's two lines. WhatsApp writes either
/// "Name" / "Incoming voice call" or "Incoming voice call" / "Name".
fn caller_of(title: &str, body: &str) -> String {
    let is_label = |s: &str| {
        let s = s.to_lowercase();
        s.is_empty() || is_whatsapp(&s) || s.contains("call") || s.contains("ringing")
    };
    if !is_label(title) {
        title.to_string()
    } else if !is_label(body) {
        body.to_string()
    } else {
        String::new()
    }
}

impl Call {
    pub fn new() -> Self {
        Self { ringing: None, logo: None, session: false, picked: None }
    }

    fn start(&mut self, caller: String, video: bool, note: Option<u32>, t: &Theme) -> Option<Color> {
        if self.picked.is_some_and(|at| at.elapsed() < PICKED_QUIET) {
            return None;
        }
        let fresh = self.ringing.is_none();
        let ringing =
            self.ringing.get_or_insert(Ringing { caller: String::new(), video, since: Instant::now(), note: None });
        if !caller.trim().is_empty() {
            ringing.caller = caller;
        }
        ringing.video |= video;
        if note.is_some() {
            ringing.note = note;
        }
        trace(&format!("call: ringing (fresh {fresh}, video {})", ringing.video));
        fresh.then_some(t.green)
    }

    fn end(&mut self, why: &str) {
        if self.ringing.take().is_some() {
            trace(&format!("call: ended ({why})"));
        }
    }

    fn logo(&self, ui: &Ui, cx: f32, cy: f32, radius: f32) {
        match &self.logo {
            Some(image) => ui.g.image(image, Rect::new(cx - radius, cy - radius, radius * 2.0, radius * 2.0), radius * 0.45),
            None => {
                ui.g.fill_circle(cx, cy, radius, WHATSAPP_GREEN);
                ui.g.icon('\u{E717}', cx, cy, radius, Color::rgb(0xffffff));
            }
        }
    }
}

impl Activity for Call {
    fn name(&self) -> &'static str {
        "call"
    }

    fn sticky(&self) -> bool {
        self.ringing.is_some()
    }

    fn compact_width(&self, g: &Gfx) -> f32 {
        let caller = self.ringing.as_ref().map_or("WhatsApp", |r| r.caller.as_str());
        (g.measure(caller, 13.0, 600).min(170.0) + 150.0).max(230.0)
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let Some(ringing) = &self.ringing else { return };
        let t = ui.t;
        let mid = r.y + r.h / 2.0;
        // Who is calling, and through what.
        self.logo(ui, r.x + 9.0, mid, 11.0);
        let caller = if ringing.caller.is_empty() { "WhatsApp call" } else { ringing.caller.as_str() };
        ui.label(caller, Rect::new(r.x + 28.0, r.y, r.w - 124.0, r.h), 13.0, 600, Align::Left, t.text);

        // Ringing: the handset shakes in short bursts, like a phone on a table.
        // A video call shows a camera instead, still.
        let icon_x = r.right() - 78.0;
        if ringing.video {
            ui.g.icon('\u{E714}', icon_x, mid, 14.0, WHATSAPP_GREEN);
        } else {
            let phase = ringing.since.elapsed().as_secs_f32() % RING;
            let shake = if phase < RING * 0.55 { (phase * 58.0).sin() * 1.8 } else { 0.0 };
            ui.g.icon('\u{E717}', icon_x + shake, mid, 14.0, WHATSAPP_GREEN);
        }

        // Pick: the one action, in the theme's solid colour.
        let pick = Rect::new(r.right() - 54.0, mid - 12.0, 54.0, 24.0);
        let hit = ui.hit(id("call.pick"), pick);
        let pick = pick.scaled(1.0 + 0.04 * hit.hover - 0.08 * hit.press);
        ui.g.fill_rect(pick, pick.h / 2.0, t.primary);
        ui.label("Pick", pick, 12.5, 700, Align::Center, t.on_primary);
        if hit.clicked {
            ui.actions.push(Action::Open("whatsapp:".into()));
            self.picked = Some(Instant::now());
            self.end("picked");
        }
    }

    fn on_event(&mut self, event: &Event, g: &Gfx, t: &Theme) -> Option<Color> {
        match event {
            Event::Notification(note) if note.call => {
                if self.logo.is_none() {
                    self.logo = note.logo.as_ref().and_then(|bytes| g.load_image(bytes, 26.0));
                }
                let video = format!("{} {}", note.title, note.body).to_lowercase().contains("video");
                self.start(caller_of(&note.title, &note.body), video, Some(note.id), t)
            }
            // A WhatsApp message while it rings usually means the call was missed.
            Event::Notification(note) if is_whatsapp(&note.app) && self.ringing.is_some() => {
                let words = format!("{} {}", note.title, note.body).to_lowercase();
                if words.contains("missed") {
                    self.end("missed");
                }
                None
            }
            Event::NotificationRemoved(gone) => {
                // The caller hung up, or it was answered on the phone: WhatsApp takes the ring away.
                if self.ringing.as_ref().is_some_and(|r| r.note == Some(*gone)) {
                    self.end("notification removed");
                }
                None
            }
            Event::Media(state) if is_whatsapp(&state.app) && state.present => {
                let fresh_session = !self.session;
                self.session = true;
                if fresh_session {
                    self.start(state.title.clone(), false, None, t)
                } else {
                    None
                }
            }
            Event::Media(_) if self.session => {
                // WhatsApp's session is gone: the call is over.
                self.session = false;
                self.end("session closed");
                None
            }
            _ => None,
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.ringing.as_ref().map(|r| r.since + UNANSWERED)
    }

    fn due(&mut self, now: Instant, _t: &Theme) -> Option<Color> {
        if self.deadline().is_some_and(|at| now >= at) {
            self.end("unanswered");
        }
        None
    }

    fn animating(&self, _expanded: bool) -> bool {
        // The handset shakes while it rings.
        self.ringing.as_ref().is_some_and(|r| !r.video)
    }
}
