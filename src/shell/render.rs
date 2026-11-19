//! Direct2D drawing onto a DirectComposition surface.
//!
//! The swap chain is created once at the largest size the island can reach;
//! the window is only ever a view onto part of it. Nothing is drawn or
//! presented unless a frame is asked for.

use crate::theme::Color;
use std::cell::RefCell;
use std::collections::HashMap;
use windows::core::{w, Interface, Result, HSTRING};
use windows_numerics::{Matrix3x2, Vector2};
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_GRADIENT_STOP, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F, D2D1_BEZIER_SEGMENT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DeviceContext, ID2D1Factory1, ID2D1PathGeometry1, ID2D1SolidColorBrush,
    D2D1_BUFFER_PRECISION_8BPC_UNORM, D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED, D2D1_COLOR_SPACE_SRGB,
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_ELLIPSE, D2D1_EXTEND_MODE_CLAMP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_LAYER_PARAMETERS1, D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1, DXGI_CREATE_FACTORY_FLAGS,
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

/// Axis-aligned rectangle in logical pixels (x, y is the top-left corner).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    pub fn inset(&self, dx: f32, dy: f32) -> Self {
        Self::new(self.x + dx, self.y + dy, (self.w - 2.0 * dx).max(0.0), (self.h - 2.0 * dy).max(0.0))
    }
    /// Scale about the centre (press feedback).
    pub fn scaled(&self, s: f32) -> Self {
        let (cx, cy) = self.center();
        Self::new(cx - self.w * s / 2.0, cy - self.h * s / 2.0, self.w * s, self.h * s)
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Face {
    /// Segoe UI Variable: interface text.
    Text,
    /// Segoe Fluent Icons / MDL2 Assets: glyph icons that ship with Windows.
    Icon,
    /// Heavy figures: Segoe UI at its Black weight.
    Number,
    /// Handwriting, for greetings: Segoe Script, which ships with Windows.
    Script,
}

fn color(c: Color) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a }
}

fn pt(x: f32, y: f32) -> Vector2 {
    Vector2 { X: x, Y: y }
}

/// A closed outline built from lines and cubic curves.
pub struct Path {
    geometry: ID2D1PathGeometry1,
}

pub struct PathBuilder {
    geometry: ID2D1PathGeometry1,
    sink: windows::Win32::Graphics::Direct2D::ID2D1GeometrySink,
}

impl PathBuilder {
    pub fn line(&mut self, x: f32, y: f32) {
        // SAFETY: the sink is open between `begin` and `finish`.
        unsafe { self.sink.AddLine(pt(x, y)) }
    }
    /// Cubic curve to `(x, y)` with control points `c1`, `c2`.
    pub fn curve(&mut self, c1: (f32, f32), c2: (f32, f32), x: f32, y: f32) {
        let segment = D2D1_BEZIER_SEGMENT { point1: pt(c1.0, c1.1), point2: pt(c2.0, c2.1), point3: pt(x, y) };
        // SAFETY: as above.
        unsafe { self.sink.AddBezier(&segment) }
    }
    pub fn finish(self) -> Path {
        // SAFETY: closes the figure that `begin` opened.
        unsafe {
            self.sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            let _ = self.sink.Close();
        }
        Path { geometry: self.geometry }
    }
}

/// Objects one GPU device provides to every window drawn with it.
struct Shared {
    dxgi: IDXGIDevice,
    factory: ID2D1Factory1,
    d2d_device: windows::Win32::Graphics::Direct2D::ID2D1Device,
    dcomp: IDCompositionDevice,
    dwrite: IDWriteFactory,
}

pub struct Gfx {
    dc: ID2D1DeviceContext,
    factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    dxgi: IDXGIDevice,
    d2d_device: windows::Win32::Graphics::Direct2D::ID2D1Device,
    swap_chain: IDXGISwapChain1,
    dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual,
    brush: ID2D1SolidColorBrush,
    formats: RefCell<HashMap<(Face, u32, u32, Align), IDWriteTextFormat>>,
    /// Text widths already measured: rolling digits ask for the same few every frame.
    widths: RefCell<HashMap<(Face, u32, u32, String), f32>>,
    glows: RefCell<HashMap<u32, windows::Win32::Graphics::Direct2D::ID2D1RadialGradientBrush>>,
    linears: RefCell<HashMap<u64, windows::Win32::Graphics::Direct2D::ID2D1LinearGradientBrush>>,
    icon_font: HSTRING,
    /// Family used for figures, and the private collection it comes from (if not installed).
    number_font: HSTRING,
    number_fonts: Option<windows::Win32::Graphics::DirectWrite::IDWriteFontCollection>,
    /// Size of the swap chain in device pixels.
    pub surface: (u32, u32),
    pub scale: f32,
    /// Global opacity multiplier for the content being drawn.
    opacity: std::cell::Cell<f32>,
}

fn create_device(driver: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    // SAFETY: standard device creation; the out-pointer is valid.
    unsafe {
        D3D11CreateDevice(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    device.ok_or_else(windows::core::Error::empty)
}

/// Loads the bundled figure face from memory into a private collection and
/// returns its family name with the collection. None on systems without
/// in-memory font support (before Windows 10 1703).
fn bundled_figures(dwrite: &IDWriteFactory) -> Option<(HSTRING, windows::Win32::Graphics::DirectWrite::IDWriteFontCollection)> {
    use windows::Win32::Graphics::DirectWrite::IDWriteFactory5;
    static DATA: &[u8] = include_bytes!("../../assets/fonts/GeistMono-Black.ttf");
    // SAFETY: DATA is 'static, so the in-memory reference outlives every use.
    unsafe {
        let factory: IDWriteFactory5 = dwrite.cast().ok()?;
        let loader = factory.CreateInMemoryFontFileLoader().ok()?;
        factory.RegisterFontFileLoader(&loader).ok()?;
        let file = loader
            .CreateInMemoryFontFileReference(&factory, DATA.as_ptr().cast(), DATA.len() as u32, None::<&windows::core::IUnknown>)
            .ok()?;
        let builder = factory.CreateFontSetBuilder().ok()?;
        builder.AddFontFile(&file).ok()?;
        let set = builder.CreateFontSet().ok()?;
        let collection = factory.CreateFontCollectionFromFontSet(&set).ok()?;
        let names = collection.GetFontFamily(0).ok()?.GetFamilyNames().ok()?;
        let mut name = vec![0u16; names.GetStringLength(0).ok()? as usize + 1];
        names.GetString(0, &mut name).ok()?;
        name.pop();
        Some((HSTRING::from_wide(&name), collection.cast().ok()?))
    }
}

impl Gfx {
    /// `surface` is the largest logical size the island ever needs.
    pub fn new(hwnd: HWND, surface: (f32, f32), scale: f32) -> Result<Self> {
        // SAFETY: plain COM object creation; every out-value is checked by `?`.
        unsafe {
            // Fall back to the software rasteriser on machines without a usable GPU.
            let device = create_device(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| create_device(D3D_DRIVER_TYPE_WARP))?;
            let dxgi: IDXGIDevice = device.cast()?;
            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = factory.CreateDevice(&dxgi)?;
            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            Self::build(hwnd, surface, scale, Shared { dxgi, factory, d2d_device, dcomp, dwrite })
        }
    }

    /// Another surface for another window, on the same GPU device: costs a swap
    /// chain, not a second device.
    pub fn sibling(&self, hwnd: HWND, surface: (f32, f32)) -> Result<Self> {
        let shared = Shared {
            dxgi: self.dxgi.clone(),
            factory: self.factory.clone(),
            d2d_device: self.d2d_device.clone(),
            dcomp: self.dcomp.clone(),
            dwrite: self.dwrite.clone(),
        };
        // SAFETY: as in `new`.
        unsafe { Self::build(hwnd, surface, self.scale, shared) }
    }

    unsafe fn build(hwnd: HWND, surface: (f32, f32), scale: f32, shared: Shared) -> Result<Self> {
        let Shared { dxgi, factory, d2d_device, dcomp, dwrite } = shared;
        let size = ((surface.0 * scale).ceil() as u32, (surface.1 * scale).ceil() as u32);
        let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: size.0,
            Height: size.1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            ..Default::default()
        };
        let swap_chain = dxgi_factory.CreateSwapChainForComposition(&dxgi, &desc, None)?;
        let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let buffer: IDXGISurface = swap_chain.GetBuffer(0)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0 * scale,
            dpiY: 96.0 * scale,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            ..Default::default()
        };
        let bitmap = dc.CreateBitmapFromDxgiSurface(&buffer, Some(&props))?;
        dc.SetTarget(&bitmap);
        dc.SetDpi(96.0 * scale, 96.0 * scale);
        dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = dc.CreateSolidColorBrush(&color(Color::rgb(0)), None)?;

        let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
        let visual = dcomp.CreateVisual()?;
        visual.SetContent(&swap_chain)?;
        target.SetRoot(&visual)?;
        dcomp.Commit()?;

        // Windows 11 ships Segoe Fluent Icons; Windows 10 has the older MDL2 set
        // with the same code points for everything Glide uses.
        let mut exists = windows::core::BOOL(0);
        let mut index = 0u32;
        let mut fonts = None;
        dwrite.GetSystemFontCollection(&mut fonts, false)?;
        if let Some(fonts) = &fonts {
            let _ = fonts.FindFamilyName(w!("Segoe Fluent Icons"), &mut index, &mut exists);
        }
        let icon_font = HSTRING::from(if exists.as_bool() { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" });

        // Figures use Geist Mono Black, bundled: every digit is the same width, so
        // numbers never shift as they change. Segoe UI Black if it cannot load.
        let (number_font, number_fonts) = match bundled_figures(&dwrite) {
            Some((family, collection)) => (family, Some(collection)),
            None => (HSTRING::from("Segoe UI"), None),
        };
        Ok(Self {
            number_font,
            number_fonts,
            dc,
            factory,
            dwrite,
            dxgi,
            d2d_device,
            swap_chain,
            dcomp,
            _target: target,
            visual,
            brush,
            formats: RefCell::new(HashMap::new()),
            widths: RefCell::new(HashMap::new()),
            glows: RefCell::new(HashMap::new()),
            linears: RefCell::new(HashMap::new()),
            icon_font,
            surface: size,
            scale,
            opacity: std::cell::Cell::new(1.0),
        })
    }
    /// Logical size of the drawing surface.
    pub fn size(&self) -> (f32, f32) {
        (self.surface.0 as f32 / self.scale, self.surface.1 as f32 / self.scale)
    }

    /// Shift the surface so that the part at logical `(x, y)` sits at the window's origin.
    pub fn set_origin(&self, x: f32, y: f32) {
        // SAFETY: plain property setters on live composition objects.
        unsafe {
            let _ = self.visual.SetOffsetX2(-(x * self.scale).round());
            let _ = self.visual.SetOffsetY2(-(y * self.scale).round());
            let _ = self.dcomp.Commit();
        }
    }

    pub fn begin(&self) {
        // SAFETY: paired with `end`.
        unsafe {
            self.dc.BeginDraw();
            self.dc.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }
        self.opacity.set(1.0);
    }

    /// Finish the frame and wait `refreshes` display refreshes (1 = every
    /// refresh; 2 halves the rate for motion that does not need more; 0 = show
    /// it without waiting, for a second surface drawn in the same frame).
    pub fn end(&self, refreshes: u32) {
        // SAFETY: paired with `begin`.
        unsafe {
            self.dc.SetTransform(&Matrix3x2::identity());
            let _ = self.dc.EndDraw(None, None);
            let _ = self.swap_chain.Present(refreshes.min(5), DXGI_PRESENT(0));
        }
    }

    /// Scale everything drawn from now on about `(cx, cy)`, shifted by `dx`.
    pub fn set_transform(&self, scale: f32, cx: f32, cy: f32, dx: f32) {
        let matrix =
            Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: cx * (1.0 - scale) + dx, M32: cy * (1.0 - scale) };
        // SAFETY: plain state change on the device context.
        unsafe { self.dc.SetTransform(&matrix) }
    }

    /// Draw with everything turned by `degrees` about `(px, py)`, on top of
    /// whatever transform is already in place.
    pub fn rotated(&self, degrees: f32, px: f32, py: f32, draw: impl FnOnce()) {
        let mut saved = Matrix3x2::default();
        // SAFETY: plain state queries and changes on the device context.
        unsafe { self.dc.GetTransform(&mut saved) };
        let (s, c) = degrees.to_radians().sin_cos();
        let r = Matrix3x2 { M11: c, M12: s, M21: -s, M22: c, M31: px - c * px + s * py, M32: py - s * px - c * py };
        let m = Matrix3x2 {
            M11: r.M11 * saved.M11 + r.M12 * saved.M21,
            M12: r.M11 * saved.M12 + r.M12 * saved.M22,
            M21: r.M21 * saved.M11 + r.M22 * saved.M21,
            M22: r.M21 * saved.M12 + r.M22 * saved.M22,
            M31: r.M31 * saved.M11 + r.M32 * saved.M21 + saved.M31,
            M32: r.M31 * saved.M12 + r.M32 * saved.M22 + saved.M32,
        };
        unsafe { self.dc.SetTransform(&m) };
        draw();
        unsafe { self.dc.SetTransform(&saved) };
    }

    /// Part of a circle's outline, from 12 o'clock clockwise, with round ends.
    pub fn ring(&self, cx: f32, cy: f32, radius: f32, width: f32, fraction: f32, c: Color) {
        use windows::Win32::Graphics::Direct2D::Common::{D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN, D2D_SIZE_F};
        use windows::Win32::Graphics::Direct2D::{
            ID2D1StrokeStyle, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND,
            D2D1_STROKE_STYLE_PROPERTIES1, D2D1_SWEEP_DIRECTION_CLOCKWISE,
        };
        let fraction = fraction.clamp(0.0, 1.0);
        if fraction >= 0.999 {
            let ellipse = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: radius, radiusY: radius };
            // SAFETY: plain draw call.
            unsafe { self.dc.DrawEllipse(&ellipse, self.brush(c), width, None) };
            return;
        }
        if fraction <= 0.001 {
            return;
        }
        let angle = fraction * std::f32::consts::TAU;
        let end = pt(cx + radius * angle.sin(), cy - radius * angle.cos());
        // SAFETY: a short-lived open figure and stroke style.
        unsafe {
            let Ok(geometry) = self.factory.CreatePathGeometry() else { return };
            let Ok(sink) = geometry.Open() else { return };
            sink.BeginFigure(pt(cx, cy - radius), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: end,
                size: D2D_SIZE_F { width: radius, height: radius },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if fraction > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = sink.Close();
            let style = D2D1_STROKE_STYLE_PROPERTIES1 {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                ..Default::default()
            };
            let style: Option<ID2D1StrokeStyle> =
                self.factory.CreateStrokeStyle(&style, None).ok().and_then(|style| style.cast().ok());
            self.dc.DrawGeometry(&geometry, self.brush(c), width, style.as_ref());
        }
    }

    /// Multiply everything drawn until the next call by `opacity`.
    pub fn set_opacity(&self, opacity: f32) {
        self.opacity.set(opacity.clamp(0.0, 1.0));
    }

    fn brush(&self, c: Color) -> &ID2D1SolidColorBrush {
        // SAFETY: the brush belongs to this device context.
        unsafe { self.brush.SetColor(&color(c.alpha(self.opacity.get()))) };
        &self.brush
    }

    pub fn path(&self, start: (f32, f32)) -> Result<PathBuilder> {
        // SAFETY: a fresh geometry and its sink.
        unsafe {
            let geometry = self.factory.CreatePathGeometry()?;
            let sink = geometry.Open()?;
            sink.BeginFigure(pt(start.0, start.1), D2D1_FIGURE_BEGIN_FILLED);
            Ok(PathBuilder { geometry, sink: sink.cast()? })
        }
    }

    pub fn fill_path(&self, path: &Path, c: Color) {
        // SAFETY: geometry and brush come from this factory / context.
        unsafe { self.dc.FillGeometry(&path.geometry, self.brush(c), None) }
    }

    pub fn stroke_path(&self, path: &Path, c: Color, width: f32) {
        // SAFETY: as above.
        unsafe { self.dc.DrawGeometry(&path.geometry, self.brush(c), width, None) }
    }

    /// Clip everything until `pop_clip` to the inside of `path`.
    /// Clip to a rounded rectangle until `pop_clip`.
    pub fn push_round_clip(&self, r: Rect, radius: f32) {
        let rounded = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        // SAFETY: a short-lived geometry, kept alive by the layer while it is pushed.
        unsafe {
            let Ok(geometry) = self.factory.CreateRoundedRectangleGeometry(&rounded) else {
                // Keep push/pop balanced even if the geometry cannot be made.
                self.dc.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
                self.dc.PopAxisAlignedClip();
                let empty = D2D_RECT_F::default();
                let params = D2D1_LAYER_PARAMETERS1 { contentBounds: empty, ..Default::default() };
                self.dc.PushLayer(&params, None);
                return;
            };
            let params = D2D1_LAYER_PARAMETERS1 {
                contentBounds: D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 },
                geometricMask: std::mem::ManuallyDrop::new(geometry.cast().ok()),
                maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                maskTransform: Matrix3x2::identity(),
                opacity: 1.0,
                opacityBrush: std::mem::ManuallyDrop::new(None),
                layerOptions: Default::default(),
            };
            self.dc.PushLayer(&params, None);
            drop(std::mem::ManuallyDrop::into_inner(params.geometricMask));
        }
    }

    pub fn push_clip(&self, path: &Path) {
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: D2D_RECT_F { left: -1e6, top: -1e6, right: 1e6, bottom: 1e6 },
            geometricMask: std::mem::ManuallyDrop::new(Some(path.geometry.clone().into())),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: Default::default(),
        };
        // SAFETY: the layer borrows the geometry for the duration of the push; the
        // extra reference taken above is released right after.
        unsafe {
            self.dc.PushLayer(&params, None);
            drop(std::mem::ManuallyDrop::into_inner(params.geometricMask));
        }
    }

    pub fn pop_clip(&self) {
        // SAFETY: paired with `push_clip`.
        unsafe { self.dc.PopLayer() }
    }

    pub fn fill_rect(&self, r: Rect, radius: f32, c: Color) {
        let rounded = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        // SAFETY: plain draw call.
        unsafe { self.dc.FillRoundedRectangle(&rounded, self.brush(c)) }
    }

    pub fn stroke_rect(&self, r: Rect, radius: f32, c: Color, width: f32) {
        let rounded = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
        // SAFETY: plain draw call.
        unsafe { self.dc.DrawRoundedRectangle(&rounded, self.brush(c), width, None) }
    }

    /// Fill a closed polygon.
    pub fn fill_poly(&self, points: &[(f32, f32)], c: Color) {
        let Some((first, rest)) = points.split_first() else { return };
        let Ok(mut path) = self.path(*first) else { return };
        for (x, y) in rest {
            path.line(*x, *y);
        }
        self.fill_path(&path.finish(), c);
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, radius: f32, c: Color) {
        let ellipse = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: radius, radiusY: radius };
        // SAFETY: plain draw call.
        unsafe { self.dc.FillEllipse(&ellipse, self.brush(c)) }
    }

    pub fn line(&self, a: (f32, f32), b: (f32, f32), c: Color, width: f32) {
        // SAFETY: plain draw call.
        unsafe { self.dc.DrawLine(pt(a.0, a.1), pt(b.0, b.1), self.brush(c), width, None) }
    }

    /// Soft light: full colour at the centre fading to nothing at `radius`.
    pub fn glow(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: Color) {
        self.radial(cx, cy, rx, ry, c, false);
    }

    /// A filled body of colour: solid through most of its size, with a soft
    /// edge, so overlapping ones read as shapes melting together, not as lights.
    pub fn blob(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: Color) {
        self.radial(cx, cy, rx, ry, c, true);
    }

    fn radial(&self, cx: f32, cy: f32, rx: f32, ry: f32, c: Color, filled: bool) {
        // One brush per colour, made once and then only moved: building a
        // gradient is far too slow to do on every frame.
        let key = ((c.r * 255.0) as u32) << 16 | ((c.g * 255.0) as u32) << 8 | (c.b * 255.0) as u32 | u32::from(filled) << 24;
        let mut glows = self.glows.borrow_mut();
        if !glows.contains_key(&key) {
            let solid = Color { a: 1.0, ..c };
            let stops = if filled {
                [
                    D2D1_GRADIENT_STOP { position: 0.0, color: color(solid) },
                    D2D1_GRADIENT_STOP { position: 0.55, color: color(solid.alpha(0.88)) },
                    D2D1_GRADIENT_STOP { position: 1.0, color: color(solid.alpha(0.0)) },
                ]
            } else {
                [
                    D2D1_GRADIENT_STOP { position: 0.0, color: color(solid) },
                    D2D1_GRADIENT_STOP { position: 0.45, color: color(solid.alpha(0.35)) },
                    D2D1_GRADIENT_STOP { position: 1.0, color: color(solid.alpha(0.0)) },
                ]
            };
            // SAFETY: the brush keeps its stop collection alive.
            let brush = unsafe {
                self.dc
                    .CreateGradientStopCollection(
                        &stops,
                        D2D1_COLOR_SPACE_SRGB,
                        D2D1_COLOR_SPACE_SRGB,
                        D2D1_BUFFER_PRECISION_8BPC_UNORM,
                        D2D1_EXTEND_MODE_CLAMP,
                        D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
                    )
                    .and_then(|collection| {
                        self.dc.CreateRadialGradientBrush(&D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES::default(), None, &collection)
                    })
            };
            let Ok(brush) = brush else { return };
            // A handful of colours are ever used; keep the cache from growing without bound.
            if glows.len() > 96 {
                glows.clear();
            }
            glows.insert(key, brush);
        }
        let Some(brush) = glows.get(&key) else { return };
        // SAFETY: plain property setters and a draw call.
        unsafe {
            brush.SetCenter(pt(cx, cy));
            brush.SetRadiusX(rx);
            brush.SetRadiusY(ry);
            brush.SetOpacity(c.a * self.opacity.get());
            let ellipse = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: rx, radiusY: ry };
            self.dc.FillEllipse(&ellipse, brush);
        }
    }

    /// Fill a rounded rectangle with a gradient running from `from` at x = `x0`
    /// to `to` at x = `x1` (clamped beyond those). Brushes are cached by colour
    /// pair and only moved, so redrawing every frame stays cheap.
    pub fn fill_rect_gradient(&self, r: Rect, radius: f32, x0: f32, x1: f32, from: Color, to: Color) {
        self.fill_gradient(r, radius, (x0, r.y), (x1, r.y), from, to);
    }

    /// Same, top to bottom: `from` at y = `y0`, `to` at y = `y1`.
    pub fn fill_rect_gradient_v(&self, r: Rect, radius: f32, y0: f32, y1: f32, from: Color, to: Color) {
        self.fill_gradient(r, radius, (r.x, y0), (r.x, y1), from, to);
    }

    fn fill_gradient(&self, r: Rect, radius: f32, p0: (f32, f32), p1: (f32, f32), from: Color, to: Color) {
        use windows::Win32::Graphics::Direct2D::D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES;
        let pack = |c: Color| -> u64 {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u64;
            q(c.r) << 24 | q(c.g) << 16 | q(c.b) << 8 | q(c.a)
        };
        let key = pack(from) << 32 | pack(to);
        let mut linears = self.linears.borrow_mut();
        if !linears.contains_key(&key) {
            let stops = [
                D2D1_GRADIENT_STOP { position: 0.0, color: color(from) },
                D2D1_GRADIENT_STOP { position: 1.0, color: color(to) },
            ];
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: pt(0.0, 0.0), endPoint: pt(1.0, 0.0) };
            // SAFETY: the brush keeps its stop collection alive.
            let brush = unsafe {
                self.dc
                    .CreateGradientStopCollection(
                        &stops,
                        D2D1_COLOR_SPACE_SRGB,
                        D2D1_COLOR_SPACE_SRGB,
                        D2D1_BUFFER_PRECISION_8BPC_UNORM,
                        D2D1_EXTEND_MODE_CLAMP,
                        D2D1_COLOR_INTERPOLATION_MODE_PREMULTIPLIED,
                    )
                    .and_then(|collection| self.dc.CreateLinearGradientBrush(&props, None, &collection))
            };
            let Ok(brush) = brush else { return };
            // Colours fade in once per page view; keep the cache small.
            if linears.len() > 64 {
                linears.clear();
            }
            linears.insert(key, brush);
        }
        let Some(brush) = linears.get(&key) else { return };
        // SAFETY: property setters and a draw call on our own brush.
        unsafe {
            brush.SetStartPoint(pt(p0.0, p0.1));
            brush.SetEndPoint(pt(p1.0, p1.1));
            brush.SetOpacity(self.opacity.get());
            let rounded = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
            self.dc.FillRoundedRectangle(&rounded, brush);
        }
    }
    /// Cheap rectangular clip, for content that already fits inside the shape.
    pub fn push_rect_clip(&self, r: Rect) {
        // SAFETY: paired with `pop_rect_clip`.
        unsafe { self.dc.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) }
    }

    pub fn pop_rect_clip(&self) {
        // SAFETY: paired with `push_rect_clip`.
        unsafe { self.dc.PopAxisAlignedClip() }
    }

    fn format(&self, face: Face, size: f32, weight: u32, align: Align) -> Option<IDWriteTextFormat> {
        let key = (face, (size * 4.0) as u32, weight, align);
        if let Some(found) = self.formats.borrow().get(&key) {
            return Some(found.clone());
        }
        // SAFETY: creating and configuring a text format.
        let format = unsafe {
            let family = match face {
                Face::Text => w!("Segoe UI Variable Display").into(),
                Face::Icon => windows::core::PCWSTR(self.icon_font.as_ptr()),
                Face::Number => windows::core::PCWSTR(self.number_font.as_ptr()),
                Face::Script => w!("Segoe Script").into(),
            };
            let collection = if face == Face::Number { self.number_fonts.as_ref() } else { None };
            let format = self
                .dwrite
                .CreateTextFormat(
                    family,
                    collection,
                    DWRITE_FONT_WEIGHT(weight as i32),
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size,
                    w!("en-us"),
                )
                .ok()?;
            let _ = format.SetTextAlignment(match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            });
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);
            let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            if face != Face::Icon {
                // Long titles end in an ellipsis instead of being cut mid-letter.
                if let Ok(sign) = self.dwrite.CreateEllipsisTrimmingSign(&format) {
                    let trimming =
                        DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                    let _ = format.SetTrimming(&trimming, &sign);
                }
            }
            format
        };
        self.formats.borrow_mut().insert(key, format.clone());
        Some(format)
    }

    /// Draw one line of text, vertically centred in `r`.
    pub fn text(&self, text: &str, r: Rect, size: f32, weight: u32, align: Align, c: Color) {
        self.draw_text(Face::Text, text, r, size, weight, align, c);
    }

    /// Draw figures in the fixed-width figure face, black weight. (`heavy` is
    /// kept for callers; every number now uses the same face.)
    pub fn number(&self, text: &str, r: Rect, size: f32, _heavy: bool, align: Align, c: Color) {
        self.draw_text(Face::Number, text, r, size, 900, align, c);
    }

    /// Draw a greeting in handwriting.
    pub fn script(&self, text: &str, r: Rect, size: f32, align: Align, c: Color) {
        self.draw_text(Face::Script, text, r, size, 700, align, c);
    }

    /// Draw an icon glyph centred on `(cx, cy)`.
    pub fn icon(&self, glyph: char, cx: f32, cy: f32, size: f32, c: Color) {
        let r = Rect::new(cx - size, cy - size, size * 2.0, size * 2.0);
        let mut buffer = [0u8; 4];
        self.draw_text(Face::Icon, glyph.encode_utf8(&mut buffer), r, size, 400, Align::Center, c);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(&self, face: Face, text: &str, r: Rect, size: f32, weight: u32, align: Align, c: Color) {
        let Some(format) = self.format(face, size, weight, align) else { return };
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: `wide` outlives the call.
        unsafe {
            self.dc.DrawText(
                &wide,
                &format,
                &r.d2d(),
                self.brush(c),
                // No pixel snapping: text moving by fractions of a pixel (a rolling
                // digit settling) must glide, not step a whole pixel at a time.
                D2D1_DRAW_TEXT_OPTIONS_CLIP | windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_NO_SNAP,
                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    /// Width a line of text would take.
    /// Width of heavy figures, as `number(.., heavy = true)` draws them.
    pub fn measure_number(&self, text: &str, size: f32) -> f32 {
        self.measure_face(Face::Number, text, size, 900)
    }

    pub fn measure(&self, text: &str, size: f32, weight: u32) -> f32 {
        self.measure_face(Face::Text, text, size, weight)
    }

    fn measure_face(&self, face: Face, text: &str, size: f32, weight: u32) -> f32 {
        let key = (face, (size * 100.0) as u32, weight, text.to_string());
        if let Some(width) = self.widths.borrow().get(&key) {
            return *width;
        }
        let width = self.measure_layout(face, text, size, weight);
        let mut widths = self.widths.borrow_mut();
        // Kept small: titles change, digits do not.
        if widths.len() > 512 {
            widths.clear();
        }
        widths.insert(key, width);
        width
    }

    fn measure_layout(&self, face: Face, text: &str, size: f32, weight: u32) -> f32 {
        let Some(format) = self.format(face, size, weight, Align::Left) else { return 0.0 };
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: `wide` outlives the call; `metrics` is a valid out-value.
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(&wide, &format, 4096.0, 100.0) else { return 0.0 };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            let _ = layout.GetMetrics(&mut metrics);
            metrics.widthIncludingTrailingWhitespace
        }
    }

    /// Decode an encoded image (PNG, JPEG, ...) into a small GPU bitmap. Artwork
    /// is never shown larger than `max_side` logical pixels, so it is scaled
    /// down while decoding rather than kept at full size.
    pub fn load_image(&self, bytes: &[u8], max_side: f32) -> Option<Image> {
        use windows::Win32::Graphics::Imaging::{
            CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
            WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand,
        };
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        use windows::Win32::UI::Shell::SHCreateMemStream;
        // SAFETY: COM objects created and used on this thread; the stream copies `bytes`.
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let stream = SHCreateMemStream(Some(bytes))?;
            let decoder = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            frame.GetSize(&mut w, &mut h).ok()?;
            let limit = (max_side * self.scale).ceil().max(1.0);
            let ratio = (limit / w.max(h).max(1) as f32).min(1.0);
            let scaler = wic.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(
                    &frame,
                    ((w as f32 * ratio) as u32).max(1),
                    ((h as f32 * ratio) as u32).max(1),
                    WICBitmapInterpolationModeFant,
                )
                .ok()?;
            let converter = wic.CreateFormatConverter().ok()?;
            converter
                .Initialize(
                    &scaler,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeMedianCut,
                )
                .ok()?;
            let bitmap = self.dc.CreateBitmapFromWicBitmap(&converter, None).ok()?;
            Some(Image { bitmap })
        }
    }

    /// Draw `image` filling `r` (cropped to cover it) with rounded corners.
    pub fn image(&self, image: &Image, r: Rect, radius: f32) {
        // SAFETY: the brush lives only for this call.
        unsafe {
            let size = image.bitmap.GetSize();
            if size.width <= 0.0 || size.height <= 0.0 {
                return;
            }
            let Ok(brush) = self.dc.CreateBitmapBrush(&image.bitmap, None, None) else { return };
            let scale = (r.w / size.width).max(r.h / size.height);
            let (dx, dy) = (r.x + (r.w - size.width * scale) / 2.0, r.y + (r.h - size.height * scale) / 2.0);
            brush.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: dx, M32: dy });
            brush.SetOpacity(self.opacity.get());
            let rounded = D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius };
            self.dc.FillRoundedRectangle(&rounded, &brush);
        }
    }
}

/// A decoded picture living on the GPU.
#[derive(Clone)]
pub struct Image {
    bitmap: windows::Win32::Graphics::Direct2D::ID2D1Bitmap1,
}

