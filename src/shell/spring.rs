//! Damped springs: the only source of motion in Glide.
//!
//! A spring carries its own velocity, so it can be given a new target at any
//! moment and continues from where it is, at the speed it has. That is what
//! makes interrupted motion feel physical instead of restarting.

/// Stiffness and damping ratio (1.0 = critically damped, lower = bouncier).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub stiffness: f32,
    pub damping: f32,
}

/// Hover and press feedback: over before you notice it.
pub const SNAPPY: Preset = Preset { stiffness: 520.0, damping: 0.86 };
/// State changes: a clear, soft settle.
pub const FLUID: Preset = Preset { stiffness: 260.0, damping: 0.78 };
/// Arrivals: a visible wobble, used sparingly.
pub const BOUNCY: Preset = Preset { stiffness: 210.0, damping: 0.52 };
/// No overshoot at all (opacity, colour).
pub const CALM: Preset = Preset { stiffness: 300.0, damping: 1.0 };

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub preset: Preset,
}

impl Spring {
    pub fn new(value: f32, preset: Preset) -> Self {
        Self { value, velocity: 0.0, target: value, preset }
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    pub fn set_with(&mut self, target: f32, preset: Preset) {
        self.target = target;
        self.preset = preset;
    }

    /// Jump without animating (reduced motion, first frame).
    pub fn snap(&mut self, value: f32) {
        (self.value, self.target, self.velocity) = (value, value, 0.0);
    }

    /// Add momentum, for example when something lands on the island.
    pub fn kick(&mut self, velocity: f32) {
        self.velocity += velocity;
    }

    /// Close enough to the target, and slow enough, that drawing can stop.
    pub fn settled(&self) -> bool {
        (self.value - self.target).abs() < 0.02 && self.velocity.abs() < 0.3
    }

    /// Advance by `dt` seconds. Sub-stepped so a dropped frame cannot blow it up.
    pub fn step(&mut self, dt: f32) {
        if self.settled() {
            (self.value, self.velocity) = (self.target, 0.0);
            return;
        }
        let damping = 2.0 * self.preset.damping * self.preset.stiffness.sqrt();
        let mut left = dt.min(0.1);
        while left > 0.0 {
            let h = left.min(0.004);
            let force = self.preset.stiffness * (self.target - self.value) - damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            left -= h;
        }
    }
}
