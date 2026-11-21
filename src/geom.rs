use std::f64::consts::PI;

use crate::map::Map;

#[inline]
pub(crate) fn dist2d(x0: f64, y0: f64, x1: f64, y1: f64) -> f64 {
    (x1 - x0).hypot(y1 - y0)
}

#[inline]
pub(crate) fn dist3d(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    let dz = b[2] - a[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

#[inline]
pub(crate) fn unit2d(x: f64, y: f64) -> [f64; 2] {
    let len = x.hypot(y);
    if len > 1e-6 {
        [x / len, y / len]
    } else {
        [0.0, 0.0]
    }
}

#[inline]
pub(crate) fn wrap_angle(mut a: f64) -> f64 {
    while a > PI {
        a -= 2.0 * PI;
    }
    while a < -PI {
        a += 2.0 * PI;
    }
    a
}

#[inline]
pub(crate) fn angle_diff(from: f64, to: f64) -> f64 {
    wrap_angle(to - from)
}

#[inline]
pub(crate) fn direct_aim(from: [f64; 3], to: [f64; 3]) -> [f64; 2] {
    let dx = to[0] - from[0];
    let dy = to[1] - from[1];
    let dz = to[2] - from[2];
    let horiz = dx.hypot(dy).max(1e-6);
    [dy.atan2(dx), dz.atan2(horiz)]
}

#[inline]
pub(crate) fn is_ground_level(map: &Map, pos: [f64; 3]) -> bool {
    (pos[2] - map.ground_height(pos[0], pos[1])).abs() <= 2.5
}
