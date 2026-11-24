//! Baking: a part becomes a mode table the simulation can run.
//!
//! Modal masses and radiation weights are integrals of the mode shapes over
//! the part's body, done by quadrature, so a new shape only has to supply its
//! frequencies, its shapes and its sample points.

use crate::math::{M3, V3, dot, mul_t, rotation};
use crate::parts::{self, Limits, ShapeFn};
use crate::scene::{Part, Port, Shape};

#[derive(Clone, Copy, Debug)]
pub struct Mode {
    pub hz: f64,
    /// Decay rate, 1/s (amplitude falls as exp(-sigma t)).
    pub sigma: f64,
    /// Modal mass, kg.
    pub mass: f64,
    /// How strongly the mode's velocity reaches a microphone, m2.
    pub rad: f64,
}

pub struct Bake {
    pub modes: Vec<Mode>,
    pub shape: ShapeFn,
    pub rot: M3,
    pub pos: V3,
}

/// The local normal every primitive vibrates along.
pub const NORMAL: V3 = [0.0, 1.0, 0.0];

pub fn bake(part: &Part, max_hz: f64) -> Result<Bake, String> {
    let name = &part.name;
    parts::check(&part.shape).map_err(|e| format!("part `{name}`: {e}"))?;
    let mat = part
        .material
        .props()
        .map_err(|e| format!("part `{name}`: {e}"))?;
    let lim = Limits {
        max_hz,
        max_modes: part.modes.unwrap_or(96),
    };
    let t = parts::table(&part.shape, &mat, &lim);
    let n = t.hz.len();
    let is_mass = matches!(part.shape, Shape::Mass { .. });
    if n == 0 {
        return Err(format!(
            "part `{name}`: no mode below {max_hz:.0} Hz: make it larger, thinner or softer"
        ));
    }
    let (mut mass, mut net, mut sq) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let mut area = 0.0;
    let mut buf = Vec::with_capacity(n);
    for q in &t.quad {
        (t.shape)(&q.at, &mut buf);
        area += q.ds;
        for (k, phi) in buf.iter().enumerate() {
            let p2 = dot(*phi, *phi);
            mass[k] += q.dm * p2;
            net[k] += q.ds * dot(*phi, NORMAL);
            sq[k] += q.ds * p2;
        }
    }
    if let Some(m) = t.mass {
        mass = m;
    }
    let loss = part.loss.unwrap_or(mat.loss);
    let floor = part.damping.unwrap_or(if is_mass { 0.0 } else { 1.0 });
    let tune = part.tune.unwrap_or(1.0);
    let modes = (0..n)
        .map(|k| {
            // The net volume a mode moves radiates at any frequency; the
            // rest of its motion radiates less. A rough stand-in for a real
            // radiation model.
            let net_k: f64 = net[k];
            let sign = if net_k < 0.0 { -1.0 } else { 1.0 };
            Mode {
                hz: t.hz[k] * tune,
                sigma: floor + std::f64::consts::PI * t.hz[k] * tune * loss,
                mass: mass[k],
                rad: sign * (net_k.abs() + 0.3 * (area * sq[k]).sqrt()),
            }
        })
        .collect();
    Ok(Bake {
        modes,
        shape: t.shape,
        rot: rotation(part.rot),
        pos: part.pos,
    })
}

/// Eigenvalues and eigenvectors of a symmetric matrix (row-major, n x n) by
/// cyclic Jacobi rotations. Returns the values and V (row-major, the
/// vectors in its columns), with `a` = V diag(values) V^T.
fn eigh(mut a: Vec<f64>, n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    let scale: f64 = a.iter().map(|x| x * x).sum::<f64>().max(f64::MIN_POSITIVE);
    for _ in 0..60 {
        let mut off = 0.0;
        for p in 0..n {
            for q in p + 1..n {
                off += a[p * n + q] * a[p * n + q];
            }
        }
        if off <= 1e-28 * scale {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq == 0.0 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
                for k in 0..n {
                    let (vkp, vkq) = (v[k * n + p], v[k * n + q]);
                    v[k * n + p] = c * vkp - s * vkq;
                    v[k * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    ((0..n).map(|i| a[i * n + i]).collect(), v)
}

impl Bake {
    /// A copy that shares the shape function.
    pub fn copy(&self) -> Bake {
        Bake {
            modes: self.modes.clone(),
            shape: self.shape.clone(),
            rot: self.rot,
            pos: self.pos,
        }
    }

    /// Tie points of the part to the ground with springs and solve its
    /// modes again: each spring is the weight of every mode at its point
    /// (from `weights`) and a stiffness, N/m. The part keeps its number of
    /// modes; they become the modes of the part as the springs hold it,
    /// lowest first, each with a modal mass of 1 kg. A mode's decay rate and
    /// radiation follow from the old modes it is made of.
    pub fn load(&mut self, springs: &[(Vec<f64>, f64)]) {
        let n = self.modes.len();
        if springs.is_empty() || n == 0 {
            return;
        }
        // M^-1/2 K M^-1/2, K the modal stiffness plus the springs.
        let root: Vec<f64> = self.modes.iter().map(|m| m.mass.sqrt()).collect();
        let mut a = vec![0.0; n * n];
        for (i, m) in self.modes.iter().enumerate() {
            a[i * n + i] = (2.0 * std::f64::consts::PI * m.hz).powi(2);
        }
        for (w, k) in springs {
            for i in 0..n {
                for j in 0..n {
                    a[i * n + j] += k * w[i] * w[j] / (root[i] * root[j]);
                }
            }
        }
        let (values, v) = eigh(a, n);
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| values[*a].total_cmp(&values[*b]));
        // mix[new][old]: how much of each old mode a new one holds.
        let mut mix = vec![0.0; n * n];
        let mut modes = Vec::with_capacity(n);
        for (new, &col) in order.iter().enumerate() {
            let (mut sigma, mut rad) = (0.0, 0.0);
            for old in 0..n {
                let part = v[old * n + col];
                let u = part / root[old];
                mix[new * n + old] = u;
                sigma += part * part * self.modes[old].sigma;
                rad += u * self.modes[old].rad;
            }
            modes.push(Mode {
                hz: values[col].max(0.0).sqrt() / (2.0 * std::f64::consts::PI),
                sigma,
                mass: 1.0,
                rad,
            });
        }
        let old = self.shape.clone();
        self.shape = std::sync::Arc::new(move |at: &[f64], out: &mut Vec<V3>| {
            let mut raw = Vec::with_capacity(n);
            old(at, &mut raw);
            out.clear();
            for new in 0..n {
                let mut phi = [0.0; 3];
                for (u, r) in mix[new * n..(new + 1) * n].iter().zip(&raw) {
                    for (p, r) in phi.iter_mut().zip(r) {
                        *p += u * r;
                    }
                }
                out.push(phi);
            }
        });
        self.modes = modes;
    }

    /// The weight of every mode for a force or a reading at `port` along the
    /// world direction `dir`.
    pub fn weights(&self, port: &Port, dir: V3) -> Vec<f64> {
        let local = mul_t(&self.rot, dir);
        let mut buf = Vec::with_capacity(self.modes.len());
        (self.shape)(&port.at, &mut buf);
        buf.iter().map(|phi| dot(*phi, local)).collect()
    }

    /// The part's normal in world space.
    pub fn normal(&self) -> V3 {
        crate::math::mul(&self.rot, NORMAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::MaterialSpec;

    fn part(shape: Shape, material: &str) -> Part {
        Part {
            name: "p".into(),
            shape,
            material: MaterialSpec::Name(material.into()),
            pos: [0.0; 3],
            rot: [0.0; 3],
            loss: None,
            damping: None,
            modes: None,
            tune: None,
        }
    }

    #[test]
    fn string_modal_mass_is_half_its_mass() {
        let (length, radius) = (0.65, 0.0004);
        let p = part(
            Shape::String {
                length,
                radius,
                f0: Some(110.0),
                tension: None,
            },
            "steel",
        );
        let b = bake(&p, 5000.0).expect("bakes");
        let total = 7850.0 * std::f64::consts::PI * radius * radius * length;
        for m in &b.modes {
            assert!((m.mass / (total / 2.0) - 1.0).abs() < 1e-6, "{}", m.mass);
        }
        assert!((b.modes[0].hz - 110.0).abs() < 0.2);
    }

    #[test]
    fn free_bar_partials_follow_the_beam_ratios() {
        let p = part(
            Shape::Bar {
                length: 0.3,
                width: 0.03,
                thickness: 0.01,
                ends: crate::scene::Ends::Free,
            },
            "aluminium",
        );
        let b = bake(&p, 16000.0).expect("bakes");
        let f: Vec<f64> = b.modes.iter().map(|m| m.hz).collect();
        assert!((f[1] / f[0] - 2.7565).abs() < 0.002, "{}", f[1] / f[0]);
        assert!((f[2] / f[0] - 5.4039).abs() < 0.003, "{}", f[2] / f[0]);
        // Free-free beam functions are normalised so the modal mass is the
        // bar's whole mass.
        let total = 2700.0 * 0.3 * 0.03 * 0.01;
        for m in b.modes.iter().take(6) {
            assert!((m.mass / total - 1.0).abs() < 0.01, "{}", m.mass / total);
        }
    }

    #[test]
    fn plate_modal_mass_is_a_quarter_of_its_mass() {
        let p = part(
            Shape::Plate {
                size: [0.4, 0.3],
                thickness: 0.003,
            },
            "wood",
        );
        let b = bake(&p, 4000.0).expect("bakes");
        let total = 450.0 * 0.4 * 0.3 * 0.003;
        for m in &b.modes {
            assert!((m.mass / (total / 4.0) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn membrane_partials_follow_the_bessel_ratios() {
        let p = part(
            Shape::Membrane {
                radius: 0.15,
                thickness: 0.00025,
                f0: Some(120.0),
                tension: None,
            },
            "mylar",
        );
        let b = bake(&p, 2000.0).expect("bakes");
        let f: Vec<f64> = b.modes.iter().map(|m| m.hz).collect();
        assert!((f[0] - 120.0).abs() < 0.01);
        // (1,1) twice, then (2,1) twice, then (0,2).
        assert!((f[1] / f[0] - 1.5933).abs() < 1e-3);
        assert!((f[3] / f[0] - 2.1355).abs() < 1e-3);
        assert!((f[5] / f[0] - 2.2954).abs() < 1e-3);
        assert!(b.modes.iter().all(|m| m.mass > 0.0));
    }
}
