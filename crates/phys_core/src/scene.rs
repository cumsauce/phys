//! The scene document: parts, joints, exciters, listeners and a score.
//!
//! Everything is plain serde JSON with unknown fields rejected, so a typo is
//! an error that names the field instead of a silently ignored setting.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::material::MaterialSpec;
use crate::math::V3;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub joints: Vec<Joint>,
    #[serde(default)]
    pub exciters: Vec<Exciter>,
    #[serde(default)]
    pub listeners: Vec<Listener>,
    /// Timed excitations, used by offline rendering.
    #[serde(default)]
    pub score: Vec<Event>,
}

/// How joints are stepped in time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Solver {
    /// Energy quadratisation (scalar auxiliary variable): explicit, and the
    /// stored energy can never grow, however stiff the joints.
    #[default]
    Sav,
    /// Plain explicit forces with automatic oversampling (sounder's rig
    /// approach). Stable only while the oversampling keeps up.
    Explicit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Highest mode frequency kept, Hz. Default: 16 kHz or 0.45 x sample rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_hz: Option<f64>,
    #[serde(default)]
    pub solver: Solver,
    /// Simulation steps per output sample, 1..64. Default: 1 for `sav`,
    /// chosen from joint stiffness for `explicit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oversample: Option<u32>,
    /// Output gain in dB, applied to every listener.
    #[serde(default)]
    pub gain_db: f64,
    /// `sav` only: keep the auxiliary variable from drifting above the true
    /// stored energy (it is only ever pulled down, so this cannot add energy).
    #[serde(default = "yes")]
    pub regulate: bool,
    /// `sav` only: a fixed gauge, joules: the energy offset added to the
    /// stored joint energy before its square root is taken. Larger is more
    /// accurate for contacts, and twice the most an instability could ever
    /// release. Left out, the gauge follows the energy in the scene, reset
    /// each time a contact engages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gauge: Option<f64>,
}

fn yes() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            max_hz: None,
            solver: Solver::default(),
            oversample: None,
            gain_db: 0.0,
            regulate: true,
            gauge: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Part {
    pub name: String,
    pub shape: Shape,
    #[serde(default)]
    pub material: MaterialSpec,
    /// Centre of the part, metres.
    #[serde(default)]
    pub pos: V3,
    /// Euler angles in degrees, applied about X, then Y, then Z.
    #[serde(default)]
    pub rot: V3,
    /// Loss factor; overrides the material's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss: Option<f64>,
    /// Frequency-independent decay rate, 1/s. Default 1 (a 6.9 s ceiling on T60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damping: Option<f64>,
    /// Most modes kept. Default 96.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<usize>,
}

/// How the ends of a bar are held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Ends {
    /// Free at both ends (xylophone key, chime, dropped rod).
    #[default]
    Free,
    /// Simply supported at both ends.
    Supported,
}

/// The primitive a part is. Local axes: a string or bar runs along X; a
/// plate or membrane lies in the XZ plane; Y is the normal they vibrate along.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Shape {
    /// A stiff string fixed at both ends. Give `f0` or `tension`.
    /// Port `at`: [u], 0..1 along the length.
    String {
        length: f64,
        #[serde(default = "default_string_radius")]
        radius: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        f0: Option<f64>,
        /// Newtons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tension: Option<f64>,
    },
    /// A bar bending across its thickness. Port `at`: [u], 0..1 along the length.
    Bar {
        length: f64,
        width: f64,
        thickness: f64,
        #[serde(default)]
        ends: Ends,
    },
    /// A simply supported rectangular plate, `size` = [x, z] in metres.
    /// Port `at`: [u, v], each 0..1.
    Plate { size: [f64; 2], thickness: f64 },
    /// A circular membrane fixed at the rim. Give `f0` or `tension` (N/m).
    /// Port `at`: [u, v] over the bounding square, each 0..1; [0.5, 0.5] is the centre.
    Membrane {
        radius: f64,
        #[serde(default = "default_membrane_thickness")]
        thickness: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        f0: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tension: Option<f64>,
    },
    /// A point mass, free to move in all three directions. Port `at`: [].
    Mass { mass: f64 },
}

fn default_string_radius() -> f64 {
    0.0005
}

fn default_membrane_thickness() -> f64 {
    0.00025
}

/// A point on a part, in the part's own normalised coordinates, so it
/// follows the part when its size or pose changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub part: String,
    #[serde(default)]
    pub at: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum JointKind {
    /// A spring, optionally with a cubic term and free play (`gap`).
    Spring,
    /// One-sided Hertz contact: `a` rests `gap` above `b` along `dir`.
    Contact,
}

/// A connection between two ports, or between a port and the ground.
/// It acts along one direction: the elongation is (a - b) . dir.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Joint {
    pub name: String,
    pub kind: JointKind,
    pub a: Port,
    /// Left out: the ground (a fixed reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<Port>,
    /// Spring: N/m, default 1e4. Contact: N/m^exponent, default 1e8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stiffness: Option<f64>,
    /// Spring only: cubic stiffness, N/m3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cubic: Option<f64>,
    /// Contact only: force law exponent. Default 1.5 (Hertz).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exponent: Option<f64>,
    /// Spring: N.s/m. Contact: Hunt-Crossley factor, s/m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damping: Option<f64>,
    /// Spring: free play either side of rest, m (a rattle).
    /// Contact: separation at rest, m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap: Option<f64>,
    /// World direction the joint acts along. Default: the normal of `a`'s part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<V3>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExciterKind {
    /// A mass thrown at the port; `strength` is its speed in m/s.
    Mallet,
    /// An ideal instantaneous hit; `strength` is the impulse in N.s.
    Impulse,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Exciter {
    pub name: String,
    pub kind: ExciterKind,
    pub on: Port,
    /// World direction the hit arrives from. Default: the part's normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<V3>,
    /// Mallet mass, kg. Default 0.02.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mass: Option<f64>,
    /// Mallet contact stiffness, N/m^1.5. Default 1e8 (hard wood); felt is
    /// nearer 1e6, steel on steel 1e10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stiffness: Option<f64>,
    /// Mallet Hunt-Crossley damping, s/m. Default 0.3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damping: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ListenerKind {
    /// A microphone in the air at `pos`.
    Mic,
    /// A contact pickup on a port: the surface velocity there.
    Pickup,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Listener {
    pub name: String,
    pub kind: ListenerKind,
    /// Mic position, metres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<V3>,
    /// Pickup port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Port>,
    /// Pickup direction. Default: the part's normal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<V3>,
    #[serde(default)]
    pub gain_db: f64,
    /// -1 left .. 1 right.
    #[serde(default)]
    pub pan: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Seconds from the start.
    pub t: f64,
    pub exciter: String,
    /// Mallet: m/s. Impulse: N.s.
    #[serde(default = "one")]
    pub strength: f64,
}

fn one() -> f64 {
    1.0
}

fn finite(what: &str, v: &[f64]) -> Result<(), String> {
    if v.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(format!("{what}: {v:?}: give finite numbers"))
    }
}

fn range(what: &str, v: Option<f64>, lo: f64, hi: f64) -> Result<(), String> {
    match v {
        Some(x) if !(x.is_finite() && x >= lo && x <= hi) => {
            Err(format!("{what}: {x}: give {lo}..{hi}"))
        }
        _ => Ok(()),
    }
}

fn unique<'a>(kind: &str, names: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for n in names {
        if n.is_empty() {
            return Err(format!("{kind}: a name is empty"));
        }
        if !seen.insert(n) {
            return Err(format!("{kind} `{n}`: the name is used twice"));
        }
    }
    Ok(())
}

impl Shape {
    /// How many coordinates a port on this shape takes.
    pub fn at_len(&self) -> usize {
        match self {
            Shape::String { .. } | Shape::Bar { .. } => 1,
            Shape::Plate { .. } | Shape::Membrane { .. } => 2,
            Shape::Mass { .. } => 0,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Shape::String { .. } => "string",
            Shape::Bar { .. } => "bar",
            Shape::Plate { .. } => "plate",
            Shape::Membrane { .. } => "membrane",
            Shape::Mass { .. } => "mass",
        }
    }
}

impl Scene {
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }

    fn check_port(&self, what: &str, port: &Port) -> Result<(), String> {
        let part = self.part(&port.part).ok_or_else(|| {
            let known: Vec<_> = self.parts.iter().map(|p| p.name.as_str()).collect();
            format!(
                "{what}: no part named `{}` (parts: {})",
                port.part,
                known.join(", ")
            )
        })?;
        let want = part.shape.at_len();
        if port.at.len() != want {
            return Err(format!(
                "{what}: `at` has {} numbers, a {} takes {want}",
                port.at.len(),
                part.shape.kind()
            ));
        }
        if port.at.iter().any(|x| !(0.0..=1.0).contains(x)) {
            return Err(format!("{what}: at {:?}: give numbers in 0..1", port.at));
        }
        if let Shape::Membrane { .. } = part.shape {
            let (x, z) = (2.0 * port.at[0] - 1.0, 2.0 * port.at[1] - 1.0);
            if x * x + z * z > 1.0 {
                return Err(format!(
                    "{what}: at {:?} is outside the membrane's rim ([0.5, 0.5] is the centre)",
                    port.at
                ));
            }
        }
        Ok(())
    }

    /// Everything that can be checked without baking the parts.
    pub fn validate(&self) -> Result<(), String> {
        unique("part", self.parts.iter().map(|p| p.name.as_str()))?;
        unique("joint", self.joints.iter().map(|p| p.name.as_str()))?;
        unique("exciter", self.exciters.iter().map(|p| p.name.as_str()))?;
        unique("listener", self.listeners.iter().map(|p| p.name.as_str()))?;
        range("settings.max_hz", self.settings.max_hz, 50.0, 96_000.0)?;
        range(
            "settings.oversample",
            self.settings.oversample.map(f64::from),
            1.0,
            64.0,
        )?;
        range(
            "settings.gain_db",
            Some(self.settings.gain_db),
            -120.0,
            120.0,
        )?;
        range("settings.gauge", self.settings.gauge, 1e-30, 1e6)?;
        for p in &self.parts {
            let n = &p.name;
            finite(&format!("part `{n}`: pos"), &p.pos)?;
            finite(&format!("part `{n}`: rot"), &p.rot)?;
            range(&format!("part `{n}`: loss"), p.loss, 0.0, 2.0)?;
            range(&format!("part `{n}`: damping"), p.damping, 0.0, 1e4)?;
            range(
                &format!("part `{n}`: modes"),
                p.modes.map(|m| m as f64),
                1.0,
                4096.0,
            )?;
            p.material.props().map_err(|e| format!("part `{n}`: {e}"))?;
            crate::parts::check(&p.shape).map_err(|e| format!("part `{n}`: {e}"))?;
        }
        for j in &self.joints {
            let n = &j.name;
            self.check_port(&format!("joint `{n}`: a"), &j.a)?;
            if let Some(b) = &j.b {
                self.check_port(&format!("joint `{n}`: b"), b)?;
                if *b == j.a {
                    return Err(format!("joint `{n}`: a and b are the same port"));
                }
            }
            range(&format!("joint `{n}`: stiffness"), j.stiffness, 1e-3, 1e13)?;
            range(&format!("joint `{n}`: damping"), j.damping, 0.0, 1e6)?;
            range(&format!("joint `{n}`: gap"), j.gap, -0.1, 10.0)?;
            match j.kind {
                JointKind::Spring => {
                    range(&format!("joint `{n}`: cubic"), j.cubic, 0.0, 1e20)?;
                    range(&format!("joint `{n}`: gap"), j.gap, 0.0, 10.0)?;
                    if j.exponent.is_some() {
                        return Err(format!("joint `{n}`: exponent is for a contact joint"));
                    }
                }
                JointKind::Contact => {
                    range(&format!("joint `{n}`: exponent"), j.exponent, 1.0, 4.0)?;
                    if j.cubic.is_some() {
                        return Err(format!("joint `{n}`: cubic is for a spring joint"));
                    }
                }
            }
            if let Some(d) = j.dir {
                crate::math::unit(d)
                    .ok_or_else(|| format!("joint `{n}`: dir {d:?}: give a direction"))?;
            }
        }
        for e in &self.exciters {
            let n = &e.name;
            self.check_port(&format!("exciter `{n}`: on"), &e.on)?;
            range(&format!("exciter `{n}`: mass"), e.mass, 1e-6, 1e4)?;
            range(&format!("exciter `{n}`: stiffness"), e.stiffness, 1.0, 1e13)?;
            range(&format!("exciter `{n}`: damping"), e.damping, 0.0, 100.0)?;
            if let Some(d) = e.dir {
                crate::math::unit(d)
                    .ok_or_else(|| format!("exciter `{n}`: dir {d:?}: give a direction"))?;
            }
        }
        for l in &self.listeners {
            let n = &l.name;
            range(
                &format!("listener `{n}`: gain_db"),
                Some(l.gain_db),
                -120.0,
                120.0,
            )?;
            range(&format!("listener `{n}`: pan"), Some(l.pan), -1.0, 1.0)?;
            match l.kind {
                ListenerKind::Mic => {
                    let pos = l
                        .pos
                        .ok_or_else(|| format!("listener `{n}`: a mic needs `pos`"))?;
                    finite(&format!("listener `{n}`: pos"), &pos)?;
                    if l.on.is_some() {
                        return Err(format!("listener `{n}`: `on` is for a pickup"));
                    }
                }
                ListenerKind::Pickup => {
                    let on =
                        l.on.as_ref()
                            .ok_or_else(|| format!("listener `{n}`: a pickup needs `on`"))?;
                    self.check_port(&format!("listener `{n}`: on"), on)?;
                    if l.pos.is_some() {
                        return Err(format!("listener `{n}`: `pos` is for a mic"));
                    }
                }
            }
            if let Some(d) = l.dir {
                crate::math::unit(d)
                    .ok_or_else(|| format!("listener `{n}`: dir {d:?}: give a direction"))?;
            }
        }
        for (i, ev) in self.score.iter().enumerate() {
            if !(ev.t.is_finite() && ev.t >= 0.0) {
                return Err(format!("score[{i}]: t {}: give seconds >= 0", ev.t));
            }
            if !ev.strength.is_finite() {
                return Err(format!(
                    "score[{i}]: strength {}: give a number",
                    ev.strength
                ));
            }
            if !self.exciters.iter().any(|e| e.name == ev.exciter) {
                return Err(format!("score[{i}]: no exciter named `{}`", ev.exciter));
            }
        }
        Ok(())
    }

    /// Parse and validate a scene.
    pub fn from_json(text: &str) -> Result<Scene, String> {
        let scene: Scene = serde_json::from_str(text).map_err(|e| format!("scene: {e}"))?;
        scene.validate()?;
        Ok(scene)
    }
}
