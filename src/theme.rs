//! Light and dark appearance. Every colour is premultiplied only at draw time.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const fn rgb(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }

    pub fn alpha(self, a: f32) -> Self {
        Self { a: self.a * a, ..self }
    }

    pub fn mix(self, other: Self, t: f32) -> Self {
        let l = |a: f32, b: f32| a + (b - a) * t;
        Self { r: l(self.r, other.r), g: l(self.g, other.g), b: l(self.b, other.b), a: l(self.a, other.a) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Appearance {
    /// Follow the Windows app mode.
    #[default]
    System,
    Light,
    Dark,
}

/// Accent choices: (name, colour in dark mode, colour in light mode). Dark-mode
/// accents are bright and carry dark text; light-mode accents are deep and carry white.
pub const ACCENTS: [(&str, u32, u32); 7] = [
    ("Blue", 0x4aa8ff, 0x0a6cff),
    ("Mint", 0x4de0b0, 0x00896b),
    ("Lime", 0xc8f560, 0x4e7a00),
    ("Yellow", 0xffd60a, 0xa86a00),
    ("Orange", 0xff9f45, 0xc25400),
    ("Pink", 0xff6fa8, 0xd4266c),
    ("Purple", 0xb79cff, 0x6b46d9),
];

/// Semantic colours. Text pairs meet 4.5:1 on `body` in both appearances.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    /// The island itself.
    pub body: Color,
    /// Hairline that separates the island from what is behind it.
    pub rim: Color,
    pub shadow: Color,
    pub text: Color,
    pub secondary: Color,
    pub tertiary: Color,
    /// Fill for controls resting on the body.
    pub control: Color,
    pub control_hover: Color,
    pub track: Color,
    /// The filled primary control (play, answer).
    pub primary: Color,
    pub on_primary: Color,
    pub accent: Color,
    /// Text and glyphs drawn on top of an accent fill.
    pub on_accent: Color,
    pub green: Color,
    pub amber: Color,
    pub red: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            dark: true,
            body: Color::rgb(0x000000),
            rim: Color::rgb(0xffffff).alpha(0.10),
            shadow: Color::rgb(0x000000).alpha(0.30),
            text: Color::rgb(0xffffff),
            secondary: Color::rgb(0xebebf5).alpha(0.62),
            tertiary: Color::rgb(0xebebf5).alpha(0.34),
            control: Color::rgb(0xffffff).alpha(0.10),
            control_hover: Color::rgb(0xffffff).alpha(0.18),
            track: Color::rgb(0xffffff).alpha(0.20),
            primary: Color::rgb(0xffffff),
            on_primary: Color::rgb(0x000000),
            accent: Color::rgb(ACCENTS[0].1),
            on_accent: Color::rgb(0x0b0b0d),
            green: Color::rgb(0x30d158),
            amber: Color::rgb(0xff9f0a),
            red: Color::rgb(0xff453a),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            body: Color::rgb(0xfbfbfd),
            rim: Color::rgb(0x000000).alpha(0.10),
            shadow: Color::rgb(0x000000).alpha(0.16),
            text: Color::rgb(0x1d1d1f),
            secondary: Color::rgb(0x3c3c43).alpha(0.72),
            tertiary: Color::rgb(0x3c3c43).alpha(0.42),
            control: Color::rgb(0x000000).alpha(0.06),
            control_hover: Color::rgb(0x000000).alpha(0.11),
            track: Color::rgb(0x000000).alpha(0.13),
            primary: Color::rgb(0x1d1d1f),
            on_primary: Color::rgb(0xffffff),
            accent: Color::rgb(ACCENTS[0].2),
            on_accent: Color::rgb(0xffffff),
            green: Color::rgb(0x248a3d),
            amber: Color::rgb(0xb25000),
            red: Color::rgb(0xd70015),
        }
    }

    pub fn resolve(appearance: Appearance, accent: usize) -> Self {
        let dark = match appearance {
            Appearance::Dark => true,
            Appearance::Light => false,
            Appearance::System => system_prefers_dark(),
        };
        let (_, on_dark, on_light) = ACCENTS[accent % ACCENTS.len()];
        let mut theme = if dark { Self::dark() } else { Self::light() };
        theme.accent = Color::rgb(if dark { on_dark } else { on_light });
        theme
    }
}

/// The "Choose your default app mode" setting in Windows.
pub fn system_prefers_dark() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value = 1u32;
    let mut size = 4u32;
    // SAFETY: `value` and `size` describe a valid 4-byte buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 0
}

/// Windows' "Transparency effects" setting (Personalization > Colors). Off
/// means glass surfaces should be drawn solid.
pub fn system_transparency() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value = 1u32;
    let mut size = 4u32;
    // SAFETY: `value` and `size` describe a valid 4-byte buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("EnableTransparency"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    !status.is_ok() || value != 0
}
