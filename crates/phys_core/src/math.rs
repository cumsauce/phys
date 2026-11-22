//! Small 3-vector helpers and the Bessel functions the membrane needs.

use std::f64::consts::PI;

pub type V3 = [f64; 3];
/// Row-major rotation matrix.
pub type M3 = [[f64; 3]; 3];

pub fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// The unit vector along `a`, or `None` when it has no length.
pub fn unit(a: V3) -> Option<V3> {
    let l = len(a);
    (l.is_finite() && l > 1e-12).then(|| [a[0] / l, a[1] / l, a[2] / l])
}

/// Rotation from Euler angles in degrees, applied about X, then Y, then Z.
pub fn rotation(deg: V3) -> M3 {
    let (sx, cx) = deg[0].to_radians().sin_cos();
    let (sy, cy) = deg[1].to_radians().sin_cos();
    let (sz, cz) = deg[2].to_radians().sin_cos();
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}

pub fn mul(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}

/// `m` transposed times `v`: world direction to local direction.
pub fn mul_t(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

/// Bessel function of the first kind, integer order, by Bessel's integral
/// (the trapezoid rule converges exponentially on it once `n > x + m`).
pub fn bessel_j(m: u32, x: f64) -> f64 {
    let n = ((x.abs() + m as f64) as usize + 32).max(64);
    let h = PI / n as f64;
    let f = |t: f64| (m as f64 * t - x * t.sin()).cos();
    let mut s = 0.5 * (f(0.0) + f(PI));
    for i in 1..n {
        s += f(i as f64 * h);
    }
    s * h / PI
}

/// The positive zeros of `J_m` below `limit`, ascending.
pub fn bessel_zeros(m: u32, limit: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let step = 0.1;
    // The first zero of J_m lies above m (above 2.4 for m = 0).
    let mut x0 = (m as f64).max(0.5);
    let mut y0 = bessel_j(m, x0);
    while x0 < limit {
        let x1 = x0 + step;
        let y1 = bessel_j(m, x1);
        if y0 * y1 < 0.0 {
            let (mut a, mut b, mut ya) = (x0, x1, y0);
            for _ in 0..50 {
                let c = 0.5 * (a + b);
                let yc = bessel_j(m, c);
                if ya * yc <= 0.0 {
                    b = c;
                } else {
                    a = c;
                    ya = yc;
                }
            }
            let z = 0.5 * (a + b);
            if z < limit {
                out.push(z);
            }
        }
        x0 = x1;
        y0 = y1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bessel_zeros_match_tables() {
        let z0 = bessel_zeros(0, 12.0);
        for (got, want) in z0.iter().zip([2.404826, 5.520078, 8.653728, 11.791534]) {
            assert!((got - want).abs() < 1e-5, "{got} vs {want}");
        }
        let z1 = bessel_zeros(1, 8.0);
        for (got, want) in z1.iter().zip([3.831706, 7.015587]) {
            assert!((got - want).abs() < 1e-5, "{got} vs {want}");
        }
        let z2 = bessel_zeros(2, 6.0);
        assert!((z2[0] - 5.135622).abs() < 1e-5);
    }

    #[test]
    fn rotation_about_z_turns_x_to_y() {
        let r = rotation([0.0, 0.0, 90.0]);
        let v = mul(&r, [1.0, 0.0, 0.0]);
        assert!((v[0]).abs() < 1e-12 && (v[1] - 1.0).abs() < 1e-12);
        let back = mul_t(&r, v);
        assert!((back[0] - 1.0).abs() < 1e-12);
    }
}
