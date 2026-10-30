//! Drawing and hit-testing for the radial wheel. Pure cairo, no widgets,
//! so it can be rendered to a PNG for checking.
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use gtk::cairo::{Context, Operator};
use gtk::pango;

pub const SIZE: i32 = 340;
const OUTER: f64 = 160.0;
const INNER: f64 = 64.0;
const GAP: f64 = 1.6 * PI / 180.0;

type Rgba = (f64, f64, f64, f64);
const ORANGE: (f64, f64, f64) = (1.0, 0.54, 0.12);
const RING: Rgba = (0.09, 0.09, 0.10, 0.92);
const RING_EDGE: Rgba = (1.0, 1.0, 1.0, 0.10);
const TEXT: Rgba = (1.0, 1.0, 1.0, 0.92);
const DIM: Rgba = (1.0, 1.0, 1.0, 0.55);

/// Start and end angle of a segment. Segment 0 is centred at the top, then clockwise.
pub fn segment_angles(count: usize, index: usize) -> (f64, f64) {
    let span = TAU / count as f64;
    let mid = -FRAC_PI_2 + index as f64 * span;
    (mid - span / 2.0, mid + span / 2.0)
}

/// Which segment (x, y) is over, or None for the centre and outside.
pub fn hit(count: usize, x: f64, y: f64, size: i32) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let half = size as f64 / 2.0;
    let (dx, dy) = (x - half, y - half);
    let r = dx.hypot(dy);
    if !(INNER..=OUTER).contains(&r) {
        return None;
    }
    let angle = dy.atan2(dx) + FRAC_PI_2; // 0 at the top, clockwise positive
    let span = TAU / count as f64;
    Some(((angle + span / 2.0).rem_euclid(TAU) / span) as usize % count)
}

fn set(cr: &Context, (r, g, b, a): Rgba) {
    cr.set_source_rgba(r, g, b, a);
}

fn text(cr: &Context, s: &str, x: f64, y: f64, size: f64, color: Rgba, bold: bool) {
    let layout = pangocairo::functions::create_layout(cr);
    let desc = pango::FontDescription::from_string(&format!("Sans {} {size}", if bold { "Bold" } else { "" }));
    layout.set_font_description(Some(&desc));
    layout.set_text(s);
    let (_ink, logical) = layout.pixel_extents();
    set(cr, color);
    cr.move_to(x - logical.width() as f64 / 2.0, y - logical.height() as f64 / 2.0);
    pangocairo::functions::show_layout(cr, &layout);
}

// cairo calls only fail once the context is broken; there's nothing to do about it mid-draw
pub fn draw(cr: &Context, labels: &[String], hover: Option<usize>, center: &str, subtitle: &str, size: i32) {
    let c = size as f64 / 2.0;
    cr.set_operator(Operator::Clear); // the window is transparent outside the ring
    let _ = cr.paint();
    cr.set_operator(Operator::Over);

    let count = labels.len();
    if count == 0 {
        cr.arc(c, c, OUTER, 0.0, TAU);
        cr.arc_negative(c, c, INNER, TAU, 0.0);
        set(cr, RING);
        let _ = cr.fill();
    }
    for (i, label) in labels.iter().enumerate() {
        let (a0, a1) = segment_angles(count, i);
        let (a0, a1) = (a0 + GAP / 2.0, a1 - GAP / 2.0);
        cr.new_path();
        cr.arc(c, c, OUTER, a0, a1);
        cr.arc_negative(c, c, INNER, a1, a0);
        cr.close_path();
        if hover == Some(i) {
            cr.set_source_rgba(ORANGE.0, ORANGE.1, ORANGE.2, 1.0);
        } else {
            set(cr, RING);
        }
        let _ = cr.fill_preserve();
        set(cr, RING_EDGE);
        cr.set_line_width(1.0);
        let _ = cr.stroke();
        let mid = (a0 + a1) / 2.0;
        let r = (OUTER + INNER) / 2.0 + 4.0;
        let font = if label.chars().count() <= 5 { 11.0 } else { 9.0 };
        let color = if hover == Some(i) { (0.08, 0.08, 0.08, 1.0) } else { TEXT };
        text(cr, label, c + r * mid.cos(), c + r * mid.sin(), font, color, true);
    }

    // centre disc
    cr.arc(c, c, INNER - 6.0, 0.0, TAU);
    cr.set_source_rgba(0.05, 0.05, 0.06, 0.96);
    let _ = cr.fill();
    cr.arc(c, c, INNER - 6.0, 0.0, TAU);
    cr.set_source_rgba(ORANGE.0, ORANGE.1, ORANGE.2, 0.9);
    cr.set_line_width(2.0);
    let _ = cr.stroke();
    let font = if center.chars().count() <= 8 { 11.0 } else { 9.0 };
    text(cr, center, c, c - if subtitle.is_empty() { 0.0 } else { 7.0 }, font, TEXT, true);
    if !subtitle.is_empty() {
        text(cr, subtitle, c, c + 11.0, 7.0, DIM, false);
    }
}
