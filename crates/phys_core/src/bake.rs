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
    let modes = (0..n)
        .map(|k| {
            // The net volume a mode moves radiates at any frequency; the
            // rest of its motion radiates less. A rough stand-in for a real
            // radiation model.
            let net_k: f64 = net[k];
            let sign = if net_k < 0.0 { -1.0 } else { 1.0 };
            Mode {
                hz: t.hz[k],
                sigma: floor + std::f64::consts::PI * t.hz[k] * loss,
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

impl Bake {
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
