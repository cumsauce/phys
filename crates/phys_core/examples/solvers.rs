//! Solver comparison: how each joint solver converges, what it costs, and
//! what it does to the energy.
//!
//!   cargo run --release -p phys_core --example solvers
//!
//! For each scene the reference is the explicit solver at 64x oversampling.
//! "error" is the RMS difference from that reference over the first 30 ms
//! (before a chaotic rattle can pull two correct answers apart), "growth"
//! the largest single-step rise of the discrete energy relative to the
//! starting energy, over 2 s.

use std::time::Instant;

use phys_core::render::render;
use phys_core::scene::{Scene, Solver};
use phys_core::sim::Sim;

const SR: u32 = 48_000;

const SCENES: &[(&str, &str)] = &[
    (
        "hard mallet on a bar (contact only)",
        r#"{
      "parts": [{"name": "bar", "shape": {"kind": "bar", "length": 0.3, "width": 0.04, "thickness": 0.012}, "material": "aluminium"}],
      "exciters": [{"name": "m", "kind": "mallet", "on": {"part": "bar", "at": [0.4]}, "mass": 0.03, "stiffness": 3e9, "damping": 0}],
      "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "bar", "at": [0.9]}}],
      "score": [{"t": 0.0, "exciter": "m", "strength": 2.0}]
    }"#,
    ),
    (
        "hammer on a sheet with a loose screw (contact + rattle)",
        r#"{
      "parts": [
        {"name": "sheet", "shape": {"kind": "plate", "size": [0.5, 0.5], "thickness": 0.002}, "loss": 0.01},
        {"name": "screw", "shape": {"kind": "mass", "mass": 0.005}}
      ],
      "joints": [{"name": "loose", "kind": "spring", "a": {"part": "screw"}, "b": {"part": "sheet", "at": [0.8, 0.8]}, "stiffness": 2e5, "gap": 0.0002}],
      "exciters": [{"name": "m", "kind": "mallet", "on": {"part": "sheet", "at": [0.4, 0.4]}, "mass": 0.4, "stiffness": 5e8, "damping": 0}],
      "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "sheet", "at": [0.3, 0.6]}}],
      "score": [{"t": 0.0, "exciter": "m", "strength": 3.0}]
    }"#,
    ),
    (
        "string tied to a plate by a stiff spring (linear joint)",
        r#"{
      "parts": [
        {"name": "top", "shape": {"kind": "plate", "size": [0.4, 0.3], "thickness": 0.003}, "material": "wood"},
        {"name": "string", "shape": {"kind": "string", "length": 0.65, "radius": 0.0004, "f0": 196}}
      ],
      "joints": [{"name": "bridge", "kind": "spring", "a": {"part": "string", "at": [0.95]}, "b": {"part": "top", "at": [0.7, 0.45]}, "stiffness": 5e6}],
      "exciters": [{"name": "m", "kind": "impulse", "on": {"part": "string", "at": [0.18]}}],
      "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "top", "at": [0.3, 0.6]}}],
      "score": [{"t": 0.0, "exciter": "m", "strength": 0.001}]
    }"#,
    ),
];

fn with(json: &str, solver: Solver, os: u32) -> Scene {
    let mut s = Scene::from_json(json).expect("scene parses");
    s.settings.regulate = std::env::var("NO_REGULATE").is_err();
    s.settings.gauge = std::env::var("GAUGE").ok().and_then(|g| g.parse().ok());
    s.settings.solver = solver;
    s.settings.oversample = Some(os);
    s
}

fn main() {
    for (title, json) in SCENES {
        println!("\n{title}");
        let reference = render(&with(json, Solver::Explicit, 64), SR, 0.03)
            .expect("renders")
            .mono();
        let power: f64 = reference.iter().map(|x| f64::from(*x).powi(2)).sum();
        println!("  solver    os   error    growth      cost (x real time)");
        for solver in [Solver::Sav, Solver::Explicit] {
            for os in [1u32, 2, 4, 8, 16] {
                let scene = with(json, solver, os);
                let take = render(&scene, SR, 0.03).expect("renders");
                let diff: f64 = take
                    .mono()
                    .iter()
                    .zip(&reference)
                    .map(|(a, b)| f64::from(a - b).powi(2))
                    .sum();
                let error = (diff / power).sqrt();

                let mut sim = Sim::compile(&scene, f64::from(SR)).expect("compiles");
                let ex = sim.exciter("m").expect("exciter");
                sim.excite(ex, scene.score[0].strength);
                let mut buf = [0.0f32; 2];
                sim.process(&mut buf);
                let start = sim.energy();
                let (mut last, mut growth) = (start, 0.0f64);
                let t0 = Instant::now();
                for _ in 0..2 * SR {
                    sim.process(&mut buf);
                    let e = sim.energy();
                    growth = growth.max((e - last) / start);
                    last = e;
                }
                let took = t0.elapsed().as_secs_f64();
                let state = if sim.blown() || !last.is_finite() {
                    "BLEW UP".to_string()
                } else {
                    format!("{growth:9.2e}")
                };
                println!(
                    "  {:9} {os:2}  {:7.4}  {state}  {:8.0}",
                    format!("{solver:?}").to_lowercase(),
                    if error.is_finite() { error } else { f64::NAN },
                    2.0 / took
                );
            }
        }
    }
}
