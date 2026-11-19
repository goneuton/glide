//! Weather from Open-Meteo (free, open data, no account or key): now, and the
//! next five days.
//!
//! The place comes from the city in settings, or from Windows location when
//! that is allowed. A worker fetches every half hour and sleeps in between.

use crate::island::{Activity, Event, Post, PAGE};
use crate::modules::net;
use crate::settings::Settings;
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::ui::Ui;
use crate::theme::{Color, Theme};
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sky {
    Clear,
    Cloudy,
    Fog,
    Rain,
    Snow,
    Storm,
}

fn sky_of(code: u32, day: bool) -> (Sky, &'static str) {
    match code {
        0 => (Sky::Clear, if day { "Clear sky" } else { "Clear night" }),
        1 | 2 => (Sky::Cloudy, "Partly cloudy"),
        3 => (Sky::Cloudy, "Cloudy"),
        45 | 48 => (Sky::Fog, "Foggy"),
        51..=57 => (Sky::Rain, "Drizzle"),
        61..=67 | 80..=82 => (Sky::Rain, "Rainy"),
        71..=77 | 85 | 86 => (Sky::Snow, "Snowy"),
        95..=99 => (Sky::Storm, "Thunderstorm"),
        _ => (Sky::Cloudy, "Cloudy"),
    }
}

/// One day of the outlook.
#[derive(Clone, Debug)]
pub struct Day {
    pub name: String,
    pub code: u32,
    pub low: f32,
    pub high: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Why there is no forecast, in words a person can act on.
    pub problem: Option<String>,
    pub place: String,
    pub temperature: f32,
    pub code: u32,
    pub day: bool,
    pub days: Vec<Day>,
}

/// Weekday name for an ISO date ("2026-11-17"), by Zeller's congruence.
fn weekday(date: &str) -> &'static str {
    let part = |range: std::ops::Range<usize>| date.get(range).and_then(|s| s.parse::<i32>().ok()).unwrap_or(1);
    let (mut y, mut m, d) = (part(0..4), part(5..7), part(8..10));
    if m < 3 {
        m += 12;
        y -= 1;
    }
    let h = (d + 13 * (m + 1) / 5 + y % 100 + (y % 100) / 4 + (y / 100) / 4 + 5 * (y / 100)).rem_euclid(7);
    ["Sat", "Sun", "Mon", "Tue", "Wed", "Thu", "Fri"][h as usize]
}

fn locate(settings: &Settings) -> Result<(f64, f64, String), String> {
    let city = settings.city.trim();
    if !city.is_empty() {
        let url = format!("https://geocoding-api.open-meteo.com/v1/search?count=1&name={}", net::encode(city));
        let json: serde_json::Value =
            serde_json::from_str(&net::get(&url)?).map_err(|_| "The weather service sent something unreadable")?;
        let hit = &json["results"][0];
        return match (hit["latitude"].as_f64(), hit["longitude"].as_f64()) {
            (Some(lat), Some(lon)) => Ok((lat, lon, hit["name"].as_str().unwrap_or(city).to_string())),
            _ => Err(format!("No place called \"{city}\"")),
        };
    }
    // No city chosen: ask Windows where the PC is.
    let position = || -> windows::core::Result<(f64, f64)> {
        let locator = windows::Devices::Geolocation::Geolocator::new()?;
        let point = locator.GetGeopositionAsync()?.get()?.Coordinate()?.Point()?.Position()?;
        Ok((point.Latitude, point.Longitude))
    };
    match position() {
        Ok((lat, lon)) => Ok((lat, lon, "Here".to_string())),
        Err(_) => Err("Turn on Windows location, or set a city from the tray menu".to_string()),
    }
}

fn fetch(settings: &Settings) -> Report {
    let fahrenheit = net::region() == "US";
    let attempt = || -> Result<Report, String> {
        let (lat, lon, place) = locate(settings)?;
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat:.3}&longitude={lon:.3}&current=temperature_2m,weather_code,is_day\
             &daily=weather_code,temperature_2m_max,temperature_2m_min&timezone=auto&forecast_days=5{}",
            if fahrenheit { "&temperature_unit=fahrenheit" } else { "" }
        );
        let json: serde_json::Value =
            serde_json::from_str(&net::get(&url)?).map_err(|_| "The weather service sent something unreadable")?;
        let (current, daily) = (&json["current"], &json["daily"]);
        let days = (0..5)
            .filter_map(|i| {
                Some(Day {
                    name: weekday(daily["time"][i].as_str()?).to_string(),
                    code: daily["weather_code"][i].as_u64()? as u32,
                    low: daily["temperature_2m_min"][i].as_f64()? as f32,
                    high: daily["temperature_2m_max"][i].as_f64()? as f32,
                })
            })
            .collect();
        Ok(Report {
            problem: None,
            place,
            temperature: current["temperature_2m"].as_f64().ok_or("The forecast was incomplete")? as f32,
            code: current["weather_code"].as_u64().unwrap_or(3) as u32,
            day: current["is_day"].as_u64().unwrap_or(1) == 1,
            days,
        })
    };
    attempt().unwrap_or_else(|problem| Report { problem: Some(problem), ..Report::default() })
}

/// Start the forecast worker. Send `()` on the returned channel to refresh now
/// (after the city changed, for example).
pub fn spawn(post: Post) -> Sender<()> {
    let (tx, rx) = channel::<()>();
    let _ = std::thread::Builder::new().name("glide-weather".into()).spawn(move || loop {
        // Settings are read fresh each time so a new city takes effect.
        post.send(Event::Weather(fetch(&Settings::load())));
        // Half an hour between forecasts; a sleeping thread costs nothing.
        if let Err(std::sync::mpsc::RecvTimeoutError::Disconnected) = rx.recv_timeout(Duration::from_secs(30 * 60)) {
            return;
        }
    });
    tx
}

pub struct Weather {
    report: Option<Report>,
}

impl Weather {
    pub fn new() -> Self {
        Self { report: None }
    }
}

/// A small sky drawn from circles. With `mono`, everything is one colour (for
/// drawing on an accent card); otherwise the sun is warm and clouds are pale.
fn draw_sky(ui: &Ui, sky: Sky, day: bool, cx: f32, cy: f32, size: f32, mono: Option<Color>) {
    let g = ui.g;
    let s = size / 100.0;
    let pick = |color: u32| mono.unwrap_or(Color::rgb(color));
    let cloud = |x: f32, y: f32, k: f32, color: Color| {
        for (dx, dy, r) in [(-26.0, 8.0, 20.0), (-4.0, -8.0, 28.0), (24.0, 4.0, 22.0), (0.0, 12.0, 24.0)] {
            g.fill_circle(x + dx * k * s, y + dy * k * s, r * k * s, color);
        }
    };
    let (light, dark) = (pick(0xf2f4f8), mono.map_or(Color::rgb(0x8b93a6), |c| c.alpha(0.55)));
    if sky == Sky::Clear || sky == Sky::Cloudy {
        let (x, y) = if sky == Sky::Clear { (cx, cy) } else { (cx + 22.0 * s, cy - 20.0 * s) };
        let body = pick(if day { 0xffcc33 } else { 0xd9def0 });
        if sky == Sky::Clear && day {
            // Rays, as short strokes around the disc.
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let (inner, outer) = (30.0 * s, 42.0 * s);
                g.line((x + a.cos() * inner, y + a.sin() * inner), (x + a.cos() * outer, y + a.sin() * outer), body, 6.0 * s);
            }
        }
        g.fill_circle(x, y, 22.0 * s, body);
    }
    match sky {
        Sky::Clear => {}
        Sky::Cloudy => cloud(cx - 8.0 * s, cy + 12.0 * s, 1.0, light),
        Sky::Fog => {
            cloud(cx, cy - 6.0 * s, 1.0, light.alpha(0.8));
            for i in 0..3 {
                let y = cy + (26.0 + 11.0 * i as f32) * s;
                g.fill_rect(Rect::new(cx - 34.0 * s, y, 68.0 * s, 4.0 * s), 2.0 * s, light.alpha(0.55));
            }
        }
        Sky::Rain | Sky::Snow | Sky::Storm => {
            cloud(cx + 14.0 * s, cy - 14.0 * s, 0.8, dark);
            cloud(cx - 6.0 * s, cy - 2.0 * s, 1.0, light);
            for i in 0..3 {
                let x = cx + (-22.0 + 20.0 * i as f32) * s;
                let y = cy + 32.0 * s;
                match sky {
                    Sky::Snow => g.fill_circle(x, y + 6.0 * s, 3.5 * s, light),
                    _ => g.line((x + 4.0 * s, y), (x - 2.0 * s, y + 14.0 * s), pick(0x5ac8fa), 4.0 * s),
                }
            }
            if sky == Sky::Storm {
                g.icon('\u{E945}', cx + 4.0 * s, cy + 36.0 * s, 22.0 * s, pick(0xffcc33));
            }
        }
    }
}

impl Activity for Weather {
    fn name(&self) -> &'static str {
        "weather"
    }

    fn compact_width(&self, g: &Gfx) -> f32 {
        let words = self.report.as_ref().filter(|r| r.problem.is_none()).map_or("Weather unavailable", |r| sky_of(r.code, r.day).1);
        g.measure(words, 13.0, 500) + 104.0
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let Some(report) = self.report.as_ref().filter(|r| r.problem.is_none()) else {
            ui.label("Weather unavailable", r, 13.0, 500, Align::Center, t.secondary);
            return;
        };
        let (sky, words) = sky_of(report.code, report.day);
        draw_sky(ui, sky, report.day, r.x + 10.0, r.y + r.h / 2.0, 22.0, Some(t.accent));
        ui.label(words, Rect::new(r.x + 30.0, r.y, r.w - 76.0, r.h), 13.0, 500, Align::Left, t.text);
        ui.number(&format!("{:.0}\u{00B0}", report.temperature), r, 13.5, true, Align::Right, t.accent);
    }

    fn page_size(&self) -> Option<(f32, f32)> {
        Some(PAGE)
    }

    fn page(&mut self, ui: &mut Ui, r: Rect) {
        let t = ui.t;
        let body = Rect::new(r.x, r.y + 12.0, r.w, PAGE.1 - 12.0 - 26.0);
        let Some(report) = self.report.as_ref() else {
            ui.label("Getting the forecast", body, 14.0, 500, Align::Center, t.secondary);
            return;
        };
        if let Some(problem) = &report.problem {
            ui.label("No forecast", Rect::new(r.x, body.y + 44.0, r.w, 24.0), 17.0, 600, Align::Center, t.text);
            ui.label(problem, Rect::new(r.x, body.y + 72.0, r.w, 20.0), 12.5, 500, Align::Center, t.secondary);
            return;
        }
        // Left: now, on a card in the accent colour.
        let card = Rect::new(body.x, body.y, 178.0, body.h);
        ui.g.fill_rect(card, 22.0, t.accent);
        let (sky, words) = sky_of(report.code, report.day);
        draw_sky(ui, sky, report.day, card.x + 24.0, card.y + 24.0, 24.0, Some(t.on_accent));
        ui.label(words, Rect::new(card.x + 42.0, card.y + 13.0, card.w - 50.0, 22.0), 13.0, 600, Align::Left, t.on_accent);
        ui.number(
            &format!("{:.0}\u{00B0}", report.temperature),
            Rect::new(card.x + 14.0, card.y + 40.0, card.w - 20.0, 62.0),
            52.0,
            true,
            Align::Left,
            t.on_accent,
        );
        let place = if report.place == "Here" { "Current location" } else { &report.place };
        ui.label(place, Rect::new(card.x + 16.0, card.bottom() - 34.0, card.w - 28.0, 22.0), 14.5, 600, Align::Left, t.on_accent);

        // Right: the next five days.
        let list = Rect::new(card.right() + 10.0, body.y, body.w - card.w - 10.0, body.h);
        ui.g.fill_rect(list, 22.0, t.control);
        let row_h = (list.h - 14.0) / 5.0;
        for (i, day) in report.days.iter().take(5).enumerate() {
            let row = Rect::new(list.x + 16.0, list.y + 7.0 + row_h * i as f32, list.w - 32.0, row_h);
            let mid = row.y + row.h / 2.0;
            ui.label(&day.name, Rect::new(row.x, row.y, 44.0, row.h), 14.0, 600, Align::Left, t.text);
            draw_sky(ui, sky_of(day.code, true).0, true, row.x + 70.0, mid, 19.0, None);
            ui.number(&format!("{:.0}", day.low), Rect::new(row.right() - 76.0, row.y, 34.0, row.h), 13.0, false, Align::Right, t.tertiary);
            ui.number(&format!("{:.0}", day.high), Rect::new(row.right() - 36.0, row.y, 36.0, row.h), 13.0, true, Align::Right, t.text);
        }
    }

    fn on_event(&mut self, event: &Event, _g: &Gfx, _t: &Theme) -> Option<Color> {
        if let Event::Weather(report) = event {
            crate::island::trace(&format!("weather problem={:?} days={}", report.problem, report.days.len()));
            self.report = Some(report.clone());
        }
        None
    }
}
