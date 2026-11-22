//! The MCP server: the same scene, audio engine and editor page as
//! `phys edit`, driven by tools over stdio.
//!
//! An agent cannot hear, so `listen` and `verify` render offline and answer
//! with numbers and a spectrogram.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use phys_core::analysis;
use phys_core::ops::Op;
use phys_core::render::Render;
use phys_core::scene::{Exciter, Joint, Listener, Part, Scene, Settings, Shape};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::{ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::edit::{self, App, HandKind, Played, Shared, Strike};

const INSTRUCTIONS: &str = r#"phys models sound from connected physical parts. Units are SI (metres, kilograms, seconds, newtons, hertz); Y is up.
- You cannot hear. `listen` renders offline and returns measurements (strongest partials, decay times, peak) and a spectrogram; `verify` runs PASS / FAIL checks. Say what you measured, never that something "sounds" a certain way: the human's ears decide.
- Call scene_summary first. Build with apply_ops: one atomic batch of items tagged by "type" (part, joint, exciter, listener, settings, score, name, delete, rename). Items are upserts by name: re-issue the whole item to change it; a field left out returns to its default. Any failing item rolls the whole batch back. describe_op gives an example and the schema of each.
- Parts: string (f0 or tension), bar (free or supported ends), plate (simply supported), membrane (f0 or tension), mass. A part's local axes: strings and bars run along X, plates and membranes lie in XZ, and all vibrate along Y. `pos` and `rot` (degrees, about X then Y then Z) place it.
- A port is a point on a part in its own normalised coordinates: {"part": "top", "at": [0.7, 0.45]}. `at` is [u] for a string or bar, [u, v] for a plate or membrane, [] for a mass; each 0..1. A port on a node line of a mode cannot excite or hear that mode (the middle of a string misses every even harmonic).
- Joints act along one direction (default: the normal of `a`'s part). spring: stiffness N/m, optional damping, cubic, and `gap` for free play (a rattle). contact: one-sided Hertz, stiffness N/m^1.5, `gap` the separation at rest. Leave `b` out to tie `a` to the ground. A spring near a string's end raises its pitch a little, as it would physically.
- To excite by hand: `strike` acts on any point, live (the human hears it and it joins the take), with `kind` strike (a mallet: speed, mass, stiffness), pluck (pull, in metres) or bow (speed, pressure, duration). `listen` / `verify` take the same actions as `strikes` with times `t`, or replay the take, or play the scene's score of named exciters (kinds mallet, impulse, pluck, bow).
- Bowing: speed about 0.1-0.5 m/s, pressure about 0.1-1 N, near one end of a string (at 0.08-0.15). Too little pressure for the speed gives a thin whistle on a high harmonic; too much gives a rough, pitchless scrape. `listen` shows which you have: a steady harmonic series on the string's pitch is a held note.
- A joint pulls a part's pitch away from its own: a string tied to a body sounds 15-20 cents sharp. After tying things together, call `tune` so each part sounds at the pitch it was given (it sets a small `tune` factor on the part); call it again after changing joints.
- A microphone hears every part through a rough radiation estimate; a pickup reads the surface velocity at one port exactly. A scene with no listener is silent.
- The human may be editing the same scene in the browser page; scene_summary shows the current state. `screenshot` shows what their viewport shows."#;

/// Arguments of a tool that takes none.
#[derive(Deserialize, JsonSchema)]
struct NoArgs {}

#[derive(Deserialize, JsonSchema)]
struct OpsArgs {
    /// The batch: items tagged by "type". See describe_op.
    ops: Vec<Op>,
}

#[derive(Deserialize, JsonSchema)]
struct DescribeArgs {
    /// part, joint, exciter, listener, settings, score, name or delete.
    /// Left out: a short example of each.
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct ExciteArgs {
    /// A named exciter of the scene.
    exciter: String,
    /// Mallet: speed in m/s (default 2). Impulse: N.s (default 0.01).
    /// Pluck: metres pulled (default 0.003). Bow: speed in m/s (default
    /// 0.2); 0 lifts it.
    #[serde(default)]
    strength: Option<f64>,
}

/// A strike at a time.
#[derive(Clone, Deserialize, JsonSchema)]
struct TimedStrike {
    /// Seconds from the start. Default 0.
    #[serde(default)]
    t: f64,
    #[serde(flatten)]
    strike: Strike,
}

#[derive(Deserialize, JsonSchema)]
struct ListenArgs {
    /// Strikes to play. Left out: the recorded take if there is one, else
    /// the scene's score.
    #[serde(default)]
    strikes: Option<Vec<TimedStrike>>,
    /// Seconds rendered after the last event. Default 3.
    #[serde(default)]
    tail: Option<f64>,
    /// Also write the take to this WAV path, normalised to -6 dBFS.
    #[serde(default)]
    wav: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Check {
    /// A partial within `within_pct` percent of `hz`, no more than `min_db`
    /// below the strongest.
    Partial {
        hz: f64,
        #[serde(default)]
        within_pct: Option<f64>,
        #[serde(default)]
        min_db: Option<f64>,
    },
    /// The time to fall `drop_db` (default 40) lies between `min` and `max` seconds.
    Decay {
        #[serde(default)]
        min: Option<f64>,
        #[serde(default)]
        max: Option<f64>,
        #[serde(default)]
        drop_db: Option<f64>,
    },
    /// Something reaches a listener.
    Audible,
    /// The simulation never went non-finite.
    Stable,
}

#[derive(Deserialize, JsonSchema)]
struct VerifyArgs {
    checks: Vec<Check>,
    #[serde(default)]
    strikes: Option<Vec<TimedStrike>>,
    #[serde(default)]
    tail: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
struct TuneArgs {
    /// Part names. Left out: every string, bar and membrane.
    #[serde(default)]
    parts: Option<Vec<String>>,
}

#[derive(Deserialize, JsonSchema)]
struct SaveArgs {
    /// Where to save. Left out: the scene's own file (phys-scene.json if it has none).
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct NewArgs {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct OpenArgs {
    /// A scene JSON file.
    path: String,
}

#[derive(Clone)]
pub(crate) struct Tools {
    app: Shared,
    /// The editor page's address, when it is being served.
    url: Option<String>,
}

fn ok(text: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![Content::text(text.into())]))
}

fn fail(text: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::error(vec![Content::text(text.into())]))
}

fn reply(r: Result<String, String>) -> Result<CallToolResult, ErrorData> {
    match r {
        Ok(t) => ok(t),
        Err(e) => fail(e),
    }
}

fn png(width: usize, height: usize, rgb: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width as u32, height as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(rgb).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// An enum's name in lower case, as the scene JSON spells it.
fn low(kind: &impl std::fmt::Debug) -> String {
    format!("{kind:?}").to_lowercase()
}

fn describe_part(p: &Part, modes: Option<&(String, usize, usize)>) -> String {
    let shape = match &p.shape {
        Shape::String {
            length,
            f0,
            tension,
            ..
        } => match (f0, tension) {
            (Some(f), _) => format!("string {length} m at {f} Hz"),
            (_, Some(t)) => format!("string {length} m at {t} N"),
            _ => format!("string {length} m"),
        },
        Shape::Bar {
            length,
            width,
            thickness,
            ends,
        } => {
            format!("bar {length} x {width} x {thickness} m, {} ends", low(ends))
        }
        Shape::Plate { size, thickness } => {
            format!("plate {} x {} x {thickness} m", size[0], size[1])
        }
        Shape::Membrane {
            radius,
            f0,
            tension,
            ..
        } => match (f0, tension) {
            (Some(f), _) => format!("membrane r {radius} m at {f} Hz"),
            (_, Some(t)) => format!("membrane r {radius} m at {t} N/m"),
            _ => format!("membrane r {radius} m"),
        },
        Shape::Mass { mass } => format!("mass {mass} kg"),
    };
    let material = match &p.material {
        phys_core::material::MaterialSpec::Name(n) => n.clone(),
        phys_core::material::MaterialSpec::Props(_) => "custom material".into(),
    };
    let count = modes.map_or(String::new(), |m| format!(", {} modes", m.1));
    let count = match p.tune {
        Some(t) => format!("{count}, tuned x{t}"),
        None => count,
    };
    format!("  {}: {shape}, {material}, at {:?}{count}", p.name, p.pos)
}

fn summary(app: &App, url: Option<&str>) -> String {
    let s = app.doc.scene();
    let modes: usize = app.info.parts.iter().map(|p| p.1).sum();
    let solver = low(&app.info.solver);
    let mut out = vec![format!(
        "scene `{}`: {modes} modes at {} Hz, {solver} solver at {}x",
        s.name, app.rate, app.info.oversample
    )];
    out.push(format!("parts ({}):", s.parts.len()));
    for (i, p) in s.parts.iter().enumerate() {
        out.push(describe_part(p, app.info.parts.get(i)));
    }
    let port = |p: &phys_core::scene::Port| format!("{}{:?}", p.part, p.at);
    out.push(format!("joints ({}):", s.joints.len()));
    for j in &s.joints {
        let b = j.b.as_ref().map_or("ground".to_string(), port);
        let mut extra = String::new();
        if let Some(k) = j.stiffness {
            extra += &format!(", stiffness {k:e}");
        }
        if let Some(g) = j.gap {
            extra += &format!(", gap {g}");
        }
        out.push(format!(
            "  {}: {} {} -> {b}{extra}",
            j.name,
            low(&j.kind),
            port(&j.a)
        ));
    }
    out.push(format!("exciters ({}):", s.exciters.len()));
    for e in &s.exciters {
        out.push(format!("  {}: {} on {}", e.name, low(&e.kind), port(&e.on)));
    }
    out.push(format!("listeners ({}):", s.listeners.len()));
    for l in &s.listeners {
        let place = match (&l.pos, &l.on) {
            (Some(p), _) => format!("at {p:?}"),
            (_, Some(on)) => format!("on {}", port(on)),
            _ => String::new(),
        };
        out.push(format!("  {}: {} {place}", l.name, low(&l.kind)));
    }
    out.push(format!(
        "score: {} events; take: {} recorded strikes",
        s.score.len(),
        app.take.len()
    ));
    if app.info.warnings.is_empty() {
        out.push("warnings: none".into());
    } else {
        for w in &app.info.warnings {
            out.push(format!("warning: {w}"));
        }
    }
    out.push(format!(
        "audio: {}; editor page: {}, {} open; file: {}; undo {}, redo {}",
        if app.audio.is_some() {
            "live"
        } else {
            "off (offline tools still work)"
        },
        url.unwrap_or("not served"),
        app.events.receiver_count(),
        app.path
            .as_ref()
            .map_or("unsaved".to_string(), |p| p.display().to_string()),
        if app.doc.can_undo() {
            "available"
        } else {
            "none"
        },
        if app.doc.can_redo() {
            "available"
        } else {
            "none"
        },
    ));
    out.join("\n")
}

/// What `listen` and `verify` play: given strikes, else the take, else the score.
fn take_of(
    app: &App,
    strikes: Option<Vec<TimedStrike>>,
    tail: Option<f64>,
) -> Result<(Render, String), String> {
    let tail = tail.unwrap_or(3.0);
    match strikes {
        Some(list) if !list.is_empty() => {
            let timed: Vec<(f64, Played)> = list
                .into_iter()
                .map(|s| (s.t, Played::Hand(s.strike)))
                .collect();
            let r = app.offline(&timed, false, tail)?;
            Ok((r, format!("{} given strikes", timed.len())))
        }
        _ if !app.take.is_empty() => {
            let r = app.offline(&app.take, false, tail)?;
            Ok((r, format!("the recorded take ({} strikes)", app.take.len())))
        }
        _ => {
            let r = app.offline(&[], true, tail)?;
            Ok((
                r,
                format!("the scene's score ({} events)", app.doc.scene().score.len()),
            ))
        }
    }
}

fn measure(r: &Render, source: &str) -> String {
    let mono = r.mono();
    let seconds = mono.len() as f64 / f64::from(r.sample_rate);
    let mut out = vec![format!(
        "played {source}: {seconds:.2} s at {} Hz{}",
        r.sample_rate,
        if r.blown {
            " — THE SIMULATION BLEW UP and was reset"
        } else {
            ""
        }
    )];
    if r.peak <= 0.0 {
        out.push("silent: nothing reached a listener (is there a listener, and does the strike land off a node line?)".into());
        return out.join("\n");
    }
    out.push(format!(
        "peak {:.1} dB (the simulation's own physical level; the editor and WAVs normalise it)",
        20.0 * f64::from(r.peak).log10()
    ));
    out.push(format!(
        "falls 20 dB in {:.2} s, 40 dB in {:.2} s, 60 dB in {:.2} s",
        analysis::decay_time(&mono, r.sample_rate, 20.0),
        analysis::decay_time(&mono, r.sample_rate, 40.0),
        analysis::decay_time(&mono, r.sample_rate, 60.0),
    ));
    let parts: Vec<String> = analysis::partials(&mono, r.sample_rate, 0.0, 12)
        .iter()
        .map(|(hz, db)| format!("{hz:.1} Hz ({db:.0} dB)"))
        .collect();
    out.push(format!("strongest partials: {}", parts.join(", ")));
    out.push("image: spectrogram, time left to right over the whole take, 40 Hz (bottom) to 20 kHz (top) on a log scale, 80 dB range; faint lines at 100 Hz, 1 kHz, 10 kHz and at each second".into());
    out.join("\n")
}

#[tool_router]
impl Tools {
    fn with<R>(&self, f: impl FnOnce(&mut App) -> Result<R, String>) -> Result<R, String> {
        let mut app = self
            .app
            .lock()
            .map_err(|_| "the editor's state is poisoned".to_string())?;
        f(&mut app)
    }

    /// Everything in the scene: parts (with shape, material, position and
    /// mode count), joints, exciters, listeners, the score and take, compile
    /// warnings, and whether audio and the editor page are live. Call it
    /// first, and after anything confusing.
    #[tool]
    async fn scene_summary(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| Ok(summary(app, self.url.as_deref()))))
    }

    /// Edit the scene: one atomic batch of items tagged by "type" (part,
    /// joint, exciter, listener, settings, score, name, delete). Items are
    /// upserts by name. If any item is invalid or the scene will not
    /// compile, nothing changes. One undo step per call. Returns the new
    /// summary.
    #[tool]
    async fn apply_ops(
        &self,
        Parameters(a): Parameters<OpsArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| {
            app.apply(a.ops)?;
            Ok(summary(app, self.url.as_deref()))
        }))
    }

    /// An example and the JSON schema of an op type; with no kind, a short
    /// example of each, plus the material names.
    #[tool]
    async fn describe_op(
        &self,
        Parameters(a): Parameters<DescribeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let examples = json!({
            "part": {"type": "part", "name": "top", "shape": {"kind": "plate", "size": [0.4, 0.3], "thickness": 0.003}, "material": "wood", "pos": [0, 0, 0], "rot": [0, 0, 0]},
            "joint": {"type": "joint", "name": "bridge", "kind": "spring", "a": {"part": "string", "at": [0.95]}, "b": {"part": "top", "at": [0.7, 0.45]}, "stiffness": 2e4},
            "exciter": {"type": "exciter", "name": "pick", "kind": "mallet", "on": {"part": "string", "at": [0.18]}, "mass": 0.002, "stiffness": 1e7},
            "exciter (pluck)": {"type": "exciter", "name": "finger", "kind": "pluck", "on": {"part": "string", "at": [0.2]}, "time": 0.02},
            "exciter (bow)": {"type": "exciter", "name": "bow", "kind": "bow", "on": {"part": "string", "at": [0.12]}, "pressure": 0.3},
            "listener": {"type": "listener", "name": "mic", "kind": "mic", "pos": [0.1, 0.4, 0.3]},
            "settings": {"type": "settings", "solver": "sav", "oversample": 2},
            "score": {"type": "score", "events": [{"t": 0.05, "exciter": "pick", "strength": 1.5}, {"t": 1.0, "exciter": "bow", "strength": 0.2}, {"t": 2.5, "exciter": "bow", "strength": 0}]},
            "name": {"type": "name", "name": "guitar"},
            "delete": {"type": "delete", "kind": "joint", "name": "bridge"},
            "rename": {"type": "rename", "kind": "part", "from": "top", "to": "soundboard"},
            "exciter (play point)": {"type": "exciter", "name": "play_e", "kind": "pluck", "on": {"part": "string", "at": [0.2]}, "strength": 0.003, "key": "a", "note": 64},
        });
        let shapes = json!([
            {"kind": "string", "length": 0.65, "radius": 0.0004, "f0": 196},
            {"kind": "bar", "length": 0.3, "width": 0.04, "thickness": 0.012, "ends": "free"},
            {"kind": "plate", "size": [0.4, 0.3], "thickness": 0.003},
            {"kind": "membrane", "radius": 0.15, "f0": 120},
            {"kind": "mass", "mass": 0.01},
        ]);
        let Some(kind) = a.kind else {
            return ok(serde_json::to_string_pretty(&json!({
                "examples": examples,
                "shapes": shapes,
                "materials": phys_core::material::names(),
            }))
            .unwrap_or_default());
        };
        let schema = match kind.as_str() {
            "part" => serde_json::to_value(schemars::schema_for!(Part)),
            "joint" => serde_json::to_value(schemars::schema_for!(Joint)),
            "exciter" => serde_json::to_value(schemars::schema_for!(Exciter)),
            "listener" => serde_json::to_value(schemars::schema_for!(Listener)),
            "settings" => serde_json::to_value(schemars::schema_for!(Settings)),
            "score" | "name" | "delete" | "rename" => {
                serde_json::to_value(schemars::schema_for!(Op))
            }
            other => {
                return fail(format!(
                    "kind `{other}`: use part, joint, exciter, listener, settings, score, name, delete or rename"
                ));
            }
        };
        let mut out =
            json!({"example": examples[kind.as_str()], "schema": schema.unwrap_or_default()});
        if kind == "part" {
            out["shapes"] = shapes;
            out["materials"] = json!(phys_core::material::names());
        }
        ok(serde_json::to_string_pretty(&out).unwrap_or_default())
    }

    /// Excite a point on a part by hand, live: `kind` strike (a mallet, the
    /// default), pluck (pull aside and let go) or bow (a stroke lasting
    /// `duration` seconds; the call returns when the bow lifts). The human
    /// hears it when audio is on, and it joins the take that `listen`
    /// replays. You get no measurement back; use `listen` for that.
    #[tool]
    async fn strike(&self, Parameters(s): Parameters<Strike>) -> Result<CallToolResult, ErrorData> {
        let bow = (s.kind == HandKind::Bow).then(|| s.duration.clamp(0.05, 20.0));
        let started = self.with(|app| {
            let what = s.describe();
            app.strike_live(s)?;
            Ok(format!(
                "{what}; actions in the take: {}{}",
                app.take.len(),
                if app.audio.is_some() {
                    ""
                } else {
                    " (audio is off: nobody heard it)"
                }
            ))
        });
        if let (Ok(_), Some(seconds)) = (&started, bow) {
            tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            let _ = self.with(|app| {
                app.bow_stop();
                Ok(())
            });
        }
        reply(started)
    }

    /// Fire one of the scene's named exciters (a play point), live. With no
    /// strength it uses the exciter's own; 0 lifts a bow.
    #[tool]
    async fn excite(
        &self,
        Parameters(a): Parameters<ExciteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| {
            let strength = app.excite(&a.exciter, a.strength, 1.0)?;
            Ok(format!("fired {} at {strength}", a.exciter))
        }))
    }

    /// Render offline and measure: peak, decay times, the strongest
    /// partials, and a spectrogram image. Plays the given `strikes`, else
    /// the recorded take, else the scene's score.
    #[tool]
    async fn listen(
        &self,
        Parameters(a): Parameters<ListenArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self.with(|app| {
            let (mut r, source) = take_of(app, a.strikes, a.tail)?;
            let mut text = measure(&r, &source);
            let (w, h, rgb) = analysis::spectrogram(&r.mono(), r.sample_rate, 640, 320);
            let image = png(w, h, &rgb)?;
            if let Some(path) = &a.wav {
                r.normalise(-6.0);
                r.write_wav(std::path::Path::new(path))?;
                text += &format!("\nwrote {path}");
            }
            Ok((text, image, r.peak > 0.0))
        });
        match result {
            Ok((text, image, audible)) => {
                let mut content = vec![Content::text(text)];
                if audible {
                    content.push(Content::image(B64.encode(image), "image/png"));
                }
                Ok(CallToolResult::success(content))
            }
            Err(e) => fail(e),
        }
    }

    /// Render offline and run checks, one PASS or FAIL line each. Kinds:
    /// partial {hz, within_pct, min_db}, decay {min, max, drop_db}, audible,
    /// stable. Plays what `listen` would.
    #[tool]
    async fn verify(
        &self,
        Parameters(a): Parameters<VerifyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = self.with(|app| {
            let (r, source) = take_of(app, a.strikes, a.tail)?;
            let mono = r.mono();
            let partials = analysis::partials(&mono, r.sample_rate, 0.0, 40);
            let mut lines = vec![format!("played {source}")];
            let mut failed = 0;
            for c in &a.checks {
                let (pass, line) = match c {
                    Check::Partial {
                        hz,
                        within_pct,
                        min_db,
                    } => {
                        let (pct, floor) = (within_pct.unwrap_or(2.0), min_db.unwrap_or(-40.0));
                        let hit = partials
                            .iter()
                            .filter(|(f, db)| (f / hz - 1.0).abs() * 100.0 <= pct && *db >= floor)
                            .max_by(|a, b| a.1.total_cmp(&b.1));
                        match hit {
                            Some((f, db)) => (
                                true,
                                format!("partial {hz} Hz: found {f:.1} Hz at {db:.0} dB"),
                            ),
                            None => {
                                let near = partials.iter().min_by(|a, b| {
                                    (a.0 / hz).ln().abs().total_cmp(&(b.0 / hz).ln().abs())
                                });
                                let near = near
                                    .map_or("no partials at all".to_string(), |(f, db)| {
                                        format!("nearest is {f:.1} Hz at {db:.0} dB")
                                    });
                                (
                                    false,
                                    format!(
                                        "partial {hz} Hz within {pct}% above {floor} dB: {near}"
                                    ),
                                )
                            }
                        }
                    }
                    Check::Decay { min, max, drop_db } => {
                        let drop = drop_db.unwrap_or(40.0);
                        let t = analysis::decay_time(&mono, r.sample_rate, drop);
                        let pass = min.is_none_or(|m| t >= m) && max.is_none_or(|m| t <= m);
                        (
                            pass,
                            format!("decay: falls {drop} dB in {t:.2} s (wanted {min:?}..{max:?})"),
                        )
                    }
                    Check::Audible => (
                        r.peak > 0.0,
                        format!(
                            "audible: peak {:.1} dB",
                            20.0 * f64::from(r.peak.max(1e-30)).log10()
                        ),
                    ),
                    Check::Stable => (!r.blown, "stable: the simulation stayed finite".to_string()),
                };
                failed += usize::from(!pass);
                lines.push(format!("{} {line}", if pass { "PASS" } else { "FAIL" }));
            }
            lines.push(format!(
                "{} of {} checks passed",
                a.checks.len() - failed,
                a.checks.len()
            ));
            Ok((lines.join("\n"), failed))
        });
        match result {
            Ok((text, 0)) => ok(text),
            Ok((text, _)) => fail(text),
            Err(e) => fail(e),
        }
    }

    /// Correct parts so they sound at the pitch they were given. A joint
    /// pulls a part's pitch away from its own (a string tied to a body goes
    /// 15-20 cents sharp); this plucks each part offline, reads its pitch
    /// off the part itself and sets its `tune` factor to cancel the
    /// difference. Reports cents off before and after. With no `parts`:
    /// every string, bar and membrane. One undo step.
    #[tool]
    async fn tune(&self, Parameters(a): Parameters<TuneArgs>) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| app.tune(a.parts)))
    }

    /// Undo the last edit.
    #[tool]
    async fn undo(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| {
            if app.undo()? {
                Ok(summary(app, self.url.as_deref()))
            } else {
                Err("nothing to undo".into())
            }
        }))
    }

    /// Redo the edit last undone.
    #[tool]
    async fn redo(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| {
            if app.redo()? {
                Ok(summary(app, self.url.as_deref()))
            } else {
                Err("nothing to redo".into())
            }
        }))
    }

    /// Save the scene as JSON.
    #[tool]
    async fn save(&self, Parameters(a): Parameters<SaveArgs>) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| {
            if let Some(p) = a.path {
                app.path = Some(PathBuf::from(p));
            }
            app.save()
        }))
    }

    /// Start an empty scene (no parts, no listeners). Clears the edit
    /// history and the take; the current scene is lost unless saved.
    #[tool]
    async fn new_scene(
        &self,
        Parameters(a): Parameters<NewArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| {
            let scene = Scene {
                name: a.name.unwrap_or_else(|| "untitled".into()),
                ..Scene::default()
            };
            app.replace(scene, None)?;
            Ok(summary(app, self.url.as_deref()))
        }))
    }

    /// Open a scene file. Clears the edit history and the take; the current
    /// scene is lost unless saved.
    #[tool]
    async fn open(&self, Parameters(a): Parameters<OpenArgs>) -> Result<CallToolResult, ErrorData> {
        reply(self.with(|app| {
            app.open_path(std::path::Path::new(&a.path))?;
            Ok(summary(app, self.url.as_deref()))
        }))
    }

    /// A picture of the editor page's 3D viewport, as the human sees it.
    /// Needs the page open in a browser.
    #[tool]
    async fn screenshot(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let waiting = self.with(|app| {
            if app.events.receiver_count() == 0 {
                return Err(format!(
                    "no editor page is open: open {} in a browser first",
                    self.url
                        .as_deref()
                        .unwrap_or("the editor (this server was started headless)")
                ));
            }
            let (tx, rx) = tokio::sync::oneshot::channel();
            app.shot = Some(tx);
            let _ = app.events.send(json!({"type": "shot"}).to_string());
            Ok(rx)
        });
        let rx = match waiting {
            Ok(rx) => rx,
            Err(e) => return fail(e),
        };
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(data)) => match data.split_once("base64,") {
                Some((_, b64)) => Ok(CallToolResult::success(vec![
                    Content::text("the editor's viewport"),
                    Content::image(b64.to_string(), "image/jpeg"),
                ])),
                None => fail("the page sent something that is not an image"),
            },
            _ => fail(
                "the editor page did not answer within 5 s (a background tab may be asleep: bring it to the front)",
            ),
        }
    }
}

#[tool_handler]
impl rmcp::ServerHandler for Tools {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "phys",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(INSTRUCTIONS)
    }
}

/// Serve MCP on stdio, and the editor page unless `headless`.
pub fn run(path: Option<PathBuf>, port: u16, audio: bool, headless: bool) -> Result<(), String> {
    let shared = edit::build(path, audio)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let url = (!headless).then(|| format!("http://localhost:{port}"));
        if !headless {
            let page = shared.clone();
            tokio::spawn(async move {
                if let Err(e) = edit::serve(page, port).await {
                    eprintln!("phys: the editor page is not served: {e}");
                }
            });
        }
        let server = Tools { app: shared, url }
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| format!("MCP server: {e}"))?;
        server
            .waiting()
            .await
            .map_err(|e| format!("MCP server: {e}"))?;
        Ok(())
    })
}
