//! Clock and month calendar. No accounts and no events: today's date, the
//! month at a glance, and (once loaded) the public holidays of the region.

use crate::island::Activity;
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::ui::{id, Ui};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November",
    "December",
];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

pub fn days_in_month(year: u16, month: u16) -> u16 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// A public holiday: (month, day, name).
pub type Holiday = (u16, u16, String);

pub struct Calendar {
    now: SYSTEMTIME,
    pub holidays: Vec<Holiday>,
    /// Use a 24-hour clock (taken from the Windows time format).
    pub hour24: bool,
    /// A day of this month picked in the grid; None shows today.
    selected: Option<u16>,
    /// The big number, rolling from one day to the next.
    roll: crate::shell::ui::Roll,
    roll_day: u16,
    /// When the page was last drawn, to notice it being opened again.
    drawn: Option<Instant>,
}


impl Calendar {
    pub fn new() -> Self {
        // SAFETY: plain value-returning call.
        let now = unsafe { GetLocalTime() };
        Self { now, holidays: Vec::new(), hour24: false, selected: None, roll: crate::shell::ui::Roll::new(&now.wDay.to_string()), roll_day: now.wDay, drawn: None }
    }

    fn time(&self) -> String {
        let (h, m) = (self.now.wHour, self.now.wMinute);
        if self.hour24 {
            format!("{h}:{m:02}")
        } else {
            format!("{}:{m:02}", if h % 12 == 0 { 12 } else { h % 12 })
        }
    }

    fn holiday_on(&self, month: u16, day: u16) -> Option<&str> {
        self.holidays.iter().find(|(m, d, _)| *m == month && *d == day).map(|(_, _, name)| name.as_str())
    }

    /// The next holiday from today within this year: (days away, name).
    fn next_holiday(&self) -> Option<(u32, &str)> {
        let ordinal = |month: u16, day: u16| -> u32 {
            (1..month).map(|m| u32::from(days_in_month(self.now.wYear, m))).sum::<u32>() + u32::from(day)
        };
        let today = ordinal(self.now.wMonth, self.now.wDay);
        self.holidays
            .iter()
            .map(|(m, d, name)| (ordinal(*m, *d), name.as_str()))
            .filter(|(at, _)| *at >= today)
            .min_by_key(|(at, _)| *at)
            .map(|(at, name)| (at - today, name))
    }
}

impl Activity for Calendar {
    fn name(&self) -> &'static str {
        "calendar"
    }

    fn compact_width(&self, g: &Gfx) -> f32 {
        let date = format!("{} {}", &DAYS[self.now.wDayOfWeek as usize % 7][..3], self.now.wDay);
        g.measure(&self.time(), 14.0, 700) + g.measure(&date, 13.0, 500) + 78.0
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let date = format!("{} {}", &DAYS[self.now.wDayOfWeek as usize % 7][..3], self.now.wDay);
        ui.number(&self.time(), r, 13.5, true, Align::Left, ui.t.accent);
        ui.label(&date, r, 13.0, 500, Align::Right, ui.t.text);
    }

    fn page_size(&self) -> Option<(f32, f32)> {
        Some(crate::island::PAGE)
    }

    fn page(&mut self, ui: &mut Ui, r: Rect) {
        let (t, now) = (ui.t, self.now);
        let body = Rect::new(r.x, r.y + 12.0, r.w, crate::island::PAGE.1 - 12.0 - 26.0);

        // Coming back to the page shows today again.
        let clock = Instant::now();
        if self.drawn.is_none_or(|at| clock.duration_since(at) > Duration::from_millis(600)) {
            self.selected = None;
        }
        self.drawn = Some(clock);
        let first = (i32::from(now.wDayOfWeek) - (i32::from(now.wDay) - 1)).rem_euclid(7) as u16;
        let shown = self.selected.unwrap_or(now.wDay);
        let up = shown >= self.roll_day;
        self.roll_day = shown;
        self.roll.set(&shown.to_string(), up);

        // Left: the day being looked at (today unless another is picked), on an accent card.
        let card = Rect::new(body.x, body.y, 178.0, body.h);
        ui.g.fill_rect(card, 22.0, t.accent);
        let weekday = ((first + shown - 1) % 7) as usize;
        // The number rolls to the new day, up when moving later and down when
        // earlier. Digits slide right under the day and month labels, which sit
        // on bands of the card's colour so a digit vanishes beneath them.
        let number = Rect::new(card.x + 14.0, card.y + 34.0, card.w - 20.0, 62.0);
        self.roll.draw(ui, number, 56.0, true, Align::Left, t.on_accent, 58.0, card, false);
        let clear = t.accent.alpha(0.0);
        let top = Rect::new(card.x, card.y, card.w, number.y + 14.0 - card.y);
        ui.g.fill_rect_gradient_v(top, 22.0, number.y + 2.0, top.bottom(), t.accent, clear);
        let bottom = Rect::new(card.x, number.bottom() - 10.0, card.w, card.bottom() - number.bottom() + 10.0);
        ui.g.fill_rect_gradient_v(bottom, 22.0, bottom.y, number.bottom() + 2.0, clear, t.accent);
        ui.label(DAYS[weekday], Rect::new(card.x + 16.0, card.y + 12.0, card.w - 28.0, 22.0), 14.0, 600, Align::Left, t.on_accent);
        // A holiday on that day is named under the number.
        if let Some(name) = self.holiday_on(now.wMonth, shown) {
            ui.g.fill_circle(card.x + 19.0, card.y + 104.0, 3.0, t.on_accent);
            ui.label(name, Rect::new(card.x + 28.0, card.y + 95.0, card.w - 40.0, 18.0), 11.5, 600, Align::Left, t.on_accent);
        }
        ui.label(
            &format!("{} {}", MONTHS[(now.wMonth as usize + 11) % 12], now.wYear),
            Rect::new(card.x + 16.0, card.bottom() - 34.0, card.w - 28.0, 22.0),
            14.0,
            600,
            Align::Left,
            t.on_accent,
        );
        // Right: the month as dots. Today is the accent one; holidays are red.
        let panel = Rect::new(card.right() + 10.0, body.y, body.w - card.w - 10.0, body.h);
        ui.g.fill_rect(panel, 22.0, t.control);
        let grid = panel.inset(14.0, 0.0);
        let cell_w = grid.w / 7.0;
        for (i, letter) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
            ui.label(letter, Rect::new(grid.x + cell_w * i as f32, panel.y + 9.0, cell_w, 14.0), 10.0, 700, Align::Center, t.tertiary);
        }
        let days = days_in_month(now.wYear, now.wMonth);
        let weeks = (first + days).div_ceil(7);
        // The last line is kept for the next holiday.
        let cell_h = (panel.h - 28.0 - 24.0) / f32::from(weeks);
        for day in 1..=days {
            let slot = first + day - 1;
            let cx = grid.x + cell_w * (f32::from(slot % 7) + 0.5);
            let cy = panel.y + 28.0 + cell_h * (f32::from(slot / 7) + 0.5);
            // Any day can be picked to see it on the left; picking today goes back.
            let hit = ui.hit(id("calendar.day") ^ u32::from(day), Rect::new(cx - cell_w / 2.0, cy - cell_h / 2.0, cell_w, cell_h));
            if hit.clicked {
                self.selected = (day != now.wDay).then_some(day);
            }
            if day == now.wDay {
                ui.g.fill_circle(cx, cy, 8.5, t.accent);
                ui.number(&day.to_string(), Rect::new(cx - 12.0, cy - 8.0, 24.0, 16.0), 8.5, true, Align::Center, t.on_accent);
            } else if self.selected == Some(day) {
                ui.g.fill_circle(cx, cy, 8.5, t.text.alpha(0.16));
                ui.number(&day.to_string(), Rect::new(cx - 12.0, cy - 8.0, 24.0, 16.0), 8.5, true, Align::Center, t.text);
            } else if self.holiday_on(now.wMonth, day).is_some() {
                ui.g.fill_circle(cx, cy, 3.6 + 1.2 * hit.hover, t.red);
            } else {
                let weekend = slot % 7 == 0 || slot % 7 == 6;
                let dot = if weekend { t.tertiary.alpha(0.6) } else { t.tertiary };
                ui.g.fill_circle(cx, cy, 3.2 + 1.2 * hit.hover, dot.mix(t.text, hit.hover * 0.5));
            }
        }
        // What is coming up, when the region's holidays are known.
        let note = match (self.holiday_on(now.wMonth, now.wDay), self.next_holiday()) {
            (Some(name), _) => Some(format!("Today \u{00B7} {name}")),
            (None, Some((1, name))) => Some(format!("Tomorrow \u{00B7} {name}")),
            (None, Some((days, name))) => Some(format!("In {days} days \u{00B7} {name}")),
            _ => None,
        };
        if let Some(note) = note {
            ui.g.fill_circle(grid.x + 4.0, panel.bottom() - 13.0, 3.0, t.red);
            ui.label(&note, Rect::new(grid.x + 13.0, panel.bottom() - 23.0, grid.w - 13.0, 20.0), 11.0, 600, Align::Left, t.secondary);
        }
    }

    fn refresh(&mut self) {
        // SAFETY: plain value-returning call.
        self.now = unsafe { GetLocalTime() };
    }

    fn on_event(
        &mut self,
        event: &crate::island::Event,
        _g: &Gfx,
        _t: &crate::theme::Theme,
    ) -> Option<crate::theme::Color> {
        if let crate::island::Event::Holidays(holidays) = event {
            self.holidays = holidays.clone();
        }
        None
    }

    fn animating(&self, _expanded: bool) -> bool {
        // Only while the number rolls.
        self.roll.moving()
    }
}

/// Google's public holiday calendars, for regions the primary source lacks.
fn google_calendar(region: &str) -> Option<&'static str> {
    Some(match region {
        "IN" => "indian",
        "US" => "usa",
        "GB" => "uk",
        "CA" => "canadian",
        "AU" => "australian",
        "DE" => "german",
        "FR" => "french",
        "JP" => "japanese",
        "BR" => "brazilian",
        "MX" => "mexican",
        "ES" => "spain",
        "IT" => "italian",
        "ID" => "indonesian",
        "PK" => "pk",
        "BD" => "bd",
        "LK" => "lk",
        "NP" => "np",
        "AE" => "ae",
        "SA" => "saudiarabian",
        "SG" => "singapore",
        "MY" => "malaysia",
        "PH" => "philippines",
        "ZA" => "sa",
        "NG" => "ng",
        "KR" => "south_korea",
        "CN" => "china",
        "RU" => "russian",
        "TR" => "turkish",
        "NL" => "dutch",
        "SE" => "swedish",
        _ => return None,
    })
}

fn fetch_holidays(region: &str, year: u16) -> Vec<Holiday> {
    use crate::modules::net;
    let mut found = Vec::new();
    // Nager.Date: open data, covers about a hundred countries.
    if let Ok(text) = net::get(&format!("https://date.nager.at/api/v3/PublicHolidays/{year}/{region}")) {
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str(&text) {
            for item in items {
                let date = item["date"].as_str().unwrap_or_default();
                let name = item["name"].as_str().or(item["localName"].as_str()).unwrap_or_default();
                if let (Some(month), Some(day)) = (date.get(5..7).and_then(|m| m.parse().ok()), date.get(8..10).and_then(|d| d.parse().ok())) {
                    found.push((month, day, name.to_string()));
                }
            }
        }
    }
    if !found.is_empty() {
        return found;
    }
    // Otherwise the public holiday calendar Google publishes for the country.
    let Some(calendar) = google_calendar(region) else { return found };
    let url = format!("https://calendar.google.com/calendar/ical/en.{calendar}%23holiday%40group.v.calendar.google.com/public/basic.ics");
    let Ok(text) = net::get(&url) else { return found };
    let (mut date, prefix) = (None::<(u16, u16)>, year.to_string());
    for line in text.lines() {
        let line = line.trim();
        if line == "BEGIN:VEVENT" {
            date = None;
        } else if let Some(value) = line.strip_prefix("DTSTART;VALUE=DATE:") {
            if value.starts_with(&prefix) {
                date = value.get(4..6).and_then(|m| m.parse().ok()).zip(value.get(6..8).and_then(|d| d.parse().ok()));
            }
        } else if let (Some(name), Some((month, day))) = (line.strip_prefix("SUMMARY:"), date) {
            found.push((month, day, name.replace("\\,", ",")));
            date = None;
        }
    }
    found.sort();
    found.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    found
}

/// Load this year's holidays for the region: from the cache when it is there,
/// from the network once a year otherwise.
pub fn spawn_holidays(post: crate::island::Post, region_override: String) {
    let _ = std::thread::Builder::new().name("glide-holidays".into()).spawn(move || {
        let region = if region_override.trim().is_empty() { crate::modules::net::region() } else { region_override.trim().to_uppercase() };
        if region.len() != 2 {
            return;
        }
        // SAFETY: plain value-returning call.
        let year = unsafe { GetLocalTime() }.wYear;
        let cache = crate::settings::data_dir().map(|dir| dir.join(format!("holidays-{region}-{year}.json")));
        let cached: Option<Vec<Holiday>> =
            cache.as_ref().and_then(|path| std::fs::read_to_string(path).ok()).and_then(|text| serde_json::from_str(&text).ok());
        let holidays = match cached {
            Some(list) => list,
            None => {
                let list = fetch_holidays(&region, year);
                if let (false, Some(path), Ok(text)) = (list.is_empty(), &cache, serde_json::to_string(&list)) {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    let _ = std::fs::write(path, text);
                }
                list
            }
        };
        if !holidays.is_empty() {
            post.send(crate::island::Event::Holidays(holidays));
        }
    });
}
