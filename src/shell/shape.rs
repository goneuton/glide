//! The island's outline: one closed curve hanging from the top edge.
//!
//! It never detaches. Each side leaves the screen edge through a concave
//! shoulder, runs down a slightly slanted flank and turns into a rounded
//! bottom corner; the bottom edge can sag, which is what makes an arrival
//! look like liquid being pushed out of the edge.

use super::render::{Gfx, Path, Rect};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    /// Width of the body at the screen edge (below the shoulders).
    pub w: f32,
    pub h: f32,
    /// Radius of the bottom corners.
    pub r: f32,
    /// Radius of the concave shoulders.
    pub flare: f32,
    /// How far each flank leans inward from top to bottom.
    pub slant: f32,
    /// Extra sag at the middle of the bottom edge.
    pub bulge: f32,
    /// Horizontal offset of the bottom edge (the body leaning after the pointer).
    pub lean: f32,
}

/// Circle-approximation constant for cubic curves.
const K: f32 = 0.5523;

impl Shape {
    /// Bounding box in surface coordinates when centred on `cx`.
    pub fn bounds(&self, cx: f32) -> Rect {
        let half = self.w / 2.0 + self.flare;
        Rect::new(cx - half, 0.0, half * 2.0, self.h + self.bulge.max(0.0))
    }

    /// The area content may use: inside the flanks and the bottom corners.
    pub fn content(&self, cx: f32) -> Rect {
        let half = self.w / 2.0 - self.slant.max(0.0);
        Rect::new(cx - half + self.lean * 0.5, 0.0, half * 2.0, self.h)
    }

    pub fn path(&self, g: &Gfx, cx: f32) -> Option<Path> {
        let h = self.h.max(1.0);
        let flare = self.flare.min(h * 0.5).max(0.0);
        let half_top = self.w / 2.0;
        // The bottom is narrower than the top by the slant, and may lean sideways.
        let half_bot = (half_top - self.slant).max(2.0);
        let r = self.r.min(h - flare).min(half_bot).max(0.5);
        let (tl, tr) = (cx - half_top, cx + half_top);
        let (bl, br) = (cx - half_bot + self.lean, cx + half_bot + self.lean);
        // A point on the left flank at height `y` (flanks are straight lines).
        let flank = |top: f32, bottom: f32, y: f32| top + (bottom - top) * (y / h).clamp(0.0, 1.0);
        let sag = self.bulge * 4.0 / 3.0;

        let mut p = g.path((tl - flare, 0.0)).ok()?;
        // Left shoulder: tangent to the screen edge, then down onto the flank.
        let ly = flare;
        let lx = flank(tl, bl, ly);
        p.curve((tl - flare * (1.0 - K), 0.0), (lx, ly - flare * K), lx, ly);
        // Left flank down to the bottom-left corner.
        let cy = h - r;
        let cxl = flank(tl, bl, cy);
        p.line(cxl, cy);
        p.curve((cxl + (bl - cxl) * K, cy + r * K), (bl + r * (1.0 - K), h), bl + r, h);
        // Bottom edge, sagging in the middle.
        p.curve((bl + r + (br - bl - 2.0 * r) * 0.3, h + sag), (br - r - (br - bl - 2.0 * r) * 0.3, h + sag), br - r, h);
        // Bottom-right corner and right flank.
        let cxr = flank(tr, br, cy);
        p.curve((br - r * (1.0 - K), h), (cxr + (br - cxr) * K, cy + r * K), cxr, cy);
        let ry = flare;
        let rx = flank(tr, br, ry);
        p.line(rx, ry);
        p.curve((rx, ry - flare * K), (tr + flare * (1.0 - K), 0.0), tr + flare, 0.0);
        Some(p.finish())
    }
}
