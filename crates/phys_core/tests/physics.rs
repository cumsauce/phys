//! Physics checks on rendered audio and on the simulation's energy.

use phys_core::render::render;
use phys_core::scene::{Scene, Solver};
use phys_core::sim::Sim;

const SR: u32 = 48_000;

fn scene(json: &str) -> Scene {
    Scene::from_json(json).expect("scene parses")
}

/// Hann-windowed magnitude of `x` at `hz`.
fn level(x: &[f32], hz: f64) -> f64 {
    let n = x.len() as f64;
    let w = 2.0 * std::f64::consts::PI * hz / f64::from(SR);
    let (mut re, mut im) = (0.0, 0.0);
    for (i, s) in x.iter().enumerate() {
        let win = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n).cos();
        let v = f64::from(*s) * win;
        re += v * (w * i as f64).cos();
        im += v * (w * i as f64).sin();
    }
    (re * re + im * im).sqrt() / n
}

/// The strongest frequency within `span` (a fraction) of `near`.
fn peak_near(x: &[f32], near: f64, span: f64) -> f64 {
    let steps = 400;
    let mut best = (0.0, near);
    for i in 0..=steps {
        let f = near * (1.0 - span + 2.0 * span * i as f64 / steps as f64);
        let l = level(x, f);
        if l > best.0 {
            best = (l, f);
        }
    }
    best.1
}

fn mono(json: &str, seconds: f64) -> Vec<f32> {
    let r = render(&scene(json), SR, seconds).expect("renders");
    assert!(!r.blown, "the simulation blew up");
    assert!(r.peak > 0.0, "the render is silent");
    r.mono()
}

const STRING: &str = r#"{
  "parts": [{"name": "s", "shape": {"kind": "string", "length": 0.65, "radius": 0.0004, "f0": 220}}],
  "exciters": [{"name": "hit", "kind": "impulse", "on": {"part": "s", "at": [AT]}}],
  "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "s", "at": [0.37]}}],
  "score": [{"t": 0.01, "exciter": "hit", "strength": 0.001}]
}"#;

#[test]
fn a_string_rings_at_its_f0() {
    let x = mono(&STRING.replace("AT", "0.3"), 1.0);
    let f = peak_near(&x, 220.0, 0.05);
    assert!((f / 220.0 - 1.0).abs() < 0.01, "f0 measured {f}");
}

#[test]
fn striking_a_string_at_its_middle_leaves_out_the_even_harmonics() {
    let x = mono(&STRING.replace("AT", "0.5"), 1.0);
    let (odd, even) = (level(&x, 220.0), level(&x, 440.0));
    assert!(even < odd * 0.01, "f0 {odd}, 2 f0 {even}");
    let y = mono(&STRING.replace("AT", "0.3"), 1.0);
    assert!(level(&y, 440.0) > level(&y, 220.0) * 0.05);
}

#[test]
fn a_free_bar_sounds_its_beam_partials() {
    let x = mono(
        r#"{
      "parts": [{"name": "bar", "shape": {"kind": "bar", "length": 0.3, "width": 0.04, "thickness": 0.012}, "material": "aluminium"}],
      "exciters": [{"name": "hit", "kind": "impulse", "on": {"part": "bar", "at": [0.03]}}],
      "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "bar", "at": [0.97]}}],
      "score": [{"t": 0.01, "exciter": "hit", "strength": 0.01}]
    }"#,
        1.0,
    );
    // f1 = 22.373 / (2 pi L^2) sqrt(E h^2 / (12 rho)).
    let f1 = 22.3733 / (2.0 * std::f64::consts::PI * 0.09)
        * (7.0e10 * 0.012f64.powi(2) / (12.0 * 2700.0)).sqrt();
    for ratio in [1.0, 2.7565, 5.4039] {
        let want = f1 * ratio;
        let got = peak_near(&x, want, 0.05);
        assert!(
            (got / want - 1.0).abs() < 0.02,
            "partial {ratio}: {got} vs {want}"
        );
    }
}

#[test]
fn a_mass_on_a_spring_bobs_at_the_textbook_frequency() {
    // f = sqrt(k / m) / (2 pi) = 50.33 Hz, through each solver.
    for solver in ["sav", "explicit"] {
        let x = mono(
            &r#"{
          "settings": {"solver": "SOLVER"},
          "parts": [{"name": "m", "shape": {"kind": "mass", "mass": 0.1}}],
          "joints": [{"name": "k", "kind": "spring", "a": {"part": "m"}, "stiffness": 1e4}],
          "exciters": [{"name": "hit", "kind": "impulse", "on": {"part": "m"}}],
          "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "m"}}],
          "score": [{"t": 0.0, "exciter": "hit", "strength": 0.01}]
        }"#
            .replace("SOLVER", solver),
            2.0,
        );
        let f = peak_near(&x, 50.33, 0.05);
        assert!((f / 50.33 - 1.0).abs() < 0.005, "{solver}: {f}");
    }
}

const TWO_BARS: &str = r#"{
  "parts": [
    {"name": "a", "shape": {"kind": "bar", "length": 0.3, "width": 0.04, "thickness": 0.012}},
    {"name": "b", "shape": {"kind": "bar", "length": 0.25, "width": 0.04, "thickness": 0.012}, "pos": [0, -0.05, 0]}
  ],
  "joints": [JOINT],
  "exciters": [{"name": "hit", "kind": "impulse", "on": {"part": "a", "at": [0.2]}}],
  "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "b", "at": [0.8]}}],
  "score": [{"t": 0.0, "exciter": "hit", "strength": 0.01}]
}"#;

#[test]
fn a_spring_carries_vibration_from_one_part_to_another() {
    let joined = TWO_BARS.replace(
        "JOINT",
        r#"{"name": "j", "kind": "spring", "a": {"part": "a", "at": [0.9]}, "b": {"part": "b", "at": [0.1]}, "stiffness": 1e5}"#,
    );
    let r = render(&scene(&joined), SR, 0.5).expect("renders");
    assert!(r.peak > 1e-6, "nothing reached the second bar: {}", r.peak);
    let apart = render(&scene(&TWO_BARS.replace("JOINT", "")), SR, 0.5).expect("renders");
    assert_eq!(apart.peak, 0.0);
}

#[test]
fn an_absurdly_stiff_spring_stays_finite_and_loses_no_stability() {
    let joined = TWO_BARS.replace(
        "JOINT",
        r#"{"name": "j", "kind": "spring", "a": {"part": "a", "at": [0.9]}, "b": {"part": "b", "at": [0.1]}, "stiffness": 1e12}"#,
    );
    let mut sim = Sim::compile(&scene(&joined), f64::from(SR)).expect("compiles");
    sim.excite(0, 0.01);
    let mut buf = [0.0f32; 2];
    sim.process(&mut buf);
    let mut last = sim.energy();
    let start = last;
    for _ in 0..48_000 {
        sim.process(&mut buf);
        let e = sim.energy();
        assert!(
            e.is_finite() && e <= last * (1.0 + 1e-9) + 1e-20,
            "{last} -> {e}"
        );
        last = e;
    }
    assert!(!sim.blown());
    assert!(last < start);
}

const RATTLE: &str = r#"{
  "settings": {"solver": "SOLVER"},
  "parts": [
    {"name": "sheet", "shape": {"kind": "plate", "size": [0.5, 0.5], "thickness": 0.002}, "loss": 0.01},
    {"name": "screw", "shape": {"kind": "mass", "mass": 0.005}}
  ],
  "joints": [{"name": "loose", "kind": "spring", "a": {"part": "screw"}, "b": {"part": "sheet", "at": [0.8, 0.8]}, "stiffness": 2e5, "gap": 0.0002}],
  "exciters": [{"name": "hammer", "kind": "mallet", "on": {"part": "sheet", "at": [0.4, 0.4]}, "mass": 0.4, "stiffness": 5e8, "damping": 0}],
  "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "sheet", "at": [0.3, 0.6]}}],
  "score": [{"t": 0.0, "exciter": "hammer", "strength": 3.0}]
}"#;

#[test]
fn energy_never_grows_through_a_strike_and_ten_seconds_of_rattle() {
    let mut sim =
        Sim::compile(&scene(&RATTLE.replace("SOLVER", "sav")), f64::from(SR)).expect("compiles");
    sim.excite(0, 3.0);
    let mut buf = [0.0f32; 2];
    sim.process(&mut buf);
    let start = sim.energy();
    // The hammer carries 0.4 kg at 3 m/s: 1.8 J.
    assert!((start - 1.8).abs() < 0.05, "{start}");
    let mut last = start;
    let mut worst: f64 = 0.0;
    for _ in 0..480_000 {
        sim.process(&mut buf);
        let e = sim.energy();
        assert!(e.is_finite());
        worst = worst.max(e - last);
        last = e;
    }
    assert!(!sim.blown());
    assert!(
        worst <= start * 1e-9,
        "energy rose by {worst} J in one step"
    );
    assert!(last < start * 0.5);
}

#[test]
fn the_two_solvers_agree_on_a_strike() {
    // The energy-stable scheme and plain explicit forces, each refined,
    // should describe the same collision.
    let run = |solver: Solver, os: u32| {
        let mut s = scene(
            r#"{
          "parts": [{"name": "bar", "shape": {"kind": "bar", "length": 0.3, "width": 0.04, "thickness": 0.012}, "material": "aluminium"}],
          "exciters": [{"name": "m", "kind": "mallet", "on": {"part": "bar", "at": [0.4]}, "mass": 0.03, "stiffness": 3e9}],
          "listeners": [{"name": "pu", "kind": "pickup", "on": {"part": "bar", "at": [0.9]}}],
          "score": [{"t": 0.0, "exciter": "m", "strength": 2.0}]
        }"#,
        );
        s.settings.solver = solver;
        s.settings.oversample = Some(os);
        render(&s, SR, 0.05).expect("renders").mono()
    };
    let a = run(Solver::Sav, 8);
    let b = run(Solver::Explicit, 32);
    let (mut diff, mut total) = (0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(&b) {
        diff += f64::from(x - y).powi(2);
        total += f64::from(*x).powi(2);
    }
    assert!(total > 0.0);
    let rel = (diff / total).sqrt();
    assert!(rel < 0.02, "relative difference {rel}");
}

#[test]
fn block_size_does_not_change_a_single_bit() {
    let s = scene(&RATTLE.replace("SOLVER", "sav"));
    let run = |block: usize| {
        let mut sim = Sim::compile(&s, f64::from(SR)).expect("compiles");
        sim.excite(0, 2.0);
        let mut out = vec![0.0f32; 2 * 4800];
        for chunk in out.chunks_mut(2 * block) {
            sim.process(chunk);
        }
        out
    };
    let whole = run(4800);
    assert!(whole.iter().any(|x| *x != 0.0));
    assert_eq!(whole, run(1));
    assert_eq!(whole, run(64));
    assert_eq!(whole, run(4800));
}

#[test]
fn a_port_on_a_missing_part_is_named_in_the_error() {
    let err = Scene::from_json(
        r#"{"parts": [{"name": "m", "shape": {"kind": "mass", "mass": 1}}],
            "joints": [{"name": "j", "kind": "spring", "a": {"part": "ghost"}}]}"#,
    )
    .expect_err("rejected");
    assert!(err.contains("joint `j`") && err.contains("ghost"), "{err}");
}
