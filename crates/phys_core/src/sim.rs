//! The compiled simulation: numbers only, stepped one sample at a time.
//!
//! Every part is a bank of modes. A mode is stepped with the exact two-pole
//! update of a damped oscillator, written as a centred difference scheme so
//! that it has a discrete energy.
//!
//! Joints couple the modes. With the default `sav` solver:
//! * linear springs and their dampers are stepped implicitly. The coupled
//!   system is solved exactly through a small matrix (one row per linear
//!   joint) inverted once at compile time, and is stable at any stiffness;
//! * everything nonlinear (contacts, rattling springs, cubic springs) shares
//!   one auxiliary variable `psi` standing for the square root of their
//!   stored energy, after Ducceschi, Bilbao and Webb (DAFx 2023). The update
//!   is explicit (a rank-one correction) and the discrete energy cannot grow.
//!
//! The `explicit` solver applies every joint force as it stands, with
//! oversampling chosen from the joint stiffness. It is the approach of
//! sounder's rig and is kept as the reference to compare against.

use crate::bake::{Bake, bake};
use crate::math::{V3, len, sub, unit};
use crate::scene::{ExciterKind, JointKind, ListenerKind, Port, Scene, Shape, Solver};

const SPEED_OF_SOUND: f64 = 343.0;
/// Where an idle mallet waits, metres from the surface.
const PARKED: f64 = 1.0;
/// The default gauge: an energy offset, in joules, added to the stored
/// energy before its square root is taken.
const GAUGE: f64 = 1e-12;

/// A weight per mode over one contiguous run of modes.
#[derive(Debug)]
struct Seg {
    start: usize,
    w: Vec<f64>,
}

impl Clone for Seg {
    /// Keeps the capacity: the roaming mallet's weights are filled in later
    /// on the audio thread, where nothing may allocate.
    fn clone(&self) -> Self {
        let mut w = Vec::with_capacity(self.w.capacity());
        w.extend_from_slice(&self.w);
        Seg {
            start: self.start,
            w,
        }
    }
}

/// A sparse vector over all modes: what a port (or a joint, as the
/// difference of two ports) reads from and pushes on.
#[derive(Clone, Debug, Default)]
struct Wv(Vec<Seg>);

impl Wv {
    #[inline]
    fn dot(&self, x: &[f64]) -> f64 {
        let mut s = 0.0;
        for seg in &self.0 {
            let xs = &x[seg.start..seg.start + seg.w.len()];
            for (w, x) in seg.w.iter().zip(xs) {
                s += w * x;
            }
        }
        s
    }

    /// w . (a - b) in one pass.
    #[inline]
    fn dot_diff(&self, a: &[f64], b: &[f64]) -> f64 {
        let mut s = 0.0;
        for seg in &self.0 {
            let r = seg.start..seg.start + seg.w.len();
            for ((w, a), b) in seg.w.iter().zip(&a[r.clone()]).zip(&b[r]) {
                s += w * (a - b);
            }
        }
        s
    }

    /// x += scale * d * w
    #[inline]
    fn push(&self, x: &mut [f64], d: &[f64], scale: f64) {
        for seg in &self.0 {
            let r = seg.start..seg.start + seg.w.len();
            for ((x, d), w) in x[r.clone()].iter_mut().zip(&d[r]).zip(&seg.w) {
                *x += scale * d * w;
            }
        }
    }

    /// x += scale * w
    #[inline]
    fn add(&self, x: &mut [f64], scale: f64) {
        for seg in &self.0 {
            for (x, w) in x[seg.start..seg.start + seg.w.len()].iter_mut().zip(&seg.w) {
                *x += scale * w;
            }
        }
    }

    /// sum of w^2 / mass: one over the mass the port feels.
    fn mobility(&self, mass: &[f64]) -> f64 {
        let mut s = 0.0;
        for seg in &self.0 {
            for (w, m) in seg.w.iter().zip(&mass[seg.start..]) {
                s += w * w / m;
            }
        }
        s
    }
}

#[derive(Clone, Copy, Debug)]
enum Law {
    /// Stored energy k e^2 / 2 + k3 e^4 / 4, e the stretch beyond `gap`.
    Spring { k: f64, k3: f64, gap: f64 },
    /// Stored energy K p^(alpha + 1) / (alpha + 1), p the penetration.
    Contact { k: f64, alpha: f64 },
}

impl Law {
    /// Stored energy and its derivative at elongation `eta`.
    #[inline]
    fn eval(&self, eta: f64) -> (f64, f64) {
        match *self {
            Law::Spring { k, k3, gap } => {
                let e = if eta > gap {
                    eta - gap
                } else if eta < -gap {
                    eta + gap
                } else {
                    return (0.0, 0.0);
                };
                let e2 = e * e;
                (0.5 * k * e2 + 0.25 * k3 * e2 * e2, k * e + k3 * e2 * e)
            }
            Law::Contact { k, alpha } => {
                if eta >= 0.0 {
                    return (0.0, 0.0);
                }
                let p = -eta;
                let pa = p.powf(alpha);
                (k * pa * p / (alpha + 1.0), -k * pa)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Joint {
    w: Wv,
    law: Law,
    /// Added to the port difference: a contact's separation at rest.
    offset: f64,
    /// Spring: N.s/m. Contact: Hunt-Crossley factor, s/m.
    damping: f64,
    /// The mass the joint feels, kg.
    m_eff: f64,
    eta_prev: f64,
    /// Scratch: this step's energy derivative.
    dv: f64,
}

#[derive(Clone, Debug)]
enum Exciter {
    Mallet {
        joint: usize,
        mode: usize,
    },
    Impulse {
        w: Wv,
    },
    /// Index into `pushes`, and the seconds the pull takes.
    Pluck {
        push: usize,
        time: f64,
    },
    /// Index into `bows`, and the bow's pressure, N.
    Bow {
        bow: usize,
        pressure: f64,
    },
}

/// A pluck: a force that rises smoothly, as a finger pulls the part aside,
/// then lets go within a fraction of a millisecond.
#[derive(Clone, Debug)]
struct Push {
    w: Wv,
    force: f64,
    t: usize,
    rise: usize,
    fall: usize,
    on: bool,
}

/// Most probe samples kept from one `process` call.
const PROBE: usize = 8192;

/// How sharply bow friction falls once the part slips, s2/m2.
const BOW_SHARPNESS: f64 = 100.0;

/// A bow: friction against a surface moving at `speed`. The friction force
/// is `pressure` x phi(v), v the part's speed relative to the bow, with
/// phi(v) = sqrt(2a) v exp(-a v^2 + 1/2): it grips near v = 0 and lets go as
/// the part slips. Each step it is treated as a damper on the relative
/// speed with its coefficient frozen, which keeps the update explicit and
/// cannot add energy beyond what the moving bow supplies (after Russo,
/// Ducceschi and Bilbao, DAFx 2022).
#[derive(Clone, Debug)]
struct Bow {
    w: Wv,
    /// (linear joints)^-1 D w, dense: constant while the bow stays put.
    h: Vec<f64>,
    speed: f64,
    pressure: f64,
    speed_to: f64,
    pressure_to: f64,
    /// Scratch: this step's damper coefficient.
    c: f64,
}

#[derive(Clone, Debug)]
struct Tap {
    w: Wv,
    gain: f64,
    line: Vec<f64>,
    pos: usize,
    acc: f64,
}

#[derive(Clone, Debug)]
struct Listener {
    left: f64,
    right: f64,
    taps: Vec<Tap>,
}

/// What the compiler decided, for reports and the UI.
#[derive(Clone, Debug, Default)]
pub struct Info {
    pub modes: usize,
    /// (part name, modes kept, modes baked)
    pub parts: Vec<(String, usize, usize)>,
    pub oversample: usize,
    pub solver: Solver,
    pub linear_joints: usize,
    pub nonlinear_joints: usize,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Sim {
    sample_rate: f64,
    os: usize,
    /// Time step, seconds.
    k: f64,
    solver: Solver,
    regulate: bool,
    // Per mode: q+ = c1 q - c2 q1 + d F.
    c1: Vec<f64>,
    c2: Vec<f64>,
    d: Vec<f64>,
    mass: Vec<f64>,
    /// Discrete stiffness m w^2 of each mode, for the energy.
    stiff: Vec<f64>,
    q: Vec<f64>,
    q1: Vec<f64>,
    qn: Vec<f64>,
    g: Vec<f64>,
    h: Vec<f64>,
    /// Joints stepped implicitly (`sav` solver only).
    linear: Vec<Joint>,
    /// kappa-: what multiplies the previous elongation.
    lin_prev: Vec<f64>,
    /// Row-major J x J: (I + K P)^-1 K.
    lin_m: Vec<f64>,
    lin_y: Vec<f64>,
    /// Joints stepped through `psi`, or explicitly.
    nonlinear: Vec<Joint>,
    psi: f64,
    v_prev: f64,
    gauge: f64,
    /// Re-gauge at every engagement (no fixed gauge was asked for).
    adapt: bool,
    exciters: Vec<Exciter>,
    exciter_names: Vec<String>,
    listeners: Vec<Listener>,
    gain: f64,
    blown: bool,
    info: Info,
    /// Angular frequency of each mode, for `amplitude`.
    omega: Vec<f64>,
    /// The roaming mallet's place in `exciters`.
    hand: Option<usize>,
    pushes: Vec<Push>,
    bows: Vec<Bow>,
    /// What each exciter acts on (empty for the roaming ones).
    probes: Vec<Wv>,
    /// The spot last played: the probe reads the surface speed there.
    probe: Wv,
    /// The probe's signal over the last `process` call.
    probe_buf: Vec<f32>,
    /// The roaming pluck and bow (`Opts::hand`).
    hand_push: Option<usize>,
    hand_bow: Option<usize>,
    /// Scratch for the per-step rank update: which terms are active
    /// (`usize::MAX` is the auxiliary variable, otherwise a bow), and the
    /// small system they make.
    terms: Vec<usize>,
    small_m: Vec<f64>,
    small_y: Vec<f64>,
    /// Modes that belong to parts (mallet masses come after).
    part_modes: usize,
    /// (name, first mode, mode count) of each part, for `adopt`.
    part_ranges: Vec<(String, usize, usize)>,
    /// Steps left in a `hush`, and the factor applied on each.
    hush_left: usize,
    hush_step: f64,
}

fn db(x: f64) -> f64 {
    10f64.powf(x / 20.0)
}

/// Invert a small dense matrix (row-major, n x n) by Gauss-Jordan.
fn invert(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut m = a.to_vec();
    let mut inv = vec![0.0; n * n];
    for i in 0..n {
        inv[i * n + i] = 1.0;
    }
    for col in 0..n {
        let mut best = col;
        for row in col + 1..n {
            if m[row * n + col].abs() > m[best * n + col].abs() {
                best = row;
            }
        }
        let p = m[best * n + col];
        if !(p.is_finite() && p.abs() > 1e-300) {
            return None;
        }
        if best != col {
            for c in 0..n {
                m.swap(best * n + c, col * n + c);
                inv.swap(best * n + c, col * n + c);
            }
        }
        for c in 0..n {
            m[col * n + c] /= p;
            inv[col * n + c] /= p;
        }
        for row in 0..n {
            if row != col {
                let f = m[row * n + col];
                if f != 0.0 {
                    for c in 0..n {
                        m[row * n + c] -= f * m[col * n + c];
                        inv[row * n + c] -= f * inv[col * n + c];
                    }
                }
            }
        }
    }
    Some(inv)
}

/// Solve the small dense system m y = y in place (r x r, row-major), by
/// elimination with pivoting. A singular system leaves y at zero.
fn solve_small(m: &mut [f64], y: &mut [f64], r: usize) {
    for col in 0..r {
        let mut best = col;
        for row in col + 1..r {
            if m[row * r + col].abs() > m[best * r + col].abs() {
                best = row;
            }
        }
        let p = m[best * r + col];
        if !(p.is_finite() && p.abs() > 1e-300) {
            y[..r].iter_mut().for_each(|x| *x = 0.0);
            return;
        }
        if best != col {
            for c in 0..r {
                m.swap(best * r + c, col * r + c);
            }
            y.swap(best, col);
        }
        for row in col + 1..r {
            let f = m[row * r + col] / p;
            if f != 0.0 {
                for c in col..r {
                    m[row * r + c] -= f * m[col * r + c];
                }
                y[row] -= f * y[col];
            }
        }
    }
    for row in (0..r).rev() {
        let mut v = y[row];
        for c in row + 1..r {
            v -= m[row * r + c] * y[c];
        }
        y[row] = v / m[row * r + row];
    }
}

struct Pending {
    name: String,
    a: Option<(usize, Vec<f64>)>,
    b: Option<(usize, Vec<f64>)>,
    law: Law,
    offset: f64,
    damping: f64,
    /// A mallet: the mass that plays the role of `a`.
    mallet: Option<f64>,
}

/// How to compile a scene.
#[derive(Clone, Copy, Debug)]
pub struct Opts {
    /// Leave out modes no joint or exciter can reach. Turn off when strikes
    /// may land anywhere (`Sim::strike`).
    pub prune: bool,
    /// Add a roaming mallet for `Sim::strike`.
    pub hand: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            prune: true,
            hand: false,
        }
    }
}

/// What a compiled simulation was made from: the baked parts and where
/// their modes sit. Lives off the audio thread; turns a point on a part
/// into weights for `Sim::strike`, and feeds the editor's display.
pub struct Rig {
    pub bakes: Vec<Bake>,
    /// First mode of each part.
    pub start: Vec<usize>,
    keep: Vec<Vec<bool>>,
}

impl Rig {
    /// How many modes of `part` are in the simulation.
    pub fn count(&self, part: usize) -> usize {
        self.keep[part].iter().filter(|k| **k).count()
    }

    /// Mode weights at `at` on `part`, along `dir` (default: its normal).
    pub fn weights(&self, part: usize, at: &[f64], dir: Option<V3>) -> Vec<f64> {
        let b = &self.bakes[part];
        let dir = dir.and_then(unit).unwrap_or_else(|| b.normal());
        let port = Port {
            part: String::new(),
            at: at.to_vec(),
        };
        b.weights(&port, dir)
            .iter()
            .zip(&self.keep[part])
            .filter(|(_, k)| **k)
            .map(|(w, _)| *w)
            .collect()
    }

    /// The local displacement of every simulated mode of `part` at `at`.
    pub fn shapes(&self, part: usize, at: &[f64]) -> Vec<V3> {
        let mut buf = Vec::new();
        (self.bakes[part].shape)(at, &mut buf);
        buf.into_iter()
            .zip(&self.keep[part])
            .filter(|(_, k)| **k)
            .map(|(v, _)| v)
            .collect()
    }

    /// Frequencies of the simulated modes of `part`, Hz.
    pub fn hz(&self, part: usize) -> Vec<f64> {
        self.bakes[part]
            .modes
            .iter()
            .zip(&self.keep[part])
            .filter(|(_, k)| **k)
            .map(|(m, _)| m.hz)
            .collect()
    }
}

impl Sim {
    pub fn compile(scene: &Scene, sample_rate: f64) -> Result<Sim, String> {
        Self::compile_with(scene, sample_rate, &Opts::default()).map(|x| x.0)
    }

    pub fn compile_with(
        scene: &Scene,
        sample_rate: f64,
        opts: &Opts,
    ) -> Result<(Sim, Rig), String> {
        scene.validate()?;
        if !(8_000.0..=384_000.0).contains(&sample_rate) {
            return Err(format!("sample rate {sample_rate}: give 8000..384000 Hz"));
        }
        let st = &scene.settings;
        let max_hz = st.max_hz.unwrap_or(16_000.0).min(0.45 * sample_rate);
        let bakes: Vec<Bake> = scene
            .parts
            .iter()
            .map(|p| bake(p, max_hz))
            .collect::<Result<_, _>>()?;
        let find = |port: &Port| -> Result<usize, String> {
            scene
                .parts
                .iter()
                .position(|p| p.name == port.part)
                .ok_or_else(|| format!("no part named `{}`", port.part))
        };
        let direction = |given: Option<V3>, part: usize| -> V3 {
            given.and_then(unit).unwrap_or_else(|| bakes[part].normal())
        };
        let mut warnings = Vec::new();

        // Everything that pushes on a part, as full-length weight vectors.
        let mut pending: Vec<Pending> = Vec::new();
        for j in &scene.joints {
            let a = find(&j.a)?;
            let dir = direction(j.dir, a);
            let b = match &j.b {
                Some(port) => {
                    let b = find(port)?;
                    Some((b, bakes[b].weights(port, dir)))
                }
                None => None,
            };
            let (law, offset) = match j.kind {
                JointKind::Spring => (
                    Law::Spring {
                        k: j.stiffness.unwrap_or(1e4),
                        k3: j.cubic.unwrap_or(0.0),
                        gap: j.gap.unwrap_or(0.0),
                    },
                    0.0,
                ),
                JointKind::Contact => (
                    Law::Contact {
                        k: j.stiffness.unwrap_or(1e8),
                        alpha: j.exponent.unwrap_or(1.5),
                    },
                    j.gap.unwrap_or(0.0),
                ),
            };
            pending.push(Pending {
                name: format!("joint `{}`", j.name),
                a: Some((a, bakes[a].weights(&j.a, dir))),
                b,
                law,
                offset,
                damping: j.damping.unwrap_or(0.0),
                mallet: None,
            });
        }
        enum Src {
            Mallet(usize),
            Impulse(usize, Vec<f64>),
            Pluck(usize, Vec<f64>, f64),
            Bow(usize, Vec<f64>, f64),
            HandPluck,
            HandBow,
        }
        let mut exciter_src: Vec<Src> = Vec::new();
        for e in &scene.exciters {
            let on = find(&e.on)?;
            let dir = direction(e.dir, on);
            let w = bakes[on].weights(&e.on, dir);
            match e.kind {
                ExciterKind::Mallet => {
                    exciter_src.push(Src::Mallet(pending.len()));
                    pending.push(Pending {
                        name: format!("exciter `{}`", e.name),
                        a: None,
                        b: Some((on, w)),
                        law: Law::Contact {
                            k: e.stiffness.unwrap_or(1e8),
                            alpha: 1.5,
                        },
                        offset: 0.0,
                        damping: e.damping.unwrap_or(0.3),
                        mallet: Some(e.mass.unwrap_or(0.02)),
                    });
                }
                ExciterKind::Impulse => exciter_src.push(Src::Impulse(on, w)),
                ExciterKind::Pluck => exciter_src.push(Src::Pluck(on, w, e.time.unwrap_or(0.02))),
                ExciterKind::Bow => exciter_src.push(Src::Bow(on, w, e.pressure.unwrap_or(0.3))),
            }
        }

        let hand_pending = opts.hand.then(|| {
            exciter_src.push(Src::Mallet(pending.len()));
            pending.push(Pending {
                name: "the hand".to_string(),
                a: None,
                b: None,
                law: Law::Contact { k: 1e8, alpha: 1.5 },
                offset: 0.0,
                damping: 0.3,
                mallet: Some(0.02),
            });
            pending.len() - 1
        });

        // A mode nothing can push on never moves: leave it out.
        let mut keep: Vec<Vec<bool>> = bakes
            .iter()
            .map(|b| vec![!opts.prune; b.modes.len()])
            .collect();
        let mut mark = |part: usize, w: &[f64]| {
            for (k, w) in keep[part].iter_mut().zip(w) {
                *k |= w.abs() > 1e-9;
            }
        };
        for p in &pending {
            for (part, w) in p.a.iter().chain(&p.b) {
                mark(*part, w);
            }
        }
        for src in &exciter_src {
            if let Src::Impulse(part, w) | Src::Pluck(part, w, _) | Src::Bow(part, w, _) = src {
                mark(*part, w);
            }
        }
        if opts.hand {
            exciter_src.push(Src::HandPluck);
            exciter_src.push(Src::HandBow);
        }
        let mut start = Vec::with_capacity(bakes.len());
        let mut n = 0usize;
        let mut info_parts = Vec::new();
        for (i, k) in keep.iter().enumerate() {
            start.push(n);
            let kept = k.iter().filter(|x| **x).count();
            n += kept;
            info_parts.push((scene.parts[i].name.clone(), kept, k.len()));
        }
        let part_modes = n;
        let part_ranges: Vec<(String, usize, usize)> = info_parts
            .iter()
            .zip(&start)
            .map(|((name, kept, _), first)| (name.clone(), *first, *kept))
            .collect();
        let mallets = pending.iter().filter(|p| p.mallet.is_some()).count();
        n += mallets;
        let compress = |part: usize, w: &[f64], sign: f64| -> Seg {
            Seg {
                start: start[part],
                w: w.iter()
                    .zip(&keep[part])
                    .filter(|(_, k)| **k)
                    .map(|(w, _)| sign * w)
                    .collect(),
            }
        };

        let mut mass = Vec::with_capacity(n);
        let mut hz = Vec::with_capacity(n);
        let mut sigma = Vec::with_capacity(n);
        for (b, k) in bakes.iter().zip(&keep) {
            for (m, _) in b.modes.iter().zip(k).filter(|(_, k)| **k) {
                if !(m.mass.is_finite() && m.mass > 0.0) {
                    return Err("a mode has no mass: check the part's size".to_string());
                }
                mass.push(m.mass);
                hz.push(m.hz);
                sigma.push(m.sigma);
            }
        }

        let mut joints: Vec<(Joint, bool)> = Vec::new();
        let mut mallet_of: Vec<Option<(usize, usize)>> = vec![None; pending.len()];
        let mut next_mallet = part_modes;
        for (pi, p) in pending.iter().enumerate() {
            let mut segs = Vec::new();
            if let Some(m) = p.mallet {
                mass.push(m);
                hz.push(0.0);
                sigma.push(0.0);
                segs.push(Seg {
                    start: next_mallet,
                    w: vec![1.0],
                });
                mallet_of[pi] = Some((joints.len(), next_mallet));
                next_mallet += 1;
            }
            if hand_pending == Some(pi) {
                // Room for the weights of whichever part it lands on.
                segs.push(Seg {
                    start: 0,
                    w: Vec::with_capacity(part_modes),
                });
            }
            match (&p.a, &p.b) {
                (Some((pa, wa)), Some((pb, wb))) if pa == pb => {
                    let diff: Vec<f64> = wa.iter().zip(wb).map(|(a, b)| a - b).collect();
                    segs.push(compress(*pa, &diff, 1.0));
                }
                (a, b) => {
                    if let Some((pa, wa)) = a {
                        segs.push(compress(*pa, wa, 1.0));
                    }
                    if let Some((pb, wb)) = b {
                        segs.push(compress(*pb, wb, -1.0));
                    }
                }
            }
            let w = Wv(segs);
            let mobility = w.mobility(&mass);
            if mobility <= 0.0 {
                warnings.push(format!(
                    "{}: it acts on nothing that can move (a port on a node line, or a direction the part does not vibrate in)",
                    p.name
                ));
            }
            let linear = matches!(p.law, Law::Spring { k3, gap, .. } if k3 == 0.0 && gap == 0.0);
            joints.push((
                Joint {
                    w,
                    law: p.law,
                    offset: p.offset,
                    damping: p.damping,
                    m_eff: if mobility > 0.0 {
                        1.0 / mobility
                    } else {
                        f64::INFINITY
                    },
                    eta_prev: p.offset,
                    dv: 0.0,
                },
                linear,
            ));
        }

        // Steps per sample.
        let solver = st.solver;
        let os = match (st.oversample, solver) {
            (Some(os), _) => os as usize,
            (None, Solver::Sav) => 1,
            (None, Solver::Explicit) => {
                // Twenty steps across the shortest half period of any joint
                // (the rig's rule). A contact is judged at a 3 m/s landing.
                let mut os = 1usize;
                for (j, _) in &joints {
                    if !j.m_eff.is_finite() {
                        continue;
                    }
                    let k_lin = match j.law {
                        Law::Spring { k, .. } => k,
                        Law::Contact { k, alpha } => {
                            let p = ((alpha + 1.0) * j.m_eff * 9.0 / (2.0 * k))
                                .powf(1.0 / (alpha + 1.0));
                            alpha * k * p.powf(alpha - 1.0)
                        }
                    };
                    let half = std::f64::consts::PI * (j.m_eff / k_lin).sqrt();
                    os = os.max((20.0 / (half * sample_rate)).ceil() as usize);
                }
                if os > 64 {
                    warnings.push(format!(
                        "explicit solver: the stiffest joint asks for {os}x oversampling; held at 64x, so it may be unstable"
                    ));
                }
                os.min(64)
            }
        };
        let k = 1.0 / (sample_rate * os as f64);

        let (mut c1, mut c2, mut d, mut stiff) = (
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
            Vec::with_capacity(n),
        );
        for i in 0..n {
            let w = 2.0 * std::f64::consts::PI * hz[i];
            let s = sigma[i];
            let e = (-s * k).exp();
            let turn = if w >= s {
                ((w * w - s * s).sqrt() * k).cos()
            } else {
                ((s * s - w * w).sqrt() * k).cosh()
            };
            // The exact update, as a centred scheme with coefficient a.
            let a = 1.0 + (s * k).tanh();
            c1.push(2.0 * e * turn);
            c2.push(e * e);
            d.push(k * k / (mass[i] * a));
            stiff.push(mass[i] * (2.0 - 2.0 * e * turn * a).max(0.0) / (k * k));
        }

        let (mut linear, mut nonlinear) = (Vec::new(), Vec::new());
        // Where each pending joint went: (is linear, index).
        let mut placed = Vec::with_capacity(joints.len());
        for (j, lin) in joints {
            if lin && solver == Solver::Sav {
                placed.push((true, linear.len()));
                linear.push(j);
            } else {
                placed.push((false, nonlinear.len()));
                nonlinear.push(j);
            }
        }
        let nl = linear.len();
        let (mut lin_prev, mut lin_m) = (Vec::new(), Vec::new());
        if nl > 0 {
            let mut kp = vec![0.0; nl];
            for (i, j) in linear.iter().enumerate() {
                let ks = match j.law {
                    Law::Spring { k, .. } => k,
                    Law::Contact { .. } => 0.0,
                };
                kp[i] = 0.5 * ks + j.damping / (2.0 * k);
                lin_prev.push(0.5 * ks - j.damping / (2.0 * k));
            }
            // P[i][j] = w_i . D w_j
            let mut dense = vec![0.0; n];
            let mut s = vec![0.0; nl * nl];
            for (j, joint) in linear.iter().enumerate() {
                dense.iter_mut().for_each(|x| *x = 0.0);
                joint.w.push(&mut dense, &d, 1.0);
                for (i, other) in linear.iter().enumerate() {
                    s[i * nl + j] = kp[i] * other.w.dot(&dense) + if i == j { 1.0 } else { 0.0 };
                }
            }
            let inv = invert(&s, nl)
                .ok_or_else(|| "the linear joints form a singular system".to_string())?;
            lin_m = vec![0.0; nl * nl];
            for i in 0..nl {
                for j in 0..nl {
                    lin_m[i * nl + j] = inv[i * nl + j] * kp[j];
                }
            }
        }

        let mut exciters = Vec::new();
        // What each exciter acts on, for the probe.
        let mut probes: Vec<Wv> = Vec::new();
        let mut pushes: Vec<Push> = Vec::new();
        let mut bows: Vec<Bow> = Vec::new();
        let (mut hand, mut hand_push, mut hand_bow) = (None, None, None);
        let roaming = || {
            Wv(vec![Seg {
                start: 0,
                w: Vec::with_capacity(part_modes),
            }])
        };
        let new_push = |w: Wv| Push {
            w,
            force: 0.0,
            t: 0,
            rise: 1,
            fall: 1,
            on: false,
        };
        let new_bow = |w: Wv| Bow {
            w,
            h: vec![0.0; n],
            speed: 0.0,
            pressure: 0.0,
            speed_to: 0.0,
            pressure_to: 0.0,
            c: 0.0,
        };
        for src in exciter_src {
            match src {
                Src::Mallet(pi) => {
                    let (_, mode) = mallet_of[pi].ok_or("a mallet lost its mass")?;
                    if hand_pending == Some(pi) {
                        hand = Some(exciters.len());
                    }
                    probes.push(match &pending[pi].b {
                        Some((part, w)) => Wv(vec![compress(*part, w, 1.0)]),
                        None => Wv::default(),
                    });
                    exciters.push(Exciter::Mallet {
                        joint: placed[pi].1,
                        mode,
                    });
                }
                Src::Impulse(part, w) => {
                    probes.push(Wv(vec![compress(part, &w, 1.0)]));
                    exciters.push(Exciter::Impulse {
                        w: Wv(vec![compress(part, &w, 1.0)]),
                    });
                }
                Src::Pluck(part, w, time) => {
                    probes.push(Wv(vec![compress(part, &w, 1.0)]));
                    exciters.push(Exciter::Pluck {
                        push: pushes.len(),
                        time,
                    });
                    pushes.push(new_push(Wv(vec![compress(part, &w, 1.0)])));
                }
                Src::Bow(part, w, pressure) => {
                    probes.push(Wv(vec![compress(part, &w, 1.0)]));
                    exciters.push(Exciter::Bow {
                        bow: bows.len(),
                        pressure,
                    });
                    bows.push(new_bow(Wv(vec![compress(part, &w, 1.0)])));
                }
                Src::HandPluck => {
                    hand_push = Some(pushes.len());
                    pushes.push(new_push(roaming()));
                }
                Src::HandBow => {
                    hand_bow = Some(bows.len());
                    bows.push(new_bow(roaming()));
                }
            }
        }
        let rank = bows.len() + 1;

        let mut listeners = Vec::new();
        for l in &scene.listeners {
            let pan = (l.pan + 1.0) * std::f64::consts::FRAC_PI_4;
            let mut taps = Vec::new();
            match l.kind {
                ListenerKind::Pickup => {
                    let port = l.on.as_ref().ok_or("a pickup needs `on`")?;
                    let part = find(port)?;
                    let w = bakes[part].weights(port, direction(l.dir, part));
                    taps.push(Tap {
                        w: Wv(vec![compress(part, &w, 1.0)]),
                        gain: 1.0,
                        line: vec![0.0],
                        pos: 0,
                        acc: 0.0,
                    });
                }
                ListenerKind::Mic => {
                    let pos = l.pos.ok_or("a mic needs `pos`")?;
                    for (part, b) in bakes.iter().enumerate() {
                        if matches!(scene.parts[part].shape, Shape::Mass { .. }) {
                            continue;
                        }
                        let rad: Vec<f64> = b.modes.iter().map(|m| m.rad).collect();
                        let dist = len(sub(pos, b.pos));
                        let delay = (dist / SPEED_OF_SOUND * sample_rate).round() as usize;
                        taps.push(Tap {
                            w: Wv(vec![compress(part, &rad, 1.0)]),
                            gain: 1.0 / dist.max(0.05),
                            line: vec![0.0; delay + 1],
                            pos: 0,
                            acc: 0.0,
                        });
                    }
                }
            }
            listeners.push(Listener {
                left: pan.cos() * db(l.gain_db),
                right: pan.sin() * db(l.gain_db),
                taps,
            });
        }
        if listeners.is_empty() {
            warnings.push("no listeners: the scene is silent".to_string());
        }

        let info = Info {
            modes: n,
            parts: info_parts,
            oversample: os,
            solver,
            linear_joints: linear.len(),
            nonlinear_joints: nonlinear.len(),
            warnings,
        };
        let omega = hz.iter().map(|f| 2.0 * std::f64::consts::PI * f).collect();
        let mut sim = Sim {
            omega,
            pushes,
            bows,
            probes,
            probe: Wv(vec![Seg {
                start: 0,
                w: Vec::with_capacity(part_modes),
            }]),
            probe_buf: Vec::with_capacity(PROBE),
            hand_push,
            hand_bow,
            terms: Vec::with_capacity(rank),
            small_m: vec![0.0; rank * rank],
            small_y: vec![0.0; rank],
            hand,
            part_modes,
            part_ranges,
            hush_left: 0,
            hush_step: 1.0,
            sample_rate,
            os,
            k,
            solver,
            regulate: st.regulate,
            c1,
            c2,
            d,
            mass,
            stiff,
            q: vec![0.0; n],
            q1: vec![0.0; n],
            qn: vec![0.0; n],
            g: vec![0.0; n],
            h: vec![0.0; n],
            lin_y: vec![0.0; nl],
            linear,
            lin_prev,
            lin_m,
            nonlinear,
            psi: 0.0,
            v_prev: 0.0,
            gauge: st.gauge.unwrap_or(GAUGE),
            adapt: st.gauge.is_none(),
            exciters,
            exciter_names: scene.exciters.iter().map(|e| e.name.clone()).collect(),
            listeners,
            gain: db(st.gain_db),
            blown: false,
            info,
        };
        for b in 0..sim.bows.len() {
            sim.place_bow(b);
        }
        sim.reset();
        Ok((sim, Rig { bakes, start, keep }))
    }

    /// Carry on from where `old` was, when this simulation has the same
    /// modes (an edit that moved a listener or changed a joint, say). True
    /// when the state was taken over.
    pub fn adopt(&mut self, old: &Sim) -> bool {
        if self.k != old.k {
            return false;
        }
        // Part by part, by name: a part whose modes are unchanged keeps
        // ringing; an edited or new one starts from rest. Mallet masses come
        // after the parts and are not carried over.
        let mut any = false;
        for (name, start, len) in &self.part_ranges {
            let Some((_, from, _)) = old
                .part_ranges
                .iter()
                .find(|(n, _, l)| n == name && l == len)
            else {
                continue;
            };
            let (a, b) = (*start..*start + *len, *from..*from + *len);
            if self.c1[a.clone()] != old.c1[b.clone()]
                || self.c2[a.clone()] != old.c2[b.clone()]
                || self.mass[a.clone()] != old.mass[b.clone()]
            {
                continue;
            }
            self.q[a.clone()].copy_from_slice(&old.q[b.clone()]);
            self.q1[a].copy_from_slice(&old.q1[b]);
            any = true;
        }
        self.settle();
        any
    }

    /// Bring the joints' bookkeeping in line with the state as it now is.
    fn settle(&mut self) {
        for e in &self.exciters {
            if let Exciter::Mallet { mode, .. } = e {
                self.q[*mode] = PARKED;
                self.q1[*mode] = PARKED;
            }
        }
        let mut v = 0.0;
        for j in &mut self.nonlinear {
            let eta = j.w.dot(&self.q) + j.offset;
            j.eta_prev = j.w.dot(&self.q1) + j.offset;
            v += j.law.eval(eta).0;
        }
        self.psi = (2.0 * v + self.gauge).sqrt();
        self.v_prev = v;
    }

    /// Stop everything: lift the bows, drop the plucks and fade all motion
    /// away over about 30 ms.
    pub fn hush(&mut self) {
        for p in &mut self.pushes {
            p.on = false;
        }
        for b in &mut self.bows {
            (b.speed_to, b.pressure_to, b.pressure) = (0.0, 0.0, 0.0);
        }
        self.hush_left = ((0.03 / self.k) as usize).max(1);
        // -80 dB by the end of the fade.
        self.hush_step = (1e-4f64).powf(1.0 / self.hush_left as f64);
    }

    /// How many modes are simulated.
    pub fn modes(&self) -> usize {
        self.q.len()
    }

    /// How far mode `i` is swinging: its amplitude, metres. For a free mass
    /// (no frequency) this is its displacement, signed.
    #[inline]
    pub fn amplitude(&self, i: usize) -> f64 {
        let w = self.omega[i];
        if w > 0.0 {
            let v = (self.q[i] - self.q1[i]) / (self.k * w);
            (self.q[i] * self.q[i] + v * v).sqrt()
        } else {
            self.q[i]
        }
    }

    /// Hit a part with the roaming mallet (needs `Opts::hand`). `start` and
    /// `weights` come from `Rig`; `speed` in m/s, `mass` in kg, `stiffness`
    /// in N/m^1.5. Does not allocate.
    pub fn strike(&mut self, start: usize, weights: &[f64], speed: f64, mass: f64, stiffness: f64) {
        let Some(Exciter::Mallet { joint, mode }) = self.hand.and_then(|h| self.exciters.get(h))
        else {
            return;
        };
        let (joint, mode) = (*joint, *mode);
        if start + weights.len() > self.part_modes || !(mass > 0.0 && stiffness > 0.0) {
            return;
        }
        let j = &mut self.nonlinear[joint];
        let Some(seg) = j.w.0.get_mut(1) else {
            return;
        };
        if weights.len() > seg.w.capacity() {
            return;
        }
        seg.start = start;
        seg.w.clear();
        seg.w.extend(weights.iter().map(|w| -w));
        j.law = Law::Contact {
            k: stiffness,
            alpha: 1.5,
        };
        self.mass[mode] = mass;
        self.d[mode] = self.k * self.k / mass;
        Self::listen_at(&mut self.probe, start, weights);
        let mobility = j.w.mobility(&self.mass);
        j.m_eff = 1.0 / mobility;
        self.launch(joint, mode, speed);
    }

    /// Put a mallet on its surface, moving in at `speed`.
    fn launch(&mut self, joint: usize, mode: usize, speed: f64) {
        let j = &mut self.nonlinear[joint];
        self.q[mode] = 0.0;
        self.q1[mode] = 0.0;
        self.q[mode] = -(j.w.dot(&self.q) + j.offset);
        self.q1[mode] = -(j.w.dot(&self.q1) + j.offset) + speed * self.k;
        j.eta_prev = j.w.dot(&self.q1) + j.offset;
    }

    /// Back to rest.
    pub fn reset(&mut self) {
        self.q.iter_mut().for_each(|x| *x = 0.0);
        self.q1.iter_mut().for_each(|x| *x = 0.0);
        for e in &self.exciters {
            if let Exciter::Mallet { mode, .. } = e {
                self.q[*mode] = PARKED;
                self.q1[*mode] = PARKED;
            }
        }
        let mut v = 0.0;
        for j in &mut self.nonlinear {
            let eta = j.w.dot(&self.q) + j.offset;
            j.eta_prev = eta;
            v += j.law.eval(eta).0;
        }
        self.psi = (2.0 * v + self.gauge).sqrt();
        self.v_prev = v;
        for p in &mut self.pushes {
            p.on = false;
        }
        for b in &mut self.bows {
            (b.speed, b.pressure, b.speed_to, b.pressure_to) = (0.0, 0.0, 0.0, 0.0);
        }
        for l in &mut self.listeners {
            for t in &mut l.taps {
                t.line.iter_mut().for_each(|x| *x = 0.0);
                t.acc = 0.0;
            }
        }
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// True once the state went non-finite and was reset.
    pub fn blown(&self) -> bool {
        self.blown
    }

    pub fn exciter(&self, name: &str) -> Option<usize> {
        self.exciter_names.iter().position(|n| n == name)
    }

    /// Fire an exciter. Mallet: `strength` is its speed, m/s. Impulse: N.s.
    pub fn excite(&mut self, exciter: usize, strength: f64) {
        let k = self.k;
        if let Some(Seg { start, w }) = self.probes.get(exciter).and_then(|p| p.0.first()) {
            Self::listen_at(&mut self.probe, *start, w);
        }
        match self.exciters.get(exciter) {
            Some(Exciter::Mallet { joint, mode }) => {
                let (joint, mode) = (*joint, *mode);
                self.launch(joint, mode, strength);
            }
            Some(Exciter::Impulse { w }) => {
                // A velocity step of impulse x weight / mass, arriving
                // against the port's direction.
                for seg in &w.0 {
                    for (i, w) in seg.w.iter().enumerate() {
                        let m = seg.start + i;
                        self.q1[m] += strength * w / self.mass[m] * k;
                    }
                }
            }
            Some(Exciter::Pluck { push, time }) => {
                let (push, time) = (*push, *time);
                self.start_push(push, strength, time);
            }
            Some(Exciter::Bow { bow, pressure }) => {
                let b = &mut self.bows[*bow];
                b.speed_to = strength;
                b.pressure_to = if strength == 0.0 { 0.0 } else { *pressure };
            }
            None => {}
        }
    }

    /// Point the probe at a spot: `weights` over the modes from `start`.
    fn listen_at(probe: &mut Wv, start: usize, weights: &[f64]) {
        if let Some(seg) = probe.0.get_mut(0)
            && weights.len() <= seg.w.capacity()
        {
            seg.start = start;
            seg.w.clear();
            seg.w.extend_from_slice(weights);
        }
    }

    /// The surface speed at the spot last played, one sample per frame of
    /// the last `process` call (up to 8192): a clean signal of one part, for
    /// a tuner.
    pub fn probe(&self) -> &[f32] {
        &self.probe_buf
    }

    /// Pull the port of push `i` aside by `pull` metres over `time` seconds,
    /// then let go.
    fn start_push(&mut self, i: usize, pull: f64, time: f64) {
        let rate = 1.0 / self.k;
        let p = &mut self.pushes[i];
        // How far a steady newton moves the port: sum of w^2 / stiffness.
        let mut give = 0.0;
        for seg in &p.w.0 {
            for (w, s) in seg.w.iter().zip(&self.stiff[seg.start..]) {
                if *s > 0.0 {
                    give += w * w / s;
                }
            }
        }
        if !(give > 0.0 && give.is_finite() && pull.is_finite()) {
            return;
        }
        p.force = pull / give;
        p.t = 0;
        p.rise = ((time.clamp(0.001, 2.0) * rate) as usize).max(2);
        p.fall = ((0.0004 * rate) as usize).max(2);
        p.on = true;
    }

    /// Recompute what a bow needs after its weights changed.
    fn place_bow(&mut self, i: usize) {
        let b = &mut self.bows[i];
        b.h.iter_mut().for_each(|x| *x = 0.0);
        for seg in &b.w.0 {
            let r = seg.start..seg.start + seg.w.len();
            for ((h, d), w) in b.h[r.clone()].iter_mut().zip(&self.d[r]).zip(&seg.w) {
                *h = d * w;
            }
        }
        Self::solve_linear(
            &self.linear,
            &self.lin_m,
            &mut self.lin_y,
            &self.d,
            &mut b.h,
        );
    }

    /// Pluck a part with the roaming finger (needs `Opts::hand`): pull the
    /// point `pull` metres aside over `time` seconds and let go. `start` and
    /// `weights` come from `Rig`. Does not allocate.
    pub fn pluck(&mut self, start: usize, weights: &[f64], pull: f64, time: f64) {
        let Some(i) = self.hand_push else { return };
        let Some(seg) = self.pushes[i].w.0.get_mut(0) else {
            return;
        };
        if start + weights.len() > self.part_modes || weights.len() > seg.w.capacity() {
            return;
        }
        seg.start = start;
        seg.w.clear();
        seg.w.extend_from_slice(weights);
        Self::listen_at(&mut self.probe, start, weights);
        self.start_push(i, pull, time);
    }

    /// Bow a part with the roaming bow (needs `Opts::hand`), or change the
    /// bow already there: `speed` in m/s, `pressure` (the most friction it
    /// can apply) in N. Keeps going until `bow_stop`. Does not allocate.
    pub fn bow(&mut self, start: usize, weights: &[f64], speed: f64, pressure: f64) {
        let Some(i) = self.hand_bow else { return };
        let Some(seg) = self.bows[i].w.0.get_mut(0) else {
            return;
        };
        if start + weights.len() > self.part_modes || weights.len() > seg.w.capacity() {
            return;
        }
        if !(speed.is_finite() && pressure.is_finite() && pressure >= 0.0) {
            return;
        }
        let same_part = seg.start == start && seg.w.len() == weights.len();
        let moved = !same_part || seg.w != weights;
        if moved {
            seg.start = start;
            seg.w.clear();
            seg.w.extend_from_slice(weights);
            self.place_bow(i);
            if !same_part {
                // A bow lifted off one part lands on the next from rest;
                // sliding along the same part keeps it pressed.
                self.bows[i].pressure = 0.0;
            }
        }
        self.bows[i].speed_to = speed;
        self.bows[i].pressure_to = pressure;
        Self::listen_at(&mut self.probe, start, weights);
    }

    /// Lift the roaming bow.
    pub fn bow_stop(&mut self) {
        if let Some(i) = self.hand_bow {
            self.bows[i].speed_to = 0.0;
            self.bows[i].pressure_to = 0.0;
        }
    }

    /// x <- (I + D W K W^T)^-1 x, the implicit linear joints.
    #[inline]
    fn solve_linear(linear: &[Joint], lin_m: &[f64], y: &mut [f64], d: &[f64], x: &mut [f64]) {
        let n = linear.len();
        if n == 0 {
            return;
        }
        for (y, j) in y.iter_mut().zip(linear) {
            *y = j.w.dot(x);
        }
        for (i, j) in linear.iter().enumerate() {
            let z: f64 = lin_m[i * n..(i + 1) * n]
                .iter()
                .zip(y.iter())
                .map(|(m, y)| m * y)
                .sum();
            j.w.push(x, d, -z);
        }
    }

    #[inline]
    fn step(&mut self) {
        let k = self.k;
        for (((qn, q), q1), (c1, c2)) in self
            .qn
            .iter_mut()
            .zip(&self.q)
            .zip(&self.q1)
            .zip(self.c1.iter().zip(&self.c2))
        {
            *qn = c1 * q - c2 * q1;
        }

        // Nonlinear joints: energy, its derivative, and damping.
        let mut v = 0.0;
        let mut active = false;
        for j in &mut self.nonlinear {
            let eta = j.w.dot(&self.q) + j.offset;
            let (vj, dv) = j.law.eval(eta);
            let rate = (eta - j.eta_prev) / k;
            j.eta_prev = eta;
            // An explicit damper is stable only below m / k.
            let cap = 0.5 * j.m_eff / k;
            let damp = match j.law {
                // A spring damps only while it is engaged (past its free play).
                Law::Spring { gap, .. } if gap == 0.0 || dv != 0.0 => -j.damping.min(cap) * rate,
                Law::Spring { .. } => 0.0,
                // Hunt-Crossley: the contact force scaled by the closing
                // speed, never pulling the bodies together.
                Law::Contact { .. } => {
                    let f = -dv;
                    (-(f * j.damping).min(cap) * rate).max(-f)
                }
            };
            v += vj;
            j.dv = dv;
            let force = match self.solver {
                Solver::Sav => damp,
                Solver::Explicit => damp - dv,
            };
            if force != 0.0 {
                j.w.push(&mut self.qn, &self.d, force);
            }
            active |= dv != 0.0;
        }

        if self.solver == Solver::Sav {
            // Linear joints: the part of the implicit force that depends on
            // the previous step.
            for (j, kp) in self.linear.iter().zip(&self.lin_prev) {
                let y = j.w.dot(&self.q1);
                if y != 0.0 {
                    j.w.push(&mut self.qn, &self.d, -kp * y);
                }
            }
            if active && self.v_prev == 0.0 && self.adapt {
                // Nothing nonlinear was engaged a step ago, so psi holds only
                // the gauge, and the gauge is free to change. Set it to the
                // energy now in the system: no joint can store more than
                // that, which keeps the square root smooth (second-order
                // accuracy), and an instability could at worst release half
                // of it, never more than is already there.
                //
                // psi below the gauge means the last engagement handed the
                // modes a little more energy than it took. Take that back
                // out by scaling the whole state down a hair, so the total
                // never rises.
                let mut owed = 0.5 * (self.gauge - self.psi * self.psi).max(0.0);
                let mut have = self.moving_energy();
                if owed > 0.0 && have > 0.0 {
                    let take = owed.min(0.5 * have);
                    let scale = (1.0 - take / have).sqrt();
                    self.q.iter_mut().for_each(|x| *x *= scale);
                    self.q1.iter_mut().for_each(|x| *x *= scale);
                    self.qn.iter_mut().for_each(|x| *x *= scale);
                    owed -= take;
                    have -= take;
                }
                let level = have.max(GAUGE);
                self.gauge = level + 2.0 * owed;
                self.psi = level.sqrt();
            }
            if self.regulate {
                // psi stands for sqrt(2 V + gauge). Rounding lets it wander;
                // pulling it back down can only remove energy.
                let top = (2.0 * v.max(self.v_prev) + self.gauge).sqrt();
                self.psi = self.psi.clamp(0.0, top);
            }
            self.v_prev = v;
        }

        // Plucks: a force that swells, then lets go.
        for p in &mut self.pushes {
            if !p.on {
                continue;
            }
            let f = if p.t < p.rise {
                0.5 - 0.5 * (std::f64::consts::PI * p.t as f64 / p.rise as f64).cos()
            } else {
                0.5 + 0.5 * (std::f64::consts::PI * (p.t - p.rise) as f64 / p.fall as f64).cos()
            };
            p.w.push(&mut self.qn, &self.d, p.force * f);
            p.t += 1;
            p.on = p.t < p.rise + p.fall;
        }

        // Bows: this step's damper coefficient, and the part of the damper
        // force that does not depend on the new state.
        self.terms.clear();
        let glide = 1.0 - (-k / 0.004).exp();
        for (i, b) in self.bows.iter_mut().enumerate() {
            b.pressure += glide * (b.pressure_to - b.pressure);
            b.speed += glide * (b.speed_to - b.speed);
            b.c = 0.0;
            if b.pressure < 1e-7 {
                continue;
            }
            let rel = b.w.dot_diff(&self.q, &self.q1) / k - b.speed;
            let grip = (2.0 * BOW_SHARPNESS).sqrt() * (0.5 - BOW_SHARPNESS * rel * rel).exp();
            b.c = b.pressure * grip;
            if b.c > 1e-12 {
                let y1 = b.w.dot(&self.q1);
                b.w.push(&mut self.qn, &self.d, b.c * (y1 / (2.0 * k) + b.speed));
                self.terms.push(i);
            }
        }

        let mut g_q1 = 0.0;
        let sav = self.solver == Solver::Sav && active;
        if sav {
            let root = (2.0 * v + self.gauge).sqrt();
            self.g.iter_mut().for_each(|x| *x = 0.0);
            for j in &self.nonlinear {
                if j.dv != 0.0 {
                    j.w.add(&mut self.g, j.dv / root);
                }
            }
            for (g, q1) in self.g.iter().zip(&self.q1) {
                g_q1 += g * q1;
            }
            let lead = self.psi - 0.25 * g_q1;
            for (((qn, h), g), d) in self
                .qn
                .iter_mut()
                .zip(&mut self.h)
                .zip(&self.g)
                .zip(&self.d)
            {
                *h = d * g;
                *qn -= *h * lead;
            }
            Self::solve_linear(
                &self.linear,
                &self.lin_m,
                &mut self.lin_y,
                &self.d,
                &mut self.h,
            );
            self.terms.push(usize::MAX);
        }
        Self::solve_linear(
            &self.linear,
            &self.lin_m,
            &mut self.lin_y,
            &self.d,
            &mut self.qn,
        );

        // What is left is the linear system plus one rank-one term per bow
        // and one for the auxiliary variable: each is alpha (D a) a^T. With
        // p the solution without them and h = A^-1 D a, the answer is
        // p - sum y h, where (diag(1 / alpha) + a^T h) y = a^T p.
        let r = self.terms.len();
        if r > 0 {
            let dot = |g: &[f64], bows: &[Bow], term: usize, x: &[f64]| -> f64 {
                if term == usize::MAX {
                    g.iter().zip(x).map(|(a, b)| a * b).sum()
                } else {
                    bows[term].w.dot(x)
                }
            };
            for i in 0..r {
                let ti = self.terms[i];
                self.small_y[i] = dot(&self.g, &self.bows, ti, &self.qn);
                for j in 0..r {
                    let tj = self.terms[j];
                    let hj: &[f64] = if tj == usize::MAX {
                        &self.h
                    } else {
                        &self.bows[tj].h
                    };
                    self.small_m[i * r + j] = dot(&self.g, &self.bows, ti, hj);
                }
                self.small_m[i * r + i] += if ti == usize::MAX {
                    4.0
                } else {
                    2.0 * k / self.bows[ti].c
                };
            }
            solve_small(&mut self.small_m, &mut self.small_y, r);
            for j in 0..r {
                let tj = self.terms[j];
                let y = self.small_y[j];
                let hj: &[f64] = if tj == usize::MAX {
                    &self.h
                } else {
                    &self.bows[tj].h
                };
                for (qn, h) in self.qn.iter_mut().zip(hj) {
                    *qn -= y * h;
                }
            }
        }
        if sav {
            let g_qn: f64 = self.g.iter().zip(&self.qn).map(|(g, q)| g * q).sum();
            self.psi += 0.5 * (g_qn - g_q1);
        }

        std::mem::swap(&mut self.q1, &mut self.q);
        std::mem::swap(&mut self.q, &mut self.qn);

        if self.hush_left > 0 {
            let n = self.part_modes;
            let f = self.hush_step;
            self.q[..n].iter_mut().for_each(|x| *x *= f);
            self.q1[..n].iter_mut().for_each(|x| *x *= f);
            self.hush_left -= 1;
            self.settle();
        }

        // A mallet that has bounced clear goes back to waiting.
        for e in &self.exciters {
            if let Exciter::Mallet { joint, mode } = e {
                let j = &mut self.nonlinear[*joint];
                if self.q[*mode] < 0.5 * PARKED && self.q[*mode] > self.q1[*mode] {
                    let eta = j.w.dot(&self.q) + j.offset;
                    if eta > 1e-3 {
                        self.q[*mode] = PARKED;
                        self.q1[*mode] = PARKED;
                        j.eta_prev = PARKED;
                    }
                }
            }
        }
    }

    /// Render the next block of interleaved stereo samples.
    pub fn process(&mut self, out: &mut [f32]) {
        let norm = 1.0 / (self.os as f64 * self.k);
        self.probe_buf.clear();
        for frame in out.as_chunks_mut::<2>().0 {
            let mut heard = 0.0;
            for _ in 0..self.os {
                self.step();
                heard += self.probe.dot_diff(&self.q, &self.q1);
                for l in &mut self.listeners {
                    for t in &mut l.taps {
                        t.acc += t.w.dot_diff(&self.q, &self.q1);
                    }
                }
            }
            if self.probe_buf.len() < PROBE {
                self.probe_buf.push((heard * norm) as f32);
            }
            let (mut left, mut right) = (0.0, 0.0);
            for l in &mut self.listeners {
                let mut s = 0.0;
                for t in &mut l.taps {
                    t.line[t.pos] = t.acc * norm * t.gain;
                    t.acc = 0.0;
                    t.pos = (t.pos + 1) % t.line.len();
                    s += t.line[t.pos];
                }
                left += s * l.left;
                right += s * l.right;
            }
            left *= self.gain;
            right *= self.gain;
            if !(left.is_finite() && right.is_finite()) {
                self.blown = true;
                self.reset();
                left = 0.0;
                right = 0.0;
            }
            frame[0] = left as f32;
            frame[1] = right as f32;
        }
    }

    /// Energy in the modes and the linear joints, joules.
    fn moving_energy(&self) -> f64 {
        let k = self.k;
        let mut e = 0.0;
        for i in 0..self.q.len() {
            let v = (self.q[i] - self.q1[i]) / k;
            e += 0.5 * self.mass[i] * v * v + 0.5 * self.stiff[i] * self.q[i] * self.q1[i];
        }
        for j in &self.linear {
            if let Law::Spring { k: ks, .. } = j.law {
                let (a, b) = (j.w.dot(&self.q), j.w.dot(&self.q1));
                e += 0.25 * ks * (a * a + b * b);
            }
        }
        e
    }

    /// The discrete energy the scheme conserves, joules. Only joint and
    /// modal damping may lower it; nothing may raise it.
    pub fn energy(&self) -> f64 {
        let mut e = self.moving_energy();
        match self.solver {
            Solver::Sav => e += 0.5 * (self.psi * self.psi - self.gauge),
            Solver::Explicit => {
                for j in &self.nonlinear {
                    e += j.law.eval(j.w.dot(&self.q) + j.offset).0;
                }
            }
        }
        e
    }
}
