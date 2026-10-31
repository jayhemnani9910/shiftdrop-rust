//! The wheel's geometry, shared by every front end: sizes in logical pixels,
//! segment angles and hit-testing. Drawing stays with each toolkit.
use std::f64::consts::{FRAC_PI_2, PI, TAU};

pub const SIZE: i32 = 340;
pub const OUTER: f64 = 160.0;
pub const INNER: f64 = 64.0;
pub const GAP: f64 = 1.6 * PI / 180.0;

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
