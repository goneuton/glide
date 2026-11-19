//! Notifications from other apps, through Windows' notification listener.
//!
//! Windows only grants this to apps it can identify. When access is refused
//! the module stays silent and says why in the trace; nothing else depends on it.

use crate::island::{trace, Activity, Event, Post};
use crate::shell::render::{Align, Gfx, Image, Rect};
use crate::shell::ui::{id, Action, Ui};
use crate::theme::{Color, Theme};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::Result;
use windows::Foundation::{Size, TypedEventHandler};
use windows::Storage::Streams::DataReader;
use windows::UI::Notifications::Management::{UserNotificationListener, UserNotificationListenerAccessStatus};
use windows::UI::Notifications::{KnownNotificationBindings, NotificationKinds, UserNotification, UserNotificationChangedKind};

/// Privacy mode: show who a notification is from, never what it says.
pub static PRIVATE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How notifications reach Glide, for the tray menu.
/// 0 starting, 1 instant, 2 delayed (no identity), 3 blocked by Windows, 4 off.
pub static MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn set_mode(mode: u8) {
    MODE.store(mode, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Clone, Default)]
pub struct Note {
    pub app: String,
    /// First line: the sender for a message, the subject otherwise.
    pub title: String,
    pub body: String,
    /// The app's logo, encoded.
    pub logo: Option<Arc<Vec<u8>>>,
    /// An incoming call rather than a message.
    pub call: bool,
    /// Windows' id for the notification, to recognise it when it is removed.
    pub id: u32,
}

fn logo_of(notification: &UserNotification) -> Result<Vec<u8>> {
    let reference = notification.AppInfo()?.DisplayInfo()?.GetLogo(Size { Width: 48.0, Height: 48.0 })?;
    let stream = reference.OpenReadAsync()?.get()?;
    let size = stream.Size()? as u32;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(size)?.get()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}

fn read(notification: &UserNotification) -> Result<Note> {
    let app = notification.AppInfo()?.DisplayInfo()?.DisplayName()?.to_string();
    let binding = notification.Notification()?.Visual()?.GetBinding(&KnownNotificationBindings::ToastGeneric()?)?;
    let mut lines = Vec::new();
    for element in binding.GetTextElements()? {
        let text = element.Text()?.to_string();
        if !text.trim().is_empty() {
            lines.push(text.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    let title = lines.first().cloned().unwrap_or_else(|| app.clone());
    let body = lines.get(1..).map(|rest| rest.join(" ")).unwrap_or_default();
    let call = crate::modules::call::is_call_note(&app, &title, &body);
    let id = notification.Id().unwrap_or(0);
    Ok(Note { app, title, body, logo: logo_of(notification).ok().filter(|b| !b.is_empty()).map(Arc::new), call, id })
}

/// Start listening. Posts an [`Event::Notification`] for every new toast.
/// Send `()` on the returned channel when a toast may have appeared, to check now.
pub fn spawn(post: Post) -> std::sync::mpsc::Sender<()> {
    let (nudge_tx, nudge) = channel::<()>();
    let _ = std::thread::Builder::new().name("glide-notify".into()).spawn(move || {
        let listen = || -> Result<()> {
            let listener = UserNotificationListener::Current()?;
            let access = listener.RequestAccessAsync()?.get()?;
            trace(&format!("notifications: access {access:?}"));
            if access != UserNotificationListenerAccessStatus::Allowed {
                set_mode(3);
                return Ok(());
            }
            // (id, true) when a notification arrives, (id, false) when it is removed:
            // a call's notification disappears when the caller hangs up.
            let (tx, rx) = channel::<(u32, bool)>();
            let added = tx.clone();
            let subscribed = listener.NotificationChanged(&TypedEventHandler::new(move |_, args: windows::core::Ref<windows::UI::Notifications::UserNotificationChangedEventArgs>| {
                if let Some(args) = args.as_ref() {
                    let id = args.UserNotificationId()?;
                    match args.ChangeKind()? {
                        UserNotificationChangedKind::Added => {
                            let _ = added.send((id, true));
                        }
                        UserNotificationChangedKind::Removed => {
                            let _ = added.send((id, false));
                        }
                        _ => {}
                    }
                }
                Ok(())
            }));
            set_mode(if subscribed.is_ok() { 1 } else { 2 });
            if let Err(err) = &subscribed {
                // Without an app identity Windows refuses the event. The shell tells us
                // when a toast window appears (see `window.rs`), and the list is read
                // then; a check every eight seconds catches what arrives without one
                // (banners are off, or Do Not Disturb is on).
                trace(&format!("notifications: no change event ({:#x}); checking the list instead", err.code().0));
                let mut seen: std::collections::HashSet<u32> =
                    listener.GetNotificationsAsync(NotificationKinds::Toast)?.get()?.into_iter().filter_map(|n| n.Id().ok()).collect();
                let mut checked = Instant::now();
                loop {
                    if let Err(std::sync::mpsc::RecvTimeoutError::Disconnected) = nudge.recv_timeout(Duration::from_secs(8)) {
                        return Ok(());
                    }
                    // The toast is listed a moment after its window shows.
                    std::thread::sleep(Duration::from_millis(400));
                    while nudge.try_recv().is_ok() {}
                    // Reading the list is not cheap: never more than once in a while.
                    if checked.elapsed() < Duration::from_millis(1500) {
                        continue;
                    }
                    checked = Instant::now();
                    let listed = listener.GetNotificationsAsync(NotificationKinds::Toast)?.get()?;
                    let mut now_seen = std::collections::HashSet::new();
                    for notification in listed {
                        let id = notification.Id()?;
                        now_seen.insert(id);
                        if !seen.contains(&id) {
                            if let Ok(note) = read(&notification) {
                                post.send(Event::Notification(note));
                            }
                        }
                    }
                    for gone in seen.difference(&now_seen) {
                        post.send(Event::NotificationRemoved(*gone));
                    }
                    seen = now_seen;
                }
            }
            for (id, added) in rx {
                if !added {
                    post.send(Event::NotificationRemoved(id));
                } else if let Ok(note) = listener.GetNotification(id).and_then(|n| read(&n)) {
                    trace(&format!("notification from {:?}: call {}", note.app, note.call));
                    post.send(Event::Notification(note));
                }
            }
            Ok(())
        };
        if let Err(err) = listen() {
            set_mode(3);
            trace(&format!("notifications: unavailable {:#x} {}", err.code().0, err.message()));
        }
    });
    nudge_tx
}

/// Shows the newest notification for a moment: who, then what.
pub struct Notifications {
    note: Note,
    logo: Option<Image>,
    arrived: Instant,
    /// The latest notifications, newest first, for the list page.
    recent: Vec<(Note, Option<Image>, Instant)>,
}

impl Notifications {
    pub fn new() -> Self {
        Self { note: Note::default(), logo: None, arrived: Instant::now(), recent: Vec::new() }
    }
}

/// How many notifications the list keeps (more than fit, none older than this session).
const KEEP: usize = 12;
/// One row of the list.
const ROW_H: f32 = 36.5;

/// "now", "4m", "2h", "1d".
fn ago(at: Instant) -> String {
    let s = at.elapsed().as_secs();
    match s {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// How long the light takes to cross the pill.
const SWEEP: Duration = Duration::from_millis(900);

impl Activity for Notifications {
    fn name(&self) -> &'static str {
        "notifications"
    }

    fn compact_width(&self, g: &Gfx) -> f32 {
        let body = if PRIVATE.load(std::sync::atomic::Ordering::Relaxed) { "Message" } else { self.note.body.as_str() };
        let text = g.measure(&self.note.title, 13.0, 600) + g.measure(body, 12.5, 500).min(170.0);
        (text + 76.0 + if self.note.call { 64.0 } else { 0.0 }).clamp(200.0, 420.0)
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let mid = r.y + r.h / 2.0;
        // One pass of light across the pill as it arrives.
        let progress = self.arrived.elapsed().as_secs_f32() / SWEEP.as_secs_f32();
        if progress < 1.0 {
            let eased = 1.0 - (1.0 - progress).powi(3);
            let color = if self.note.call { t.green } else { t.accent };
            ui.g.glow(r.x + r.w * eased, mid, 70.0, 26.0, color.alpha(0.42 * (1.0 - progress)));
        }
        let logo = Rect::new(r.x - 3.0, mid - 12.0, 24.0, 24.0);
        match &self.logo {
            Some(image) => ui.g.image(image, logo, 7.0),
            None => {
                ui.g.fill_rect(logo, 7.0, t.control_hover);
                ui.g.icon(if self.note.call { '\u{E717}' } else { '\u{EA8F}' }, r.x + 9.0, mid, 13.0, t.accent);
            }
        }
        let action = if self.note.call { 64.0 } else { 0.0 };
        let title_w = ui.g.measure(&self.note.title, 13.0, 600).min(r.w - 40.0 - action);
        ui.label(&self.note.title, Rect::new(r.x + 30.0, r.y, title_w + 2.0, r.h), 13.0, 600, Align::Left, t.text);
        let body = Rect::new(r.x + 38.0 + title_w, r.y, (r.w - 40.0 - title_w - action).max(0.0), r.h);
        // The message is cut with an ellipsis: this is a glance, not a reader. In
        // privacy mode only the sender shows; what they wrote stays in the app.
        let private = PRIVATE.load(std::sync::atomic::Ordering::Relaxed);
        ui.label(if private { "Message" } else { &self.note.body }, body, 12.5, 500, Align::Left, t.secondary);
        if self.note.call
            && ui.pill_button(id("notify.open"), "Open", Rect::new(r.right() - 56.0, mid - 12.0, 56.0, 24.0), true)
        {
            ui.actions.push(Action::Open("whatsapp:".into()));
        }
    }

    fn on_event(&mut self, event: &Event, g: &Gfx, t: &Theme) -> Option<Color> {
        let Event::Notification(note) = event else { return None };
        // Calls have their own pill (see call.rs).
        if note.call {
            return None;
        }
        self.logo = note.logo.as_ref().and_then(|bytes| g.load_image(bytes, 28.0));
        self.note = note.clone();
        self.arrived = Instant::now();
        // The same toast updated (a message edited, a download progressing) replaces its row.
        self.recent.retain(|(n, _, _)| n.id == 0 || n.id != note.id);
        self.recent.insert(0, (note.clone(), self.logo.clone(), self.arrived));
        self.recent.truncate(KEEP);
        Some(if note.call { t.green } else { t.accent })
    }

    fn animating(&self, _expanded: bool) -> bool {
        self.arrived.elapsed() < SWEEP
    }

    /// A page of its own only once something has arrived.
    fn page_size(&self) -> Option<(f32, f32)> {
        (!self.recent.is_empty()).then_some(crate::island::PAGE)
    }

    /// The latest notifications, as many as fit: logo on the left, who and
    /// what on the right. Just to look at; nothing here acts.
    fn page(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let private = PRIVATE.load(std::sync::atomic::Ordering::Relaxed);
        let body = Rect::new(r.x, r.y + 10.0, r.w, crate::island::PAGE.1 - 10.0 - 26.0);
        let rows = ((body.h / ROW_H).floor() as usize).max(1);
        for (i, (note, logo, at)) in self.recent.iter().take(rows).enumerate() {
            let row = Rect::new(body.x, body.y + ROW_H * i as f32, body.w, ROW_H);
            let mid = row.y + row.h / 2.0;
            let mark = Rect::new(row.x + 2.0, mid - 14.0, 28.0, 28.0);
            match logo {
                Some(image) => ui.g.image(image, mark, 8.0),
                None => {
                    ui.g.fill_rect(mark, 8.0, t.control_hover);
                    ui.g.icon('\u{EA8F}', mark.center().0, mid, 13.0, t.accent);
                }
            }
            let text_x = mark.right() + 12.0;
            let when = ago(*at);
            let when_w = ui.g.measure(&when, 11.0, 500) + 4.0;
            // First line: who (or the subject), with the app and the time beside it.
            let title = if note.title.is_empty() { note.app.as_str() } else { note.title.as_str() };
            let title_w = ui.g.measure(title, 12.5, 600).min(row.right() - text_x - when_w - 90.0);
            ui.label(title, Rect::new(text_x, row.y + 4.0, title_w + 2.0, 17.0), 12.5, 600, Align::Left, t.text);
            if !note.title.is_empty() && !note.app.is_empty() {
                let app = Rect::new(text_x + title_w + 8.0, row.y + 4.0, row.right() - text_x - title_w - when_w - 14.0, 17.0);
                ui.label(&note.app, app, 11.0, 500, Align::Left, t.tertiary);
            }
            ui.label(&when, Rect::new(row.right() - when_w, row.y + 4.0, when_w, 17.0), 11.0, 500, Align::Right, t.tertiary);
            // Second line: what it says (only who it is from, in privacy mode).
            let text = if private { "Message" } else { note.body.as_str() };
            ui.label(text, Rect::new(text_x, row.y + 20.0, row.right() - text_x, 16.0), 11.5, 500, Align::Left, t.secondary);
            if i + 1 < rows.min(self.recent.len()) {
                ui.g.fill_rect(Rect::new(text_x, row.bottom() - 0.5, row.right() - text_x, 1.0), 0.0, t.rim);
            }
        }
    }
}
