//! The shelf: a temporary place for things in transit. Drop files or text on
//! the island, or copy something, and it waits here to be dragged or copied
//! onward. Everything expires.
//!
//! Items can also be passed to another Glide: select some, press share, and
//! they leave encrypted under a six-character code (see `share`). Pressing
//! share with nothing selected asks for a code instead, and fetches.

use crate::island::{Activity, Event, Post};
use crate::modules::share::{self, News, CODE_LEN};
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::ui::{id, Action, Ui};
use crate::theme::{Color, Theme};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Text(String),
    File(PathBuf),
}

impl Item {
    fn title(&self) -> String {
        match self {
            Self::Text(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
            Self::File(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }

    fn glyph(&self) -> char {
        match self {
            Self::Text(_) => '\u{E8C1}',
            Self::File(path) if path.is_dir() => '\u{E8B7}',
            Self::File(_) => '\u{E7C3}',
        }
    }
}

const CAPACITY: usize = 30;
/// How long an item stays before it is forgotten.
const LIFETIME: Duration = Duration::from_secs(20 * 60);
/// How long a meeting link keeps offering to join.
const MEETING_OFFER: Duration = Duration::from_secs(25);
/// Tiles per row, and their size.
const COLUMNS: usize = 5;
const TILE_H: f32 = 92.0;
const GAP: f32 = 8.0;
/// How long shared files can be fetched: the choices offered.
const EXPIRY: [(&str, u64); 3] = [("10m", 600), ("1h", 3600), ("12h", 43_200)];

/// A link that starts a call, if `text` contains one.
fn meeting_link(text: &str) -> Option<String> {
    const HOSTS: [&str; 5] =
        ["teams.microsoft.com/l/meetup-join", "teams.live.com/meet", "zoom.us/j/", "meet.google.com/", "webex.com/meet"];
    text.split_whitespace()
        .map(|word| word.trim_matches(|c: char| "<>()[]\"'".contains(c)))
        .find(|word| word.starts_with("https://") && HOSTS.iter().any(|host| word.contains(host)))
        .map(str::to_string)
}

struct Shelved {
    item: Item,
    added: Instant,
    selected: bool,
    /// Sent to other Glides: the code, and until when it can be fetched.
    shared: Option<(String, Instant)>,
}

/// The inline box for a code: sending the selection, or receiving.
struct Panel {
    /// What is being sent; `None` when receiving.
    sending: Option<Vec<Item>>,
    code: String,
    /// The suggested code has not been typed over yet.
    suggested: bool,
    expire: u64,
    busy: bool,
    done: bool,
    /// What is happening, and whether it is a problem.
    status: Option<(String, bool)>,
    /// The island has had the keyboard for this box; losing it closes the box.
    had_keys: bool,
}

pub struct Shelf {
    items: Vec<Shelved>,
    /// Something is being dragged over the island right now.
    dragging_over: bool,
    /// What the compact strip is reporting.
    notice: Option<String>,
    meeting: Option<(String, Instant)>,
    /// Tile that was just copied, for a moment of confirmation (and the text shown).
    copied: Option<(usize, Instant, String)>,
    /// How far the tiles are scrolled, and where they are going.
    scroll: f32,
    scroll_to: f32,
    panel: Option<Panel>,
    /// A code to put on the clipboard on the next frame.
    copy_next: Option<String>,
    post: Post,
    /// Wrong codes in a row, and no new try before this.
    misses: u32,
    wait_until: Option<Instant>,
    drawn: Option<Instant>,
}

impl Shelf {
    pub fn new(post: Post) -> Self {
        Self {
            items: Vec::new(),
            dragging_over: false,
            notice: None,
            meeting: None,
            copied: None,
            scroll: 0.0,
            scroll_to: 0.0,
            panel: None,
            copy_next: None,
            post,
            misses: 0,
            wait_until: None,
            drawn: None,
        }
    }

    fn add(&mut self, item: Item) {
        self.items.retain(|s| s.item != item);
        self.items.insert(0, Shelved { item, added: Instant::now(), selected: false, shared: None });
        self.items.truncate(CAPACITY);
    }

    fn selected(&self) -> Vec<Item> {
        self.items.iter().filter(|s| s.selected).map(|s| s.item.clone()).collect()
    }

    fn open_panel(&mut self, sending: Option<Vec<Item>>) {
        let suggested = sending.is_some();
        self.panel = Some(Panel {
            code: if suggested { share::random_code() } else { String::new() },
            sending,
            suggested,
            expire: 3600,
            busy: false,
            done: false,
            status: None,
            had_keys: false,
        });
    }

    /// Send or fetch with the code in the box.
    fn submit(&mut self) {
        let Some(panel) = self.panel.as_mut() else { return };
        if panel.busy || panel.code.len() != CODE_LEN {
            return;
        }
        match panel.sending.clone() {
            Some(items) => share::send(items, panel.code.clone(), panel.expire, self.post.clone()),
            None => {
                // Guessing codes is slowed down: each miss makes the next try wait longer.
                if let Some(wait) = self.wait_until.filter(|at| *at > Instant::now()) {
                    let secs = wait.saturating_duration_since(Instant::now()).as_secs() + 1;
                    panel.status = Some((format!("Try again in {secs} s"), true));
                    return;
                }
                share::receive(panel.code.clone(), self.post.clone());
            }
        }
        (panel.busy, panel.status) = (true, Some(("Securing the code…".to_string(), false)));
    }

    /// The code box: six cells, the button, and (sending) how long it lasts.
    fn draw_panel(&mut self, ui: &mut Ui, area: Rect) {
        let t = ui.t;
        let Some(panel) = self.panel.as_mut() else { return };
        // Esc clears the code; on an empty box it closes it.
        let typed = ui.input.typed();
        let (enter, escape) = (typed.contains(&'\r'), typed.contains(&'\u{1b}') && panel.code.is_empty());
        // Keys typed into the box.
        if !panel.busy && !panel.done {
            for &c in ui.input.typed() {
                match c {
                    '\u{8}' => {
                        panel.code.pop();
                        panel.suggested = false;
                    }
                    '\r' => {}
                    '\u{1b}' => panel.code.clear(),
                    c => {
                        if let Some(c) = share::code_char(c) {
                            if panel.suggested {
                                (panel.code, panel.suggested) = (String::new(), false);
                            }
                            if panel.code.len() < CODE_LEN {
                                panel.code.push(c);
                            }
                        }
                    }
                }
            }
        }

        ui.g.fill_rect(area, 16.0, t.control);
        let inner = area.inset(14.0, 8.0);
        let title = match &panel.sending {
            Some(items) if items.len() == 1 => "Send 1 item with this code".to_string(),
            Some(items) => format!("Send {} items with this code", items.len()),
            None => "Enter a code to receive".to_string(),
        };
        ui.label(&title, Rect::new(inner.x, inner.y, inner.w - 30.0, 16.0), 12.0, 600, Align::Left, t.secondary);
        let mut close = escape;
        if !panel.busy {
            close |= ui.icon_button(id("shelf.panel.close"), '\u{E711}', area.right() - 18.0, area.y + 18.0, 9.0, t.secondary);
        }
        // Six cells, the next one to fill outlined.
        let (cell_w, cell_h, cell_gap) = (30.0, 32.0, 6.0);
        let cells_y = inner.y + 20.0;
        for i in 0..CODE_LEN {
            let cell = Rect::new(inner.x + (cell_w + cell_gap) * i as f32, cells_y, cell_w, cell_h);
            ui.g.fill_rect(cell, 9.0, t.body);
            let next = i == panel.code.len() && !panel.busy && !panel.done;
            if next || (panel.suggested && !panel.busy && !panel.done) {
                ui.g.stroke_rect(cell.inset(0.5, 0.5), 9.0, t.accent.alpha(if next { 1.0 } else { 0.35 }), if next { 1.5 } else { 1.0 });
            }
            if let Some(c) = panel.code.chars().nth(i) {
                let ink = if panel.suggested { t.secondary } else { t.text };
                ui.number(&c.to_string(), cell, 18.0, true, Align::Center, ink);
            }
        }
        // The button.
        let button = Rect::new(inner.x + (cell_w + cell_gap) * CODE_LEN as f32 + 6.0, cells_y + 2.0, 72.0, 28.0);
        let label = match (&panel.sending, panel.busy, panel.done) {
            (_, true, _) => "…",
            (_, _, true) => "Done",
            (Some(_), _, _) => "Send",
            (None, _, _) => "Get",
        };
        let ready = panel.code.len() == CODE_LEN && !panel.busy;
        let pressed = ui.pill_button(id("shelf.panel.go"), label, button, ready);
        let mut go = ready && !panel.done && (pressed || enter);
        if panel.done && pressed {
            close = true;
            go = false;
        }
        // Sending: how long the files can be fetched.
        let row = cells_y + cell_h + 6.0;
        let mut status_x = inner.x;
        if panel.sending.is_some() && !panel.done {
            ui.label("Gone in", Rect::new(inner.x, row, 52.0, 22.0), 11.5, 500, Align::Left, t.tertiary);
            let mut x = inner.x + 52.0;
            for (i, (label, secs)) in EXPIRY.into_iter().enumerate() {
                let chip = Rect::new(x, row, 40.0, 22.0);
                let on = panel.expire == secs;
                let hit = ui.hit(id("shelf.expiry") ^ i as u32, chip);
                ui.g.fill_rect(chip, 11.0, if on { t.accent } else { t.body.mix(t.control_hover, hit.hover) });
                ui.label(label, chip, 11.0, 600, Align::Center, if on { t.on_accent } else { t.text });
                if hit.clicked && !panel.busy {
                    panel.expire = secs;
                }
                x += 44.0;
            }
            status_x = x + 6.0;
        }
        if let Some((text, problem)) = &panel.status {
            let r = Rect::new(status_x, row, inner.right() - status_x, 22.0);
            ui.label(text, r, 11.5, 500, Align::Left, if *problem { t.red } else { t.secondary });
        }
        // Clicking anywhere else takes the keyboard away: that closes the box too.
        if ui.input.keyboard {
            panel.had_keys = true;
        } else if panel.had_keys && !panel.busy {
            close = true;
        }
        if close {
            self.panel = None;
        } else if go {
            self.submit();
        }
    }
}

impl Activity for Shelf {
    fn priority(&self) -> u8 {
        if self.meeting.is_some() { 60 } else { 0 }
    }

    fn name(&self) -> &'static str {
        "shelf"
    }

    fn demands_page(&self) -> bool {
        self.dragging_over
    }

    fn typing(&self) -> bool {
        // Busy too: a failed try leaves the box open to type again.
        self.panel.as_ref().is_some_and(|p| !p.done)
    }

    fn compact_width(&self, g: &Gfx) -> f32 {
        if self.meeting.is_some() {
            258.0
        } else {
            (g.measure(self.notice.as_deref().unwrap_or("Shelf"), 13.0, 600) + 66.0).clamp(170.0, 330.0)
        }
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let mid = r.y + r.h / 2.0;
        if let Some((link, _)) = self.meeting.clone() {
            ui.g.icon('\u{E714}', r.x + 8.0, mid, 15.0, t.green);
            ui.label("Meeting link copied", Rect::new(r.x + 26.0, r.y, r.w - 96.0, r.h), 13.0, 600, Align::Left, t.text);
            if ui.pill_button(id("shelf.join"), "Join", Rect::new(r.right() - 56.0, mid - 12.0, 56.0, 24.0), true) {
                ui.actions.push(Action::Open(link));
                self.meeting = None;
            }
            return;
        }
        ui.g.icon('\u{E8C8}', r.x + 8.0, mid, 14.0, t.accent);
        let busy = self.panel.as_ref().filter(|p| p.busy).and_then(|p| p.status.as_ref()).map(|(s, _)| s.clone());
        let text = busy.or_else(|| self.notice.clone()).unwrap_or_else(|| match self.items.len() {
            0 => "Shelf is empty".to_string(),
            1 => "1 item on the shelf".to_string(),
            n => format!("{n} items on the shelf"),
        });
        ui.label(&text, Rect::new(r.x + 26.0, r.y, r.w - 26.0, r.h), 13.0, 600, Align::Left, t.text);
    }

    fn page_size(&self) -> Option<(f32, f32)> {
        Some((440.0, 184.0))
    }

    fn page(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        if let Some(code) = self.copy_next.take() {
            ui.actions.push(Action::Copy(Item::Text(code)));
        }
        // Coming back to the page: a finished or abandoned code box is gone.
        let now = Instant::now();
        if self.drawn.is_none_or(|at| now.duration_since(at) > Duration::from_millis(600)) {
            // (An open box being typed into keeps the page up, so only a finished one goes.)
            if self.panel.as_ref().is_some_and(|p| p.done) {
                self.panel = None;
            }
        }
        self.drawn = Some(now);

        let top = r.y + 18.0;
        let picked = self.items.iter().filter(|s| s.selected).count();
        let title = if picked > 0 { format!("{picked} selected") } else { "Shelf".to_string() };
        ui.label(&title, Rect::new(r.x, top, 160.0, 20.0), 13.0, 600, Align::Left, t.secondary);
        let area = Rect::new(r.x, top + 30.0, r.w, 96.0);

        if self.panel.is_none() {
            // Share (with a selection) or receive (without): one icon for both ways.
            let mut x = r.right() - 13.0;
            if ui.icon_button(id("shelf.share"), '\u{E8AB}', x, top + 10.0, 13.0, t.accent) {
                let picked = self.selected();
                self.open_panel((!picked.is_empty()).then_some(picked));
            }
            x -= 34.0;
            if picked > 0 {
                if ui.icon_button(id("shelf.delete"), '\u{E74D}', x, top + 10.0, 13.0, t.text) {
                    self.items.retain(|s| !s.selected);
                    self.notice = None;
                }
                x -= 34.0;
                if ui.icon_button(id("shelf.deselect"), '\u{E8E6}', x, top + 10.0, 13.0, t.secondary) {
                    self.items.iter_mut().for_each(|s| s.selected = false);
                }
            } else if !self.items.is_empty()
                && ui.pill_button(id("shelf.clear"), "Clear", Rect::new(x - 46.0, top - 2.0, 58.0, 24.0), false)
            {
                self.items.clear();
                self.notice = None;
            }
        }

        if self.panel.is_some() && !self.dragging_over {
            self.draw_panel(ui, area);
            return;
        }
        if self.items.is_empty() || self.dragging_over {
            // The drop zone: lit while something hovers over it.
            let lit = self.dragging_over;
            ui.g.fill_rect(area, 16.0, if lit { t.accent.alpha(0.16) } else { t.control });
            ui.g.stroke_rect(area.inset(0.5, 0.5), 16.0, if lit { t.accent } else { t.rim }, if lit { 1.5 } else { 1.0 });
            let label = if lit { "Drop to keep it here" } else { "Drop files or text here, or copy something" };
            ui.label(label, area, 13.0, 500, Align::Center, if lit { t.text } else { t.secondary });
            return;
        }

        // The tiles, five to a row; up and down scroll when there are more rows.
        let tile_w = (area.w - GAP * (COLUMNS as f32 - 1.0)) / COLUMNS as f32;
        let rows = self.items.len().div_ceil(COLUMNS);
        let extent = rows as f32 * (TILE_H + GAP) - GAP;
        let max = (extent - area.h).max(0.0);
        if ui.input.inside && area.contains(ui.input.x, ui.input.y) {
            self.scroll_to += ui.input.wheel * 52.0;
        }
        self.scroll_to = self.scroll_to.clamp(0.0, max);
        self.scroll += (self.scroll_to - self.scroll) * (ui.dt * 16.0).min(1.0);
        if (self.scroll_to - self.scroll).abs() < 0.3 {
            self.scroll = self.scroll_to;
        } else {
            ui.controls.moving = true;
        }
        let pointer_in = ui.input.inside && area.contains(ui.input.x, ui.input.y);
        let selecting = picked > 0;
        let mut remove = None;
        ui.g.push_rect_clip(area.inset(-4.0, 0.0));
        for index in 0..self.items.len() {
            let (col, row) = (index % COLUMNS, index / COLUMNS);
            let tile = Rect::new(area.x + (tile_w + GAP) * col as f32, area.y + (TILE_H + GAP) * row as f32 - self.scroll, tile_w, TILE_H);
            if tile.bottom() < area.y || tile.y > area.bottom() {
                continue;
            }
            let over_tile = pointer_in && tile.contains(ui.input.x, ui.input.y);
            let item = self.items[index].item.clone();
            // The small buttons sit on the tile and are offered first, so their clicks are their own.
            let pick = Rect::new(tile.x + 4.0, tile.y + 4.0, 18.0, 18.0);
            let pick_hit = (over_tile || selecting).then(|| ui.hit(id("shelf.pick") ^ index as u32, pick));
            let remove_hit = over_tile.then(|| ui.hit(id("shelf.remove") ^ index as u32, Rect::new(tile.right() - 22.0, tile.y + 4.0, 18.0, 18.0)));
            let badge = Rect::new(tile.right() - 24.0, tile.y + 40.0, 20.0, 20.0);
            let badge_hit = self.items[index].shared.as_ref().map(|_| ui.hit(id("shelf.code") ^ index as u32, badge));
            let hit = if pointer_in { Some(ui.hit(id("shelf.tile") ^ index as u32, tile)) } else { None };
            let (hover, press, clicked) = hit.map_or((0.0, 0.0, false), |h| (h.hover, h.press, h.clicked));
            let selected = self.items[index].selected;
            let tile = tile.scaled(1.0 + 0.03 * hover - 0.05 * press);
            ui.g.fill_rect(tile, 14.0, if selected { t.accent.alpha(0.18) } else { t.control.mix(t.control_hover, hover) });
            if selected {
                ui.g.stroke_rect(tile.inset(0.75, 0.75), 14.0, t.accent, 1.5);
            }
            let (cx, _) = tile.center();
            let copied = self.copied.as_ref().filter(|(i, at, _)| *i == index && at.elapsed() < Duration::from_millis(1400));
            ui.g.icon(if copied.is_some() { '\u{E73E}' } else { item.glyph() }, cx, tile.y + 36.0, 24.0, if copied.is_some() { t.green } else { t.text });
            let name = copied.map_or_else(|| item.title(), |(_, _, text)| text.clone());
            ui.label(&name, Rect::new(tile.x + 6.0, tile.bottom() - 28.0, tile.w - 12.0, 18.0), 11.0, 500, Align::Center, t.secondary);
            // Select: a circle that fills with a tick.
            if let Some(pick_hit) = pick_hit {
                let (px, py) = pick.center();
                if selected {
                    ui.g.fill_circle(px, py, 8.0, t.accent);
                    ui.g.icon('\u{E73E}', px, py, 7.0, t.on_accent);
                } else {
                    ui.g.fill_circle(px, py, 8.0, t.body.alpha(0.6 + 0.3 * pick_hit.hover));
                    ui.g.ring(px, py, 7.5, 1.3, 1.0, t.secondary.mix(t.text, pick_hit.hover));
                }
                if pick_hit.clicked {
                    self.items[index].selected = !selected;
                    continue;
                }
            }
            // Shared: a link badge; clicking it copies the code again.
            if let (Some(badge_hit), Some((code, _))) = (badge_hit, self.items[index].shared.clone()) {
                let (bx, by) = badge.center();
                ui.g.fill_circle(bx, by, 9.0 + badge_hit.hover, t.accent);
                ui.g.icon('\u{E71B}', bx, by, 8.0, t.on_accent);
                if badge_hit.clicked {
                    ui.actions.push(Action::Copy(Item::Text(code.clone())));
                    self.copied = Some((index, Instant::now(), format!("Code {code}")));
                    continue;
                }
            }
            if let Some(remove_hit) = remove_hit {
                ui.g.fill_circle(tile.right() - 13.0, tile.y + 13.0, 8.0, t.control_hover.alpha(0.5 + 0.5 * remove_hit.hover));
                ui.g.icon('\u{E711}', tile.right() - 13.0, tile.y + 13.0, 7.0, t.secondary.mix(t.text, remove_hit.hover));
                if remove_hit.clicked {
                    remove = Some(index);
                    continue;
                }
            }
            // Press and pull to drag it out. A click copies it, or, while
            // choosing several, adds it to or takes it from the selection.
            let pulled = (ui.input.x - ui.input.press_x).abs() + (ui.input.y - ui.input.press_y).abs() > 7.0;
            if press > 0.2 && ui.input.down && pulled {
                ui.actions.push(Action::Drag(item.clone()));
            } else if clicked && selecting {
                self.items[index].selected = !selected;
            } else if clicked {
                ui.actions.push(Action::Copy(item.clone()));
                self.copied = Some((index, Instant::now(), "Copied".to_string()));
            }
        }
        ui.g.pop_rect_clip();
        // More rows than fit: a thin bar shows where in the list this is.
        if max > 0.0 {
            let track = Rect::new(area.right() + 6.0, area.y + 6.0, 3.0, area.h - 12.0);
            let h = (track.h * area.h / extent).max(14.0);
            let y = track.y + (track.h - h) * (self.scroll / max);
            ui.g.fill_rect(Rect::new(track.x, y, 3.0, h), 1.5, t.tertiary);
        }
        if let Some(index) = remove {
            self.items.remove(index);
            // The pill must not go on naming something that is gone.
            self.notice = None;
        }
    }

    fn refresh(&mut self) {
        let now = Instant::now();
        self.items.retain(|s| s.added.elapsed() < LIFETIME || s.shared.as_ref().is_some_and(|(_, until)| *until > now));
    }

    fn deadline(&self) -> Option<Instant> {
        self.meeting.as_ref().map(|(_, since)| *since + MEETING_OFFER)
    }

    fn due(&mut self, now: Instant, _t: &Theme) -> Option<Color> {
        if self.meeting.as_ref().is_some_and(|(_, since)| now >= *since + MEETING_OFFER) {
            self.meeting = None;
        }
        None
    }

    fn heartbeat(&self, expanded: bool) -> Option<Duration> {
        // Let the "Copied" confirmation fade back to the item's name.
        (expanded && self.copied.as_ref().is_some_and(|(_, at, _)| at.elapsed() < Duration::from_millis(1500)))
            .then_some(Duration::from_millis(300))
    }

    fn on_event(&mut self, event: &Event, _g: &Gfx, t: &Theme) -> Option<Color> {
        match event {
            Event::DragOver(over) => {
                self.dragging_over = *over;
                None
            }
            Event::Dropped(items) => {
                self.notice = Some(match items.as_slice() {
                    [one] => format!("Kept {}", one.title()),
                    many => format!("Kept {} items", many.len()),
                });
                for item in items.iter().rev() {
                    self.add(item.clone());
                }
                Some(t.accent)
            }
            Event::Clipboard(items) => {
                // Copies are kept quietly; only a meeting link is worth interrupting for.
                for item in items.iter().rev() {
                    self.add(item.clone());
                }
                match items.first() {
                    Some(Item::Text(text)) => meeting_link(text).map(|link| {
                        self.meeting = Some((link, Instant::now()));
                        t.green
                    }),
                    _ => None,
                }
            }
            Event::Share(news) => self.on_share(news, t),
            _ => None,
        }
    }
}

impl Shelf {
    fn on_share(&mut self, news: &News, t: &Theme) -> Option<Color> {
        let panel = self.panel.as_mut();
        match news {
            News::Progress(text) => {
                if let Some(panel) = panel {
                    panel.status = Some((text.clone(), false));
                }
                None
            }
            News::Sent(code, items) => {
                let expire = panel.as_ref().map_or(3600, |p| p.expire);
                if let Some(panel) = panel {
                    (panel.busy, panel.done) = (false, true);
                    panel.status = Some(("Sent. The code is copied.".to_string(), false));
                }
                let until = Instant::now() + Duration::from_secs(expire);
                for shelved in self.items.iter_mut().filter(|s| items.contains(&s.item)) {
                    (shelved.shared, shelved.selected) = (Some((code.clone(), until)), false);
                }
                self.copy_next = Some(code.clone());
                self.notice = Some(format!("Shared with code {code}"));
                None
            }
            News::Received(paths) => {
                if let Some(panel) = panel {
                    (panel.busy, panel.done) = (false, true);
                    let n = paths.len();
                    panel.status = Some((format!("Saved {n} file{} in Downloads\\Glide", if n == 1 { "" } else { "s" }), false));
                }
                (self.misses, self.wait_until) = (0, None);
                for path in paths.iter().rev() {
                    self.add(Item::File(path.clone()));
                }
                self.notice = Some(format!("Received {} item{}", paths.len(), if paths.len() == 1 { "" } else { "s" }));
                Some(t.accent)
            }
            News::Failed(error) => {
                let receiving = panel.as_ref().is_some_and(|p| p.sending.is_none());
                if let Some(panel) = panel {
                    panel.busy = false;
                    panel.status = Some((error.clone(), true));
                }
                if receiving {
                    self.misses += 1;
                    self.wait_until = Some(Instant::now() + Duration::from_secs(1 << self.misses.min(6)));
                }
                None
            }
        }
    }
}
