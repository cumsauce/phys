//! How many modes fit in real time on one core.
//!
//!   cargo run --release -p phys_core --example capacity
//!
//! A struck steel plate with a rattling screw (one nonlinear joint) and a
//! stiff tie to ground (one linear joint), at growing mode counts. "x real
//! time" above 1 means it keeps up; an audio thread wants a margin of 2 or
//! more.

use std::time::Instant;

use phys_core::scene::Scene;
use phys_core::sim::Sim;

fn main() {
    println!("modes   x real time (48 kHz, 1x)   ns per mode-step");
    for modes in [100usize, 250, 500, 1000, 2000, 4000] {
        let scene = Scene::from_json(&format!(
            r#"{{
          "settings": {{"max_hz": 21000}},
          "parts": [
            {{"name": "sheet", "shape": {{"kind": "plate", "size": [1.37, 1.13], "thickness": 0.0008}}, "modes": {modes}}},
            {{"name": "screw", "shape": {{"kind": "mass", "mass": 0.005}}}}
          ],
          "joints": [
            {{"name": "loose", "kind": "spring", "a": {{"part": "screw"}}, "b": {{"part": "sheet", "at": [0.81, 0.77]}}, "stiffness": 2e5, "gap": 0.0001}},
            {{"name": "tie", "kind": "spring", "a": {{"part": "sheet", "at": [0.13, 0.21]}}, "stiffness": 1e5}}
          ],
          "exciters": [{{"name": "m", "kind": "mallet", "on": {{"part": "sheet", "at": [0.43, 0.37]}}, "mass": 0.1}}],
          "listeners": [{{"name": "mic", "kind": "mic", "pos": [0, 0.5, 0.3]}}]
        }}"#
        ))
        .expect("scene parses");
        let mut sim = Sim::compile(&scene, 48_000.0).expect("compiles");
        let n = sim.info().modes;
        let mut buf = vec![0.0f32; 2 * 512];
        let seconds = 4.0;
        let blocks = (seconds * 48_000.0 / 512.0) as usize;
        let t0 = Instant::now();
        for b in 0..blocks {
            if b % 40 == 0 {
                sim.excite(0, 3.0);
            }
            sim.process(&mut buf);
        }
        let took = t0.elapsed().as_secs_f64();
        println!(
            "{n:5}   {:8.1}                    {:6.2}",
            seconds / took,
            took * 1e9 / (blocks as f64 * 512.0 * n as f64)
        );
    }
}
