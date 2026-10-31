//! Drawing the wheel and the status pop-up. Direct2D draws into a 32-bit
//! image, and UpdateLayeredWindow puts it on screen with per-pixel alpha, so
//! everything outside the shapes is see-through. Coordinates are logical
//! pixels; `scale` turns them into the monitor's real ones.
use std::f64::consts::PI;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_SWEEP_DIRECTION,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, D2D1CreateFactory, ID2D1Factory,
    ID2D1PathGeometry, ID2D1RenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow};
use windows::core::{Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use shiftdrop::ring::{GAP, INNER, OUTER, SIZE, segment_angles};

pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

const ORANGE: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 0.54, b: 0.12, a: 1.0 };
const RING: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.09, g: 0.09, b: 0.10, a: 0.92 };
const RING_EDGE: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.10 };
const TEXT: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.92 };
const DIM: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.55 };
const ERROR: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 0.48, b: 0.45, a: 1.0 };
// Not visible, but not alpha 0 either: Windows sends drags (and clicks) through
// pixels with alpha 0, and the wheel must catch the drag before it is drawn.
const ALMOST_CLEAR: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 / 255.0 };

/// Pango sizes on Linux are points; at 96 dpi a point is 4/3 px.
fn pt(n: f32) -> f32 {
    n * 4.0 / 3.0
}

/// How a run of text looks.
#[derive(Clone, Copy)]
pub struct Text {
    pub size: f32,
    pub bold: bool,
    pub color: D2D1_COLOR_F,
}

pub struct Painter {
    d2d: ID2D1Factory,
    dw: IDWriteFactory,
}

impl Painter {
    pub fn new() -> Result<Painter> {
        unsafe {
            Ok(Painter {
                d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dw: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
            })
        }
    }

    /// Draw `width` x `height` logical px with `draw`, then show it as `hwnd` at `origin` (screen px).
    pub fn present(&self, hwnd: HWND, origin: POINT, width: f32, height: f32, scale: f32,
                   draw: impl FnOnce(&ID2D1RenderTarget, &Painter) -> Result<()>) -> Result<()> {
        let (w, h) = ((width * scale).ceil() as i32, (height * scale).ceil() as i32);
        unsafe {
            let mem = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let dib = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(mem, dib.into());
            let result = (|| {
                let target = self.d2d.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                    r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: 96.0, // 1 unit = 1 px; scaling is ours
                    dpiY: 96.0,
                    usage: D2D1_RENDER_TARGET_USAGE_NONE,
                    minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
                })?;
                target.BindDC(mem, &RECT { left: 0, top: 0, right: w, bottom: h })?;
                target.BeginDraw();
                target.Clear(Some(&rgba(0.0, 0.0, 0.0, 0.0)));
                target.SetTransform(&Matrix3x2 { M11: scale, M22: scale, ..Default::default() });
                let drawn = draw(&target, self);
                target.EndDraw(None, None)?;
                drawn?;
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                UpdateLayeredWindow(hwnd, None, Some(&origin), Some(&SIZE { cx: w, cy: h }), Some(mem),
                                    Some(&POINT { x: 0, y: 0 }), COLORREF(0), Some(&blend), ULW_ALPHA)
            })();
            SelectObject(mem, old);
            let _ = DeleteObject(dib.into());
            let _ = DeleteDC(mem);
            result
        }
    }

    fn format(&self, size: f32, weight: DWRITE_FONT_WEIGHT, centered: bool) -> Result<IDWriteTextFormat> {
        unsafe {
            let format = self.dw.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL,
                                                  DWRITE_FONT_STRETCH_NORMAL, size, w!("en-us"))?;
            if centered {
                format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            } else {
                format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            }
            Ok(format)
        }
    }

    /// Text centred on (x, y).
    fn centered(&self, rt: &ID2D1RenderTarget, s: &str, (x, y): (f32, f32), style: Text) -> Result<()> {
        let weight = if style.bold { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let format = self.format(style.size, weight, true)?;
        let rect = D2D_RECT_F { left: x - 100.0, top: y - 20.0, right: x + 100.0, bottom: y + 20.0 };
        let text: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let brush = rt.CreateSolidColorBrush(&style.color, None)?;
            rt.DrawText(&text, &format, &rect, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
        }
        Ok(())
    }

    /// Text wrapped to `width` from `(left, top)`; returns its height. Only measures when `rt` is None.
    pub fn paragraph(&self, rt: Option<&ID2D1RenderTarget>, s: &str, (left, top): (f32, f32), width: f32,
                     style: Text) -> Result<f32> {
        let weight = if style.bold { DWRITE_FONT_WEIGHT_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
        let format = self.format(style.size, weight, false)?;
        let text: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let layout = self.dw.CreateTextLayout(&text, &format, width, 10_000.0)?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics)?;
            if let Some(rt) = rt {
                let brush = rt.CreateSolidColorBrush(&style.color, None)?;
                rt.DrawTextLayout(Vector2 { X: left, Y: top }, &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            }
            Ok(metrics.height)
        }
    }

    fn arc_path(&self, figures: &[(Vector2, &[Edge])]) -> Result<ID2D1PathGeometry> {
        unsafe {
            let path = self.d2d.CreatePathGeometry()?;
            let sink = path.Open()?;
            for (start, edges) in figures {
                sink.BeginFigure(*start, D2D1_FIGURE_BEGIN_FILLED);
                for edge in *edges {
                    match edge {
                        Edge::Line(to) => sink.AddLine(*to),
                        Edge::Arc { to, radius, sweep, large } => sink.AddArc(&D2D1_ARC_SEGMENT {
                            point: *to,
                            size: D2D_SIZE_F { width: *radius, height: *radius },
                            rotationAngle: 0.0,
                            sweepDirection: *sweep,
                            arcSize: if *large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                        }),
                    }
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            }
            sink.Close()?;
            Ok(path)
        }
    }
}

enum Edge {
    Line(Vector2),
    Arc { to: Vector2, radius: f32, sweep: D2D1_SWEEP_DIRECTION, large: bool },
}

fn polar(c: f32, r: f64, a: f64) -> Vector2 {
    Vector2 { X: c + (r * a.cos()) as f32, Y: c + (r * a.sin()) as f32 }
}

/// The wheel as wheel.rs draws it on Linux. `visible = false` paints a disc you
/// can't see but that still catches the drag, until the drag turns out to carry files.
pub fn wheel(rt: &ID2D1RenderTarget, p: &Painter, labels: &[String], hover: Option<usize>, center: &str,
             subtitle: &str, visible: bool) -> Result<()> {
    let c = SIZE as f32 / 2.0;
    let fill = |color: D2D1_COLOR_F| unsafe { rt.CreateSolidColorBrush(&color, None) };
    unsafe {
        if !visible {
            let disc = D2D1_ELLIPSE { point: Vector2 { X: c, Y: c }, radiusX: OUTER as f32, radiusY: OUTER as f32 };
            rt.FillEllipse(&disc, &fill(ALMOST_CLEAR)?);
            return Ok(());
        }
        let count = labels.len();
        if count == 0 {
            // a whole ring: outer circle with the inner one cut out
            let circle = |r: f64| -> [Edge; 2] {
                [Edge::Arc { to: polar(c, r, PI), radius: r as f32, sweep: D2D1_SWEEP_DIRECTION_CLOCKWISE, large: false },
                 Edge::Arc { to: polar(c, r, 0.0), radius: r as f32, sweep: D2D1_SWEEP_DIRECTION_CLOCKWISE, large: false }]
            };
            let (outer, inner) = (circle(OUTER), circle(INNER));
            let ring = p.arc_path(&[(polar(c, OUTER, 0.0), &outer), (polar(c, INNER, 0.0), &inner)])?;
            rt.FillGeometry(&ring, &fill(RING)?, None);
        }
        for (i, label) in labels.iter().enumerate() {
            let (a0, a1) = segment_angles(count, i);
            let (a0, a1) = if count == 1 { (a0 + 1e-4, a1 - 1e-4) } else { (a0 + GAP / 2.0, a1 - GAP / 2.0) };
            let large = a1 - a0 > PI;
            let edges = [
                Edge::Arc { to: polar(c, OUTER, a1), radius: OUTER as f32, sweep: D2D1_SWEEP_DIRECTION_CLOCKWISE, large },
                Edge::Line(polar(c, INNER, a1)),
                Edge::Arc { to: polar(c, INNER, a0), radius: INNER as f32, sweep: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, large },
            ];
            let segment = p.arc_path(&[(polar(c, OUTER, a0), &edges)])?;
            let on = hover == Some(i);
            rt.FillGeometry(&segment, &fill(if on { ORANGE } else { RING })?, None);
            rt.DrawGeometry(&segment, &fill(RING_EDGE)?, 1.0, None);
            let mid = (a0 + a1) / 2.0;
            let at = polar(c, (OUTER + INNER) / 2.0 + 4.0, mid);
            let size = pt(if label.chars().count() <= 5 { 11.0 } else { 9.0 });
            let color = if on { rgba(0.08, 0.08, 0.08, 1.0) } else { TEXT };
            p.centered(rt, label, (at.X, at.Y), Text { size, bold: true, color })?;
        }

        // centre disc
        let disc = D2D1_ELLIPSE { point: Vector2 { X: c, Y: c }, radiusX: INNER as f32 - 6.0, radiusY: INNER as f32 - 6.0 };
        rt.FillEllipse(&disc, &fill(rgba(0.05, 0.05, 0.06, 0.96))?);
        rt.DrawEllipse(&disc, &fill(rgba(1.0, 0.54, 0.12, 0.9))?, 2.0, None);
        let size = pt(if center.chars().count() <= 8 { 11.0 } else { 9.0 });
        let y = c - if subtitle.is_empty() { 0.0 } else { 7.0 };
        p.centered(rt, center, (c, y), Text { size, bold: true, color: TEXT })?;
        if !subtitle.is_empty() {
            p.centered(rt, subtitle, (c, c + 11.0), Text { size: pt(7.0), bold: false, color: DIM })?;
        }
    }
    Ok(())
}

/// The status pop-up's layout, in logical px: what's where, for drawing and for clicks.
pub struct StatusLayout {
    pub height: f32,
    pub detail_top: f32,
    pub bar_top: Option<f32>,
    pub buttons: Vec<(&'static str, D2D_RECT_F)>,
}

pub const STATUS_WIDTH: f32 = 380.0;
const PAD_X: f32 = 18.0;
const PAD_Y: f32 = 14.0;
const TITLE: Text = Text { size: 15.0, bold: true, color: TEXT };
const DETAIL: Text = Text { size: 14.0, bold: false, color: D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.6 } };

/// What the pop-up says right now.
pub struct StatusView<'a> {
    pub title: &'a str,
    pub detail: &'a str,
    pub failed: bool,
    pub progress: f32,
    pub hover: Option<usize>,
}

pub fn status_layout(p: &Painter, title: &str, detail: &str, working: bool, buttons: &[&'static str])
                     -> Result<StatusLayout> {
    let inner = STATUS_WIDTH - 2.0 * PAD_X;
    let mut y = PAD_Y + p.paragraph(None, title, (0.0, 0.0), inner, TITLE)? + 8.0;
    let detail_top = y;
    if !detail.is_empty() {
        y += p.paragraph(None, detail, (0.0, 0.0), inner, DETAIL)? + 8.0;
    }
    let bar_top = working.then(|| {
        let top = y;
        y += 6.0 + 10.0;
        top
    });
    let mut right = STATUS_WIDTH - PAD_X;
    let mut placed = vec![];
    for label in buttons.iter().rev() {
        let width = 26.0 + 7.5 * label.chars().count() as f32;
        placed.push((*label, D2D_RECT_F { left: right - width, top: y, right, bottom: y + 30.0 }));
        right -= width + 8.0;
    }
    placed.reverse();
    Ok(StatusLayout { height: y + 30.0 + PAD_Y, detail_top, bar_top, buttons: placed })
}

pub fn status(rt: &ID2D1RenderTarget, p: &Painter, layout: &StatusLayout, view: &StatusView) -> Result<()> {
    let fill = |color: D2D1_COLOR_F| unsafe { rt.CreateSolidColorBrush(&color, None) };
    let inner = STATUS_WIDTH - 2.0 * PAD_X;
    unsafe {
        let card = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: 0.0, top: 0.0, right: STATUS_WIDTH, bottom: layout.height },
            radiusX: 18.0,
            radiusY: 18.0,
        };
        rt.FillRoundedRectangle(&card, &fill(rgba(0.047, 0.047, 0.055, 0.96))?);
        let title = Text { color: if view.failed { ERROR } else { TEXT }, ..TITLE };
        p.paragraph(Some(rt), view.title, (PAD_X, PAD_Y), inner, title)?;
        if !view.detail.is_empty() {
            p.paragraph(Some(rt), view.detail, (PAD_X, layout.detail_top), inner, DETAIL)?;
        }
        if let Some(top) = layout.bar_top {
            let track = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { left: PAD_X, top, right: PAD_X + inner, bottom: top + 6.0 },
                radiusX: 3.0,
                radiusY: 3.0,
            };
            rt.FillRoundedRectangle(&track, &fill(rgba(1.0, 1.0, 1.0, 0.12))?);
            let done = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F { right: PAD_X + inner * view.progress.clamp(0.0, 1.0), ..track.rect },
                ..track
            };
            rt.FillRoundedRectangle(&done, &fill(ORANGE)?);
        }
        for (i, (label, rect)) in layout.buttons.iter().enumerate() {
            let alpha = if view.hover == Some(i) { 0.18 } else { 0.10 };
            rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: *rect, radiusX: 8.0, radiusY: 8.0 },
                                    &fill(rgba(1.0, 1.0, 1.0, alpha))?);
            let middle = ((rect.left + rect.right) / 2.0, (rect.top + rect.bottom) / 2.0);
            p.centered(rt, label, middle, Text { size: 13.0, bold: false, color: TEXT })?;
        }
    }
    Ok(())
}
