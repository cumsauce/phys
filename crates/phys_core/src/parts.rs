//! The primitives: for each shape, its mode frequencies, its mode shapes and
//! a quadrature rule over its body.
//!
//! This is the only place that knows what a string or a plate is. Everything
//! downstream sees a `Table`: frequencies, a function from a port coordinate
//! to one displacement vector per mode, and sample points for integrals. A
//! finite-element mesh part would produce the same thing.

use std::f64::consts::PI;
use std::sync::Arc;

use crate::material::MaterialProps;
use crate::math::{V3, bessel_j, bessel_zeros};
use crate::scene::{Ends, Shape};

/// Port coordinate -> local displacement vector of every mode.
pub type ShapeFn = Arc<dyn Fn(&[f64], &mut Vec<V3>) + Send + Sync>;

/// One quadrature sample of a part's body.
pub struct Quad {
    pub at: Vec<f64>,
    /// Mass this sample stands for, kg.
    pub dm: f64,
    /// Radiating area this sample stands for, m2.
    pub ds: f64,
}

pub struct Table {
    pub hz: Vec<f64>,
    pub shape: ShapeFn,
    pub quad: Vec<Quad>,
    /// Modal masses when they are known outright (a point mass).
    pub mass: Option<Vec<f64>>,
}

pub struct Limits {
    pub max_hz: f64,
    pub max_modes: usize,
}

fn positive(what: &str, x: f64) -> Result<(), String> {
    if x.is_finite() && x > 0.0 {
        Ok(())
    } else {
        Err(format!("{what}: {x}: give a number > 0"))
    }
}

fn one_of(kind: &str, a: Option<f64>, an: &str, b: Option<f64>, bn: &str) -> Result<(), String> {
    match (a, b) {
        (Some(x), None) => positive(an, x),
        (None, Some(x)) => positive(bn, x),
        _ => Err(format!("{kind}: give exactly one of `{an}` and `{bn}`")),
    }
}

/// Check a shape's numbers.
pub fn check(shape: &Shape) -> Result<(), String> {
    match shape {
        Shape::String {
            length,
            radius,
            f0,
            tension,
        } => {
            positive("length", *length)?;
            positive("radius", *radius)?;
            one_of("string", *f0, "f0", *tension, "tension")
        }
        Shape::Bar {
            length,
            width,
            thickness,
            ..
        } => {
            positive("length", *length)?;
            positive("width", *width)?;
            positive("thickness", *thickness)
        }
        Shape::Plate { size, thickness } => {
            positive("size[0]", size[0])?;
            positive("size[1]", size[1])?;
            positive("thickness", *thickness)
        }
        Shape::Membrane {
            radius,
            thickness,
            f0,
            tension,
        } => {
            positive("radius", *radius)?;
            positive("thickness", *thickness)?;
            one_of("membrane", *f0, "f0", *tension, "tension")
        }
        Shape::Mass { mass } => positive("mass", *mass),
    }
}

/// A port coordinate as a point in the part's local frame.
pub fn local_point(shape: &Shape, at: &[f64]) -> V3 {
    let g = |i: usize| at.get(i).copied().unwrap_or(0.5);
    match shape {
        Shape::String { length, .. } | Shape::Bar { length, .. } => {
            [(g(0) - 0.5) * length, 0.0, 0.0]
        }
        Shape::Plate { size, .. } => [(g(0) - 0.5) * size[0], 0.0, (g(1) - 0.5) * size[1]],
        Shape::Membrane { radius, .. } => [
            (2.0 * g(0) - 1.0) * radius,
            0.0,
            (2.0 * g(1) - 1.0) * radius,
        ],
        Shape::Mass { .. } => [0.0; 3],
    }
}

/// The n-th (1-based) root of cos(b) cosh(b) = 1: a free-free bar.
fn free_bar_root(n: usize) -> f64 {
    let mut b = (2 * n + 1) as f64 * PI / 2.0;
    for _ in 0..8 {
        let f = b.cos() - 1.0 / b.cosh();
        let df = -b.sin() + b.sinh() / (b.cosh() * b.cosh());
        b -= f / df;
    }
    b
}

/// Free-free bar mode shape at u in 0..1 for root `b` (mode `n`, 1-based).
/// The ends reach +-2.
fn free_bar_shape(b: f64, n: usize, u: f64) -> f64 {
    let x = b * u;
    if b < 20.0 {
        let s = (b.cosh() - b.cos()) / (b.sinh() - b.sin());
        x.cosh() + x.cos() - s * (x.sinh() + x.sin())
    } else {
        // cosh and sinh cancel to rounding error; what is left of them is a
        // decaying exponential at each end.
        let far = if n % 2 == 1 { 1.0 } else { -1.0 };
        x.cos() - x.sin() + (-x).exp() + far * (-(b - x)).exp()
    }
}

fn string_table(
    length: f64,
    radius: f64,
    f0: Option<f64>,
    tension: Option<f64>,
    mat: &MaterialProps,
    lim: &Limits,
) -> Table {
    let area = PI * radius * radius;
    let lin = mat.density * area;
    let tension = match (f0, tension) {
        (Some(f), _) => (2.0 * length * f).powi(2) * lin,
        (_, Some(t)) => t,
        _ => 1.0,
    };
    let f0 = (tension / lin).sqrt() / (2.0 * length);
    // Stiffness stretches the partials: f_n = n f0 sqrt(1 + B n^2).
    let inertia = PI * radius.powi(4) / 4.0;
    let b = PI * PI * mat.young * inertia / (tension * length * length);
    let mut hz = Vec::new();
    let mut orders = Vec::new();
    let mut n = 1usize;
    while hz.len() + 2 <= lim.max_modes.max(2) {
        let f = n as f64 * f0 * (1.0 + b * (n * n) as f64).sqrt();
        if f > lim.max_hz {
            break;
        }
        // Two polarisations: along Y and along Z.
        hz.push(f);
        hz.push(f);
        orders.push(n);
        n += 1;
    }
    let pts = (4 * orders.len()).max(256);
    let quad = (0..pts)
        .map(|i| Quad {
            at: vec![(i as f64 + 0.5) / pts as f64],
            dm: lin * length / pts as f64,
            ds: 2.0 * radius * length / pts as f64,
        })
        .collect();
    let shape: ShapeFn = Arc::new(move |at, out| {
        let u = at.first().copied().unwrap_or(0.5);
        out.clear();
        for &n in &orders {
            let s = (n as f64 * PI * u).sin();
            out.push([0.0, s, 0.0]);
            out.push([0.0, 0.0, s]);
        }
    });
    Table {
        hz,
        shape,
        quad,
        mass: None,
    }
}

fn bar_table(
    length: f64,
    width: f64,
    thickness: f64,
    ends: Ends,
    mat: &MaterialProps,
    lim: &Limits,
) -> Table {
    // f = b^2 / (2 pi L^2) sqrt(E I / (rho A)), I / A = h^2 / 12.
    let k = (mat.young * thickness * thickness / (12.0 * mat.density)).sqrt()
        / (2.0 * PI * length * length);
    let mut hz = Vec::new();
    let mut roots = Vec::new();
    for n in 1..=lim.max_modes.max(1) {
        let b = match ends {
            Ends::Free => free_bar_root(n),
            Ends::Supported => n as f64 * PI,
        };
        let f = b * b * k;
        if f > lim.max_hz {
            break;
        }
        hz.push(f);
        roots.push((b, n));
    }
    let pts = (8 * roots.len()).max(256);
    let quad = (0..pts)
        .map(|i| Quad {
            at: vec![(i as f64 + 0.5) / pts as f64],
            dm: mat.density * width * thickness * length / pts as f64,
            ds: width * length / pts as f64,
        })
        .collect();
    let shape: ShapeFn = Arc::new(move |at, out| {
        let u = at.first().copied().unwrap_or(0.5);
        out.clear();
        for &(b, n) in &roots {
            let y = match ends {
                Ends::Free => free_bar_shape(b, n, u),
                Ends::Supported => (b * u).sin(),
            };
            out.push([0.0, y, 0.0]);
        }
    });
    Table {
        hz,
        shape,
        quad,
        mass: None,
    }
}

fn plate_table(size: [f64; 2], thickness: f64, mat: &MaterialProps, lim: &Limits) -> Table {
    // Simply supported: f = (pi / 2) sqrt(D / (rho h)) (m^2 / a^2 + n^2 / b^2).
    let d = mat.young * thickness.powi(3) / (12.0 * (1.0 - mat.poisson * mat.poisson));
    let k = PI / 2.0 * (d / (mat.density * thickness)).sqrt();
    let mut found: Vec<(f64, usize, usize)> = Vec::new();
    let mut m = 1usize;
    while m <= 200 {
        let fm = k * (m * m) as f64 / (size[0] * size[0]);
        if fm > lim.max_hz {
            break;
        }
        let mut n = 1usize;
        while n <= 200 {
            let f = fm + k * (n * n) as f64 / (size[1] * size[1]);
            if f > lim.max_hz {
                break;
            }
            found.push((f, m, n));
            n += 1;
        }
        m += 1;
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    found.truncate(lim.max_modes);
    let m_top = found.iter().map(|f| f.1).max().unwrap_or(1);
    let n_top = found.iter().map(|f| f.2).max().unwrap_or(1);
    let (nx, nz) = ((3 * m_top).max(32), (3 * n_top).max(32));
    let cell = size[0] * size[1] / (nx * nz) as f64;
    let mut quad = Vec::with_capacity(nx * nz);
    for i in 0..nx {
        for j in 0..nz {
            quad.push(Quad {
                at: vec![(i as f64 + 0.5) / nx as f64, (j as f64 + 0.5) / nz as f64],
                dm: mat.density * thickness * cell,
                ds: cell,
            });
        }
    }
    let hz = found.iter().map(|f| f.0).collect();
    let orders: Vec<(usize, usize)> = found.iter().map(|f| (f.1, f.2)).collect();
    let shape: ShapeFn = Arc::new(move |at, out| {
        let u = at.first().copied().unwrap_or(0.5);
        let v = at.get(1).copied().unwrap_or(0.5);
        out.clear();
        for &(m, n) in &orders {
            let y = (m as f64 * PI * u).sin() * (n as f64 * PI * v).sin();
            out.push([0.0, y, 0.0]);
        }
    });
    Table {
        hz,
        shape,
        quad,
        mass: None,
    }
}

/// Samples of J_m(j r) for r in 0..1, so mode shapes are a table lookup.
const RADIAL: usize = 512;

fn membrane_table(
    radius: f64,
    thickness: f64,
    f0: Option<f64>,
    tension: Option<f64>,
    mat: &MaterialProps,
    lim: &Limits,
) -> Table {
    const J01: f64 = 2.404_825_557_695_773;
    let areal = mat.density * thickness;
    let tension = match (f0, tension) {
        (Some(f), _) => areal * (2.0 * PI * radius * f / J01).powi(2),
        (_, Some(t)) => t,
        _ => 1.0,
    };
    let c = (tension / areal).sqrt();
    // f = j c / (2 pi R). The zero limit is capped: the mode cap takes the
    // lowest of them anyway.
    let x_max = (2.0 * PI * radius * lim.max_hz / c).min(45.0);
    // (frequency, angular order, zero, sine orientation)
    let mut found: Vec<(f64, u32, f64, bool)> = Vec::new();
    for m in 0..40u32 {
        let zeros = bessel_zeros(m, x_max);
        if zeros.is_empty() {
            break;
        }
        for j in zeros {
            let f = j * c / (2.0 * PI * radius);
            found.push((f, m, j, false));
            if m > 0 {
                found.push((f, m, j, true));
            }
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.3.cmp(&b.3)));
    found.truncate(lim.max_modes);
    let tables: Vec<(u32, bool, Vec<f64>)> = found
        .iter()
        .map(|&(_, m, j, sine)| {
            let t = (0..=RADIAL)
                .map(|i| bessel_j(m, j * i as f64 / RADIAL as f64))
                .collect();
            (m, sine, t)
        })
        .collect();
    let (nr, nt) = (40usize, 96usize);
    let mut quad = Vec::with_capacity(nr * nt);
    for i in 0..nr {
        let r = (i as f64 + 0.5) / nr as f64;
        for k in 0..nt {
            let th = 2.0 * PI * (k as f64 + 0.5) / nt as f64;
            let ds = radius * radius * r * (1.0 / nr as f64) * (2.0 * PI / nt as f64);
            quad.push(Quad {
                at: vec![0.5 * (1.0 + r * th.cos()), 0.5 * (1.0 + r * th.sin())],
                dm: areal * ds,
                ds,
            });
        }
    }
    let hz = found.iter().map(|f| f.0).collect();
    let shape: ShapeFn = Arc::new(move |at, out| {
        let x = 2.0 * at.first().copied().unwrap_or(0.5) - 1.0;
        let z = 2.0 * at.get(1).copied().unwrap_or(0.5) - 1.0;
        let r = (x * x + z * z).sqrt().min(1.0);
        let th = z.atan2(x);
        let pos = r * RADIAL as f64;
        let i = (pos as usize).min(RADIAL - 1);
        let frac = pos - i as f64;
        out.clear();
        for (m, sine, t) in &tables {
            let radial = t[i] + frac * (t[i + 1] - t[i]);
            let ang = if *sine {
                (*m as f64 * th).sin()
            } else {
                (*m as f64 * th).cos()
            };
            out.push([0.0, radial * ang, 0.0]);
        }
    });
    Table {
        hz,
        shape,
        quad,
        mass: None,
    }
}

/// The mode table of a shape. `check` must have passed.
pub fn table(shape: &Shape, mat: &MaterialProps, lim: &Limits) -> Table {
    match shape {
        Shape::String {
            length,
            radius,
            f0,
            tension,
        } => string_table(*length, *radius, *f0, *tension, mat, lim),
        Shape::Bar {
            length,
            width,
            thickness,
            ends,
        } => bar_table(*length, *width, *thickness, *ends, mat, lim),
        Shape::Plate { size, thickness } => plate_table(*size, *thickness, mat, lim),
        Shape::Membrane {
            radius,
            thickness,
            f0,
            tension,
        } => membrane_table(*radius, *thickness, *f0, *tension, mat, lim),
        Shape::Mass { mass } => Table {
            // Three free translations: oscillators with no spring.
            hz: vec![0.0; 3],
            shape: Arc::new(|_, out| {
                out.clear();
                out.extend([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
            }),
            quad: Vec::new(),
            mass: Some(vec![*mass; 3]),
        },
    }
}

/// A mesh to draw a part with. Every vertex knows its port coordinate, so
/// mode shapes can be sampled there.
pub struct Preview {
    /// "surface" (triangles), "line" (a polyline) or "point".
    pub kind: &'static str,
    /// Local positions.
    pub verts: Vec<V3>,
    pub at: Vec<Vec<f64>>,
    pub tris: Vec<u32>,
}

/// A grid of (nu + 1) x (nv + 1) points as a slab `thickness` deep: a top
/// and a bottom face, joined by a wall along `rim` (grid indices i, j).
fn slab(
    nu: usize,
    nv: usize,
    thickness: f64,
    rim: &[(usize, usize)],
    mut point: impl FnMut(f64, f64) -> (V3, Vec<f64>),
) -> Preview {
    let (mut verts, mut at, mut tris) = (Vec::new(), Vec::new(), Vec::new());
    for face in [0.5, -0.5] {
        for i in 0..=nu {
            for j in 0..=nv {
                let (mut p, a) = point(i as f64 / nu as f64, j as f64 / nv as f64);
                p[1] += face * thickness;
                verts.push(p);
                at.push(a);
            }
        }
    }
    let row = (nv + 1) as u32;
    let below = ((nu + 1) * (nv + 1)) as u32;
    for i in 0..nu as u32 {
        for j in 0..nv as u32 {
            let a = i * row + j;
            tris.extend([a, a + 1, a + row, a + 1, a + row + 1, a + row]);
            let b = a + below;
            tris.extend([b, b + row, b + 1, b + 1, b + row, b + row + 1]);
        }
    }
    for pair in rim.windows(2) {
        let a = pair[0].0 as u32 * row + pair[0].1 as u32;
        let b = pair[1].0 as u32 * row + pair[1].1 as u32;
        tris.extend([a, a + below, b, b, a + below, b + below]);
    }
    Preview {
        kind: "surface",
        verts,
        at,
        tris,
    }
}

/// The edge of a rectangular grid, all the way round.
fn perimeter(nu: usize, nv: usize) -> Vec<(usize, usize)> {
    let mut rim: Vec<(usize, usize)> = (0..=nv).map(|j| (0, j)).collect();
    rim.extend((1..=nu).map(|i| (i, nv)));
    rim.extend((0..nv).rev().map(|j| (nu, j)));
    rim.extend((0..nu).rev().map(|i| (i, 0)));
    rim
}

/// The least radius a string is drawn with, so it can be seen and clicked.
const STRING_DRAWN: f64 = 0.0012;

pub fn preview(shape: &Shape) -> Preview {
    match shape {
        Shape::String { radius, .. } => {
            // A tube: a ring of points around each point of the centre line.
            let (n, sides) = (96usize, 6usize);
            let r = radius.max(STRING_DRAWN);
            let (mut verts, mut at, mut tris) = (Vec::new(), Vec::new(), Vec::new());
            for i in 0..=n {
                let u = i as f64 / n as f64;
                let c = local_point(shape, &[u]);
                for k in 0..sides {
                    let th = 2.0 * PI * k as f64 / sides as f64;
                    verts.push([c[0], c[1] + r * th.cos(), c[2] + r * th.sin()]);
                    at.push(vec![u]);
                }
            }
            for i in 0..n as u32 {
                for k in 0..sides as u32 {
                    let (a, b) = (
                        i * sides as u32 + k,
                        i * sides as u32 + (k + 1) % sides as u32,
                    );
                    let s = sides as u32;
                    tris.extend([a, b, a + s, b, b + s, a + s]);
                }
            }
            Preview {
                kind: "surface",
                verts,
                at,
                tris,
            }
        }
        Shape::Bar {
            width, thickness, ..
        } => slab(48, 1, *thickness, &perimeter(48, 1), |u, v| {
            let mut p = local_point(shape, &[u]);
            p[2] = (v - 0.5) * width;
            (p, vec![u])
        }),
        Shape::Plate { thickness, .. } => slab(24, 24, *thickness, &perimeter(24, 24), |u, v| {
            (local_point(shape, &[u, v]), vec![u, v])
        }),
        Shape::Membrane { thickness, .. } => {
            let rim: Vec<(usize, usize)> = (0..=48).map(|j| (12, j)).collect();
            slab(12, 48, thickness.max(0.001), &rim, |r, t| {
                let th = 2.0 * PI * t;
                let at = vec![0.5 * (1.0 + r * th.cos()), 0.5 * (1.0 + r * th.sin())];
                (local_point(shape, &at), at)
            })
        }
        Shape::Mass { .. } => Preview {
            kind: "point",
            verts: vec![[0.0; 3]],
            at: vec![Vec::new()],
            tris: Vec::new(),
        },
    }
}
