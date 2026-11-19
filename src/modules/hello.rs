//! The first thing Glide says after it is installed.

use crate::island::Activity;
use crate::shell::render::{Align, Gfx, Rect};
use crate::shell::ui::Ui;

pub struct Hello;

impl Activity for Hello {
    fn name(&self) -> &'static str {
        "hello"
    }

    fn compact_width(&self, g: &Gfx) -> f32 {
        // Handwriting runs wider than the interface face.
        g.measure("Namaste", 17.0, 700) * 1.3 + 64.0
    }

    fn compact(&mut self, ui: &mut Ui, r: Rect) {
        ui.g.script("Namaste", r, 17.0, Align::Center, ui.t.accent);
    }
}
