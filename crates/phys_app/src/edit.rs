//! The editor's backend: owns the scene, runs it on the audio thread, and
//! serves the web UI over one WebSocket.
//!
//! The page sends ops and strikes; the server answers with the whole scene
//! (plus the meshes and mode shapes to draw it) after every change, and
//! streams meters and mode amplitudes about 30 times a second.

use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use phys_audio::{Command, Engine};
use phys_core::ops::{Doc, Op};
use phys_core::parts;
use phys_core::render::Render;
use phys_core::scene::{Scene, Shape};
use phys_core::sim::{Opts, Rig, Sim};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;

/// Modes per part whose shapes are sent for the vibration display.
const DISPLAY_MODES: usize = 12;
const OPTS: Opts = Opts {
    prune: false,
    hand: true,
};

const DEFAULT_SCENE: &str = r#"{
  "name": "untitled",
  "parts": [
    {"name": "plate", "shape": {"kind": "plate", "size": [0.4, 0.3], "thickness": 0.003}, "material": "wood"}
  ],
  "listeners": [
    {"name": "mic", "kind": "mic", "pos": [0.1, 0.35, 0.3]}
  ]
}"#;

pub(crate) enum AudioMsg {
    Swap {
        sim: Box<Sim>,
        display: Vec<usize>,
        gain: f32,
    },
    Command(Command),
}

/// How the roaming hand excites a part.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HandKind {
    /// A mallet thrown at the point.
    #[default]
    Strike,
    /// A finger pulls the point aside and lets go.
    Pluck,
    /// A bow drawn across the point until it is lifted.
    Bow,
}

/// One action of the roaming hand: a strike, a pluck or a bow stroke.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct Strike {
    /// strike (the default), pluck or bow.
    #[serde(default)]
    pub(crate) kind: HandKind,
    /// The part to excite.
    pub(crate) part: String,
    /// Where on it, in the part's port coordinates (each 0..1): [u] for a
    /// string or bar, [u, v] for a plate or membrane, [] for a mass.
    #[serde(default)]
    pub(crate) at: Vec<f64>,
    /// Strike: mallet speed, m/s (default 2). Bow: bow speed, m/s (default 0.2).
    #[serde(default)]
    pub(crate) speed: Option<f64>,
    /// Strike: mallet mass, kg. Default 0.02.
    #[serde(default = "default_mass")]
    pub(crate) mass: f64,
    /// Strike: contact stiffness, N/m^1.5: 1e6 felt, 1e7 rubber, 1e8 wood
    /// (the default), 1e10 steel.
    #[serde(default = "default_stiffness")]
    pub(crate) stiffness: f64,
    /// Pluck: how far the point is pulled aside, metres. Default 0.003.
    #[serde(default = "default_pull")]
    pub(crate) pull: f64,
    /// Pluck: seconds the pull takes before the release. Default 0.02.
    #[serde(default = "default_time")]
    pub(crate) time: f64,
    /// Bow: the most friction it can apply, N. Default 0.3.
    #[serde(default = "default_pressure")]
    pub(crate) pressure: f64,
    /// Bow: seconds the stroke lasts, where nothing else lifts it (offline
    /// renders and the MCP tools). Default 1.
    #[serde(default = "default_duration")]
    pub(crate) duration: f64,
}

impl Strike {
    fn speed(&self) -> f64 {
        self.speed.unwrap_or(match self.kind {
            HandKind::Bow => 0.2,
            _ => 2.0,
        })
    }

    pub(crate) fn describe(&self) -> String {
        match self.kind {
            HandKind::Strike => format!(
                "struck {} at {:?} at {} m/s",
                self.part,
                self.at,
                self.speed()
            ),
            HandKind::Pluck => format!(
                "plucked {} at {:?}, pulled {} m",
                self.part, self.at, self.pull
            ),
            HandKind::Bow => format!(
                "bowed {} at {:?} at {} m/s, pressure {} N",
                self.part,
                self.at,
                self.speed(),
                self.pressure
            ),
        }
    }
}

/// Something that was played, as the take remembers it.
#[derive(Clone, Debug)]
pub(crate) enum Played {
    /// The roaming hand.
    Hand(Strike),
    /// One of the scene's exciters (a play point), fired at a strength.
    Exciter(String, f64),
}

fn default_pull() -> f64 {
    0.003
}

fn default_time() -> f64 {
    0.02
}

fn default_pressure() -> f64 {
    0.3
}

fn default_duration() -> f64 {
    1.0
}

fn default_mass() -> f64 {
    0.02
}

fn default_stiffness() -> f64 {
    1e8
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Ops {
        ops: Vec<Op>,
    },
    Undo,
    Redo,
    Strike(Strike),
    Excite {
        exciter: String,
        /// Left out: the exciter's own default.
        strength: Option<f64>,
        /// Multiplies the strength: how hard a key was hit.
        scale: Option<f64>,
    },
    /// Stop all sound.
    Hush,
    /// Bind the next MIDI note played to this exciter (none: stop waiting).
    Learn {
        exciter: Option<String>,
    },
    Volume {
        value: f32,
    },
    Save,
    /// Save under a new path. With no path, the app shows a file dialog
    /// and a browser page is asked for one.
    SaveAs {
        #[serde(default)]
        path: Option<String>,
    },
    /// Open a scene file; the path is chosen as for `SaveAs`. `force`
    /// skips the unsaved-changes question.
    Open {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        force: bool,
    },
    /// Start again from the default scene.
    New {
        #[serde(default)]
        force: bool,
    },
    Render,
    ClearTake,
    /// Lift the roaming bow.
    BowStop,
    /// The page's answer to a screenshot request: a data URL.
    Shot {
        data: String,
    },
}

/// A file action that needs a dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FileAction {
    New,
    Open,
    Save,
    SaveAs,
}

impl FileAction {
    fn word(self) -> &'static str {
        match self {
            FileAction::New => "new",
            FileAction::Open => "open",
            FileAction::Save | FileAction::SaveAs => "save_as",
        }
    }
}

/// What the server asks of the desktop window, which owns the main thread
/// and so is the only place a dialog can be shown.
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub(crate) enum UiEvent {
    File(FileAction),
    Title(String),
}

pub(crate) struct App {
    /// The desktop window's ear, when there is one.
    pub(crate) ui: Option<Box<dyn Fn(UiEvent) + Send>>,
    /// The scene as last saved or opened, to tell whether there are
    /// unsaved changes.
    saved: Scene,
    /// A reply for the page that sent the message being handled.
    reply: Option<serde_json::Value>,
    pub(crate) doc: Doc,
    pub(crate) path: Option<PathBuf>,
    pub(crate) rate: u32,
    rig: Option<Rig>,
    pub(crate) audio: Option<mpsc::Sender<AudioMsg>>,
    pub(crate) events: broadcast::Sender<String>,
    /// Who is waiting for the page to answer a screenshot request.
    pub(crate) shot: Option<tokio::sync::oneshot::Sender<String>>,
    /// What the last compile reported.
    pub(crate) info: phys_core::sim::Info,
    /// The latest scene message, for pages that connect later.
    scene_msg: String,
    auto_gain: f32,
    volume: f32,
    pub(crate) take: Vec<(f64, Played)>,
    /// The exciter waiting for a MIDI note to be bound to it.
    pub(crate) learn: Option<String>,
    /// Names of the connected MIDI inputs.
    pub(crate) midi: Vec<String>,
    take_start: Option<Instant>,
    /// The bow stroke in `take` that has not been lifted yet.
    bow_open: Option<usize>,
}

pub(crate) type Shared = Arc<Mutex<App>>;

fn flat(points: impl Iterator<Item = [f64; 3]>) -> Vec<f32> {
    points.flat_map(|p| p.map(|x| x as f32)).collect()
}

/// Hit every part once, offline, and return the gain that puts the loudest
/// of them at a comfortable level. Leaves `sim` at rest.
fn level(sim: &mut Sim, rig: &Rig, scene: &Scene) -> f32 {
    let frames = (0.2 * sim.sample_rate()) as usize;
    let mut buf = vec![0.0f32; 2 * frames];
    let mut peak = 0.0f32;
    for (i, part) in scene.parts.iter().enumerate() {
        if matches!(part.shape, Shape::Mass { .. }) {
            continue;
        }
        sim.reset();
        let at = &[0.37, 0.41][..part.shape.at_len()];
        sim.strike(rig.start[i], &rig.weights(i, at, None), 2.0, 0.02, 1e8);
        sim.process(&mut buf);
        peak = buf.iter().fold(peak, |a, x| a.max(x.abs()));
    }
    sim.reset();
    if peak > 0.0 && peak.is_finite() {
        0.2 / peak
    } else {
        1.0
    }
}

impl App {
    /// Compile the current scene, hand it to the audio thread and tell
    /// every page.
    fn rebuild(&mut self) -> Result<(), String> {
        let scene = self.doc.scene().clone();
        let (mut sim, rig) = Sim::compile_with(&scene, f64::from(self.rate), &OPTS)?;
        self.auto_gain = level(&mut sim, &rig, &scene);
        let mut display = Vec::new();
        let mut parts_json = Vec::new();
        for (i, part) in scene.parts.iter().enumerate() {
            let pv = parts::preview(&part.shape);
            let shown = rig.count(i).min(DISPLAY_MODES);
            let mut shapes = vec![Vec::with_capacity(3 * pv.verts.len()); shown];
            for at in &pv.at {
                let sh = rig.shapes(i, at);
                for (m, list) in shapes.iter_mut().enumerate() {
                    list.extend(sh[m].map(|x| x as f32));
                }
            }
            display.extend((0..shown).map(|m| rig.start[i] + m));
            let hz = rig.hz(i);
            parts_json.push(json!({
                "name": part.name,
                "kind": pv.kind,
                "verts": flat(pv.verts.iter().copied()),
                "tris": pv.tris,
                "shapes": shapes,
                "hz": hz.iter().take(shown).collect::<Vec<_>>(),
                "modes": hz.len(),
            }));
        }
        let info = sim.info().clone();
        self.info = info.clone();
        let msg = json!({
            "type": "scene",
            "scene": scene,
            "parts": parts_json,
            "info": {
                "modes": info.modes,
                "oversample": info.oversample,
                "warnings": info.warnings,
                "rate": self.rate,
                "audio": self.audio.is_some(),
            },
            "undo": self.doc.can_undo(),
            "redo": self.doc.can_redo(),
            "path": self.path.as_ref().map(|p| p.display().to_string()),
            "dirty": self.dirty(),
            "take": self.take.len(),
            "midi": self.midi,
            "learn": self.learn,
        })
        .to_string();
        if let Some(tx) = &self.audio {
            let _ = tx.send(AudioMsg::Swap {
                sim: Box::new(sim),
                display,
                gain: self.auto_gain * self.volume,
            });
        }
        self.rig = Some(rig);
        if let Some(ui) = &self.ui {
            ui(UiEvent::Title(self.title()));
        }
        self.scene_msg = msg.clone();
        let _ = self.events.send(msg);
        Ok(())
    }

    fn send(&self, cmd: Command) {
        if let Some(tx) = &self.audio {
            let _ = tx.send(AudioMsg::Command(cmd));
        }
    }

    pub(crate) fn strike(&mut self, s: Strike) -> Result<(), String> {
        let scene = self.doc.scene();
        let part = scene
            .parts
            .iter()
            .position(|p| p.name == s.part)
            .ok_or_else(|| format!("no part named `{}`", s.part))?;
        if s.at.len() != scene.parts[part].shape.at_len() || s.at.iter().any(|x| !x.is_finite()) {
            return Err(format!("at {:?} does not fit `{}`", s.at, s.part));
        }
        let speed = s.speed();
        let fine = match s.kind {
            HandKind::Strike => speed.is_finite() && s.mass > 0.0 && s.stiffness > 0.0,
            HandKind::Pluck => s.pull.is_finite() && s.time > 0.0,
            HandKind::Bow => speed.is_finite() && s.pressure >= 0.0 && s.pressure.is_finite(),
        };
        if !fine {
            return Err(
                "give finite numbers: a mass and stiffness > 0, a time > 0, a pressure >= 0".into(),
            );
        }
        let rig = self.rig.as_ref().ok_or("the scene is not compiled")?;
        let (start, weights) = (rig.start[part], rig.weights(part, &s.at, None));
        self.send(match s.kind {
            HandKind::Strike => Command::Strike {
                start,
                weights,
                speed,
                mass: s.mass,
                stiffness: s.stiffness,
            },
            HandKind::Pluck => Command::Pluck {
                start,
                weights,
                pull: s.pull,
                time: s.time,
            },
            HandKind::Bow => Command::Bow {
                start,
                weights,
                speed,
                pressure: s.pressure,
            },
        });
        let now = self
            .take_start
            .get_or_insert_with(Instant::now)
            .elapsed()
            .as_secs_f64();
        self.close_bow(now);
        if s.kind == HandKind::Bow {
            self.bow_open = Some(self.take.len());
        }
        self.take.push((now, Played::Hand(s)));
        Ok(())
    }

    /// The open bow stroke in the take, if any, ends at `now`.
    fn close_bow(&mut self, now: f64) {
        if let Some((t, Played::Hand(s))) = self.bow_open.take().and_then(|i| self.take.get_mut(i))
        {
            s.duration = (now - *t).max(0.01);
        }
    }

    /// Lift the roaming bow.
    pub(crate) fn bow_stop(&mut self) {
        self.send(Command::BowStop);
        if let Some(start) = self.take_start {
            self.close_bow(start.elapsed().as_secs_f64());
        }
    }

    /// Run the scene offline at 48 kHz: `strikes` with the roaming mallet,
    /// plus the scene's own score when `score` is set, then `tail` seconds.
    /// The result keeps the simulation's own level.
    pub(crate) fn offline(
        &self,
        played: &[(f64, Played)],
        score: bool,
        tail: f64,
    ) -> Result<Render, String> {
        enum Hit<'a> {
            Hand(&'a Strike),
            Lift,
            Exciter(usize, f64),
        }
        let scene = self.doc.scene();
        let sr = 48_000u32;
        let (mut sim, rig) = Sim::compile_with(scene, f64::from(sr), &OPTS)?;
        let mut events: Vec<(f64, Hit)> = Vec::new();
        for (t, p) in played {
            let s = match p {
                Played::Hand(s) => s,
                Played::Exciter(name, strength) => {
                    // An exciter that has since been removed is skipped.
                    if let Some(i) = sim.exciter(name) {
                        events.push((*t, Hit::Exciter(i, *strength)));
                    }
                    continue;
                }
            };
            let part = scene
                .parts
                .iter()
                .position(|p| p.name == s.part)
                .ok_or_else(|| format!("strike: no part named `{}`", s.part))?;
            if s.at.len() != scene.parts[part].shape.at_len() {
                return Err(format!("strike: at {:?} does not fit `{}`", s.at, s.part));
            }
            events.push((*t, Hit::Hand(s)));
            if s.kind == HandKind::Bow {
                events.push((*t + s.duration.clamp(0.01, 600.0), Hit::Lift));
            }
        }
        if score {
            for ev in &scene.score {
                if let Some(i) = sim.exciter(&ev.exciter) {
                    events.push((ev.t, Hit::Exciter(i, ev.strength)));
                }
            }
        }
        if events.is_empty() {
            return Err(
                "nothing to play: give strikes, record a take, or give the scene a score".into(),
            );
        }
        events.sort_by(|a, b| a.0.total_cmp(&b.0));
        let last = events.last().map_or(0.0, |e| e.0).max(0.0);
        let frames = ((last + tail.clamp(0.05, 120.0)) * f64::from(sr)) as usize;
        let mut samples = vec![0.0f32; 2 * frames];
        let mut at = 0usize;
        for (t, hit) in &events {
            let until = ((t.max(0.0) * f64::from(sr)) as usize).clamp(at, frames);
            sim.process(&mut samples[2 * at..2 * until]);
            at = until;
            match hit {
                Hit::Hand(s) => {
                    if let Some(part) = scene.parts.iter().position(|p| p.name == s.part) {
                        let (start, w) = (rig.start[part], rig.weights(part, &s.at, None));
                        match s.kind {
                            HandKind::Strike => {
                                sim.strike(start, &w, s.speed(), s.mass, s.stiffness)
                            }
                            HandKind::Pluck => sim.pluck(start, &w, s.pull, s.time),
                            HandKind::Bow => sim.bow(start, &w, s.speed(), s.pressure),
                        }
                    }
                }
                Hit::Lift => sim.bow_stop(),
                Hit::Exciter(i, strength) => sim.excite(*i, *strength),
            }
        }
        sim.process(&mut samples[2 * at..]);
        let peak = samples.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        Ok(Render {
            samples,
            sample_rate: sr,
            peak,
            blown: sim.blown(),
        })
    }

    /// Replay the recorded strikes offline into a WAV.
    fn render_take(&self) -> Result<String, String> {
        if self.take.is_empty() {
            return Err("nothing to render yet: strike something first".into());
        }
        let scene = self.doc.scene();
        let last = self.take.last().map_or(0.0, |t| t.0);
        let mut r = self.offline(&self.take, false, 4.0)?;
        r.normalise(-6.0);
        let name = if scene.name.is_empty() {
            "take"
        } else {
            &scene.name
        };
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        std::fs::create_dir_all("out").map_err(|e| format!("out: {e}"))?;
        let path = PathBuf::from("out").join(format!("{safe}.wav"));
        r.write_wav(&path)?;
        Ok(format!(
            "rendered {} strikes, {:.1} s, to {}",
            self.take.len(),
            last + 4.0,
            path.display()
        ))
    }

    /// Whether the scene differs from what was last saved or opened.
    pub(crate) fn dirty(&self) -> bool {
        *self.doc.scene() != self.saved
    }

    /// What the window's title bar should say.
    pub(crate) fn title(&self) -> String {
        let scene = self.doc.scene();
        let name = if scene.name.is_empty() {
            "untitled"
        } else {
            &scene.name
        };
        let file = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("not saved".to_string(), |f| {
                f.to_string_lossy().into_owned()
            });
        format!("{name}{} — {file}", if self.dirty() { " *" } else { "" })
    }

    /// Start over from `scene` (a new or opened file). Clears the history
    /// and the take.
    pub(crate) fn replace(&mut self, scene: Scene, path: Option<PathBuf>) -> Result<(), String> {
        let doc = std::mem::replace(&mut self.doc, Doc::new(scene)?);
        let old_path = std::mem::replace(&mut self.path, path);
        let old_saved = std::mem::replace(&mut self.saved, self.doc.scene().clone());
        if let Err(e) = self.rebuild() {
            self.doc = doc;
            self.path = old_path;
            self.saved = old_saved;
            return Err(e);
        }
        self.take.clear();
        self.take_start = None;
        self.bow_open = None;
        let _ = self
            .events
            .send(json!({"type": "take", "count": 0}).to_string());
        Ok(())
    }

    /// Start again from the default scene.
    pub(crate) fn new_scene(&mut self) -> Result<String, String> {
        self.replace(Scene::from_json(DEFAULT_SCENE)?, None)?;
        Ok("new scene".into())
    }

    pub(crate) fn open_path(&mut self, path: &std::path::Path) -> Result<String, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let scene = Scene::from_json(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        self.replace(scene, Some(path.to_path_buf()))?;
        Ok(format!("opened {}", path.display()))
    }

    pub(crate) fn save_to(&mut self, path: PathBuf) -> Result<String, String> {
        self.path = Some(path);
        self.save()
    }

    /// Save to the scene's own file (`phys-scene.json` if it has none).
    pub(crate) fn save(&mut self) -> Result<String, String> {
        let path = self
            .path
            .get_or_insert_with(|| PathBuf::from("phys-scene.json"))
            .clone();
        let text = serde_json::to_string_pretty(self.doc.scene()).map_err(|e| e.to_string())?;
        std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
        self.saved = self.doc.scene().clone();
        // The page and the title bar show whether there are unsaved changes.
        self.rebuild_notice();
        Ok(format!("saved {}", path.display()))
    }

    /// Tell the pages and the window that the file or its saved state
    /// changed, without recompiling anything.
    fn rebuild_notice(&mut self) {
        let msg = json!({
            "type": "file",
            "path": self.path.as_ref().map(|p| p.display().to_string()),
            "dirty": self.dirty(),
        })
        .to_string();
        let _ = self.events.send(msg);
        if let Some(ui) = &self.ui {
            ui(UiEvent::Title(self.title()));
        }
    }

    /// A file action from a page. The desktop window shows the dialog; a
    /// browser page is asked for what is missing and sends the action again.
    fn file(
        &mut self,
        action: FileAction,
        path: Option<String>,
        force: bool,
    ) -> Result<Option<String>, String> {
        let path = path.filter(|p| !p.trim().is_empty()).map(PathBuf::from);
        let needs_path = matches!(action, FileAction::Open | FileAction::SaveAs)
            || (action == FileAction::Save && self.path.is_none());
        let discards =
            matches!(action, FileAction::New | FileAction::Open) && self.dirty() && !force;
        if (needs_path && path.is_none()) || discards {
            match &self.ui {
                Some(ui) => ui(UiEvent::File(action)),
                None => {
                    self.reply = Some(json!({
                        "type": "ask",
                        "what": action.word(),
                        "dirty": discards,
                        "path": self.path.as_ref().map(|p| p.display().to_string()),
                    }));
                }
            }
            return Ok(None);
        }
        match (action, path) {
            (FileAction::New, _) => self.new_scene().map(Some),
            (FileAction::Open, Some(p)) => self.open_path(&p).map(Some),
            (FileAction::SaveAs, Some(p)) | (FileAction::Save, Some(p)) => {
                self.save_to(p).map(Some)
            }
            (FileAction::Save, None) => self.save().map(Some),
            _ => Ok(None),
        }
    }

    /// Handle one message from a page. `Ok(Some(note))` is shown to that page.
    fn handle(&mut self, text: &str) -> Result<Option<String>, String> {
        let msg: ClientMsg = serde_json::from_str(text).map_err(|e| e.to_string())?;
        match msg {
            ClientMsg::Shot { data } => {
                if let Some(tx) = self.shot.take() {
                    let _ = tx.send(data);
                }
                Ok(None)
            }
            ClientMsg::Ops { ops } => self.apply(ops).map(|_| None),
            ClientMsg::Undo => self.undo().map(|_| None),
            ClientMsg::Redo => self.redo().map(|_| None),
            other => self.handle_rest(other),
        }
    }

    /// Apply a batch of ops; nothing changes unless all of it compiles.
    pub(crate) fn apply(&mut self, ops: Vec<Op>) -> Result<(), String> {
        let before = self.doc.scene().clone();
        self.doc.apply(ops)?;
        if let Err(e) = self.rebuild() {
            // Valid on paper but it will not compile: take it back.
            self.doc.undo();
            debug_assert!(*self.doc.scene() == before);
            return Err(e);
        }
        Ok(())
    }

    pub(crate) fn undo(&mut self) -> Result<bool, String> {
        let did = self.doc.undo();
        if did {
            self.rebuild()?;
        }
        Ok(did)
    }

    pub(crate) fn redo(&mut self) -> Result<bool, String> {
        let did = self.doc.redo();
        if did {
            self.rebuild()?;
        }
        Ok(did)
    }

    /// Fire one of the scene's exciters: at `strength`, or at its own
    /// default times `scale`. Zero lifts a bow. Returns the strength used.
    pub(crate) fn excite(
        &mut self,
        exciter: &str,
        strength: Option<f64>,
        scale: f64,
    ) -> Result<f64, String> {
        let scene = self.doc.scene();
        let i = scene
            .exciters
            .iter()
            .position(|e| e.name == exciter)
            .ok_or_else(|| format!("no exciter named `{exciter}`"))?;
        let strength = strength.unwrap_or_else(|| scene.exciters[i].default_strength() * scale);
        if !strength.is_finite() {
            return Err("strength: give a number".into());
        }
        self.send(Command::Excite {
            exciter: i,
            strength,
        });
        let now = self
            .take_start
            .get_or_insert_with(Instant::now)
            .elapsed()
            .as_secs_f64();
        self.take
            .push((now, Played::Exciter(exciter.to_string(), strength)));
        let _ = self.events.send(
            json!({"type": "played", "exciter": exciter, "on": strength != 0.0, "take": self.take.len()}).to_string(),
        );
        Ok(strength)
    }

    /// A MIDI note went down. Binds it when an exciter is waiting to learn
    /// one, otherwise plays every exciter bound to it.
    #[cfg_attr(not(feature = "midi"), allow(dead_code))]
    pub(crate) fn note_on(&mut self, note: u8, velocity: u8) {
        let _ = self
            .events
            .send(json!({"type": "midi_note", "note": note, "velocity": velocity}).to_string());
        if let Some(name) = self.learn.take() {
            if let Some(mut e) = self
                .doc
                .scene()
                .exciters
                .iter()
                .find(|e| e.name == name)
                .cloned()
            {
                e.note = Some(note);
                if let Err(err) = self.apply(vec![Op::Exciter(e)]) {
                    let _ = self
                        .events
                        .send(json!({"type": "error", "message": err}).to_string());
                }
            }
            return;
        }
        // Velocity 64 plays the exciter's own strength; the hardest hit
        // about 1.7 times it.
        let scale = 0.25 + 0.75 * f64::from(velocity) / 64.0;
        let names: Vec<String> = self
            .doc
            .scene()
            .exciters
            .iter()
            .filter(|e| e.note == Some(note))
            .map(|e| e.name.clone())
            .collect();
        for name in names {
            let _ = self.excite(&name, None, scale);
        }
    }

    /// A MIDI note came up: bows bound to it lift.
    #[cfg_attr(not(feature = "midi"), allow(dead_code))]
    pub(crate) fn note_off(&mut self, note: u8) {
        let names: Vec<String> = self
            .doc
            .scene()
            .exciters
            .iter()
            .filter(|e| e.note == Some(note) && e.kind == phys_core::scene::ExciterKind::Bow)
            .map(|e| e.name.clone())
            .collect();
        for name in names {
            let _ = self.excite(&name, Some(0.0), 1.0);
        }
    }

    /// The connected MIDI inputs changed.
    #[cfg_attr(not(feature = "midi"), allow(dead_code))]
    pub(crate) fn set_midi(&mut self, devices: Vec<String>) {
        self.midi = devices;
        let _ = self
            .events
            .send(json!({"type": "midi", "devices": self.midi}).to_string());
    }

    /// Strike live and tell the pages how long the take now is.
    pub(crate) fn strike_live(&mut self, s: Strike) -> Result<(), String> {
        self.strike(s)?;
        let _ = self
            .events
            .send(json!({"type": "take", "count": self.take.len()}).to_string());
        Ok(())
    }

    fn handle_rest(&mut self, msg: ClientMsg) -> Result<Option<String>, String> {
        match msg {
            ClientMsg::Shot { .. } => Ok(None),
            ClientMsg::Ops { ops } => self.apply(ops).map(|_| None),
            ClientMsg::Undo => self.undo().map(|_| None),
            ClientMsg::Redo => self.redo().map(|_| None),
            ClientMsg::Strike(s) => self.strike_live(s).map(|_| None),
            ClientMsg::Excite {
                exciter,
                strength,
                scale,
            } => self
                .excite(&exciter, strength, scale.unwrap_or(1.0).clamp(0.0, 10.0))
                .map(|_| None),
            ClientMsg::Hush => {
                self.send(Command::Hush);
                self.bow_stop();
                Ok(None)
            }
            ClientMsg::Learn { exciter } => {
                self.learn = exciter;
                let _ = self
                    .events
                    .send(json!({"type": "learn", "exciter": self.learn}).to_string());
                Ok(None)
            }
            ClientMsg::Volume { value } => {
                self.volume = value.clamp(0.0, 8.0);
                self.send(Command::Gain(self.auto_gain * self.volume));
                Ok(None)
            }
            ClientMsg::Save => self.file(FileAction::Save, None, false),
            ClientMsg::SaveAs { path } => self.file(FileAction::SaveAs, path, false),
            ClientMsg::Open { path, force } => self.file(FileAction::Open, path, force),
            ClientMsg::New { force } => self.file(FileAction::New, None, force),
            ClientMsg::Render => self.render_take().map(Some),
            ClientMsg::BowStop => {
                self.bow_stop();
                Ok(None)
            }
            ClientMsg::ClearTake => {
                self.bow_stop();
                self.take.clear();
                self.take_start = None;
                let _ = self
                    .events
                    .send(json!({"type": "take", "count": 0}).to_string());
                Ok(Some("take cleared".into()))
            }
        }
    }
}

/// The thread that owns the audio stream (streams cannot change threads).
fn audio_thread(rx: mpsc::Receiver<AudioMsg>, events: broadcast::Sender<String>) {
    let mut engine: Option<Engine> = None;
    let mut display: Vec<usize> = Vec::new();
    let mut levels: Vec<f32> = Vec::new();
    let mut failed = false;
    loop {
        match rx.recv_timeout(Duration::from_millis(33)) {
            Ok(AudioMsg::Swap {
                sim,
                display: d,
                gain,
            }) => {
                display = d;
                match engine.as_mut() {
                    Some(e) => {
                        e.control.send(Command::Gain(gain));
                        e.control.send(Command::Swap(sim));
                    }
                    None if !failed => match Engine::start(*sim, gain) {
                        Ok(e) => {
                            eprintln!("audio: {} at {} Hz", e.device, e.sample_rate);
                            engine = Some(e);
                        }
                        Err(e) => {
                            eprintln!("audio: {e} (running silent)");
                            failed = true;
                        }
                    },
                    None => {}
                }
            }
            Ok(AudioMsg::Command(cmd)) => {
                if let Some(e) = engine.as_mut() {
                    e.control.send(cmd);
                }
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if let Some(e) = engine.as_mut()
            && events.receiver_count() > 0
        {
            let st = e.control.status();
            levels.clear();
            e.control.levels(&display, &mut levels);
            let _ = events.send(
                json!({
                    "type": "tm",
                    "load": st.load,
                    "worst": st.load_peak,
                    "peak": st.peak,
                    "blown": st.blown,
                    "amps": levels,
                })
                .to_string(),
            );
        }
    }
}

async fn socket(ws: WebSocketUpgrade, State(app): State<Shared>) -> Response {
    ws.on_upgrade(move |sock| client(sock, app))
}

async fn client(mut sock: WebSocket, app: Shared) {
    let (hello, mut events) = {
        let Ok(a) = app.lock() else { return };
        (a.scene_msg.clone(), a.events.subscribe())
    };
    if sock.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    eprintln!("phys: an editor page connected");
    loop {
        tokio::select! {
            incoming = sock.recv() => {
                let Some(Ok(msg)) = incoming else { return };
                let Message::Text(text) = msg else { continue };
                let reply = match app.lock() {
                    Ok(mut a) => match a.handle(text.as_str()) {
                        Ok(None) => a.reply.take(),
                        Ok(Some(note)) => Some(json!({"type": "note", "message": note})),
                        Err(e) => Some(json!({"type": "error", "message": e})),
                    },
                    Err(_) => Some(json!({"type": "error", "message": "the editor's state is poisoned"})),
                };
                if let Some(r) = reply
                    && sock.send(Message::Text(r.to_string().into())).await.is_err()
                {
                    return;
                }
            }
            event = events.recv() => match event {
                Ok(text) => {
                    if sock.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                // Fell behind on meters: skip ahead.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

/// The UI, built into the binary. A debug build reads the files from disk
/// first, so the page can be edited without rebuilding.
const FILES: &[(&str, &str, &str)] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_str!("../../../ui/index.html"),
    ),
    (
        "style.css",
        "text/css",
        include_str!("../../../ui/style.css"),
    ),
    (
        "app.js",
        "text/javascript",
        include_str!("../../../ui/app.js"),
    ),
    (
        "state.js",
        "text/javascript",
        include_str!("../../../ui/state.js"),
    ),
    (
        "net.js",
        "text/javascript",
        include_str!("../../../ui/net.js"),
    ),
    (
        "viewport.js",
        "text/javascript",
        include_str!("../../../ui/viewport.js"),
    ),
    (
        "handles.js",
        "text/javascript",
        include_str!("../../../ui/handles.js"),
    ),
    (
        "build.js",
        "text/javascript",
        include_str!("../../../ui/build.js"),
    ),
    (
        "play.js",
        "text/javascript",
        include_str!("../../../ui/play.js"),
    ),
    (
        "inspector.js",
        "text/javascript",
        include_str!("../../../ui/inspector.js"),
    ),
    (
        "vendor/three.module.min.js",
        "text/javascript",
        include_str!("../../../ui/vendor/three.module.min.js"),
    ),
    (
        "vendor/OrbitControls.js",
        "text/javascript",
        include_str!("../../../ui/vendor/OrbitControls.js"),
    ),
    (
        "vendor/TransformControls.js",
        "text/javascript",
        include_str!("../../../ui/vendor/TransformControls.js"),
    ),
];

fn file(path: &str) -> Response {
    let Some((name, mime, body)) = FILES.iter().find(|f| f.0 == path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let body = if cfg!(debug_assertions) {
        let disk = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui")).join(name);
        std::fs::read_to_string(disk).unwrap_or_else(|_| (*body).to_string())
    } else {
        (*body).to_string()
    };
    (
        [
            (header::CONTENT_TYPE, *mime),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    file("index.html")
}

async fn asset(Path(path): Path<String>) -> Response {
    file(&path)
}

/// Load the scene, start the audio thread and compile.
pub(crate) fn build(path: Option<PathBuf>, audio: bool) -> Result<Shared, String> {
    let scene = match &path {
        Some(p) if p.exists() => {
            let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            Scene::from_json(&text)?
        }
        _ => Scene::from_json(DEFAULT_SCENE)?,
    };
    let (events, _) = broadcast::channel(64);
    let (rate, audio_tx) = match audio.then(phys_audio::output) {
        Some(Ok((_, rate))) => {
            let (tx, rx) = mpsc::channel();
            let ev = events.clone();
            std::thread::Builder::new()
                .name("phys-audio".into())
                .spawn(move || audio_thread(rx, ev))
                .map_err(|e| e.to_string())?;
            (rate, Some(tx))
        }
        Some(Err(e)) => {
            eprintln!("audio: {e} (running silent)");
            (48_000, None)
        }
        None => (48_000, None),
    };
    let mut app = App {
        doc: Doc::new(scene)?,
        path,
        rate,
        rig: None,
        audio: audio_tx,
        events,
        ui: None,
        saved: Scene::default(),
        reply: None,
        shot: None,
        info: phys_core::sim::Info::default(),
        scene_msg: String::new(),
        auto_gain: 1.0,
        volume: 1.0,
        take: Vec::new(),
        learn: None,
        midi: Vec::new(),
        take_start: None,
        bow_open: None,
    };
    app.saved = app.doc.scene().clone();
    app.rebuild()?;
    let shared = Arc::new(Mutex::new(app));
    #[cfg(feature = "midi")]
    crate::midi::start(shared.clone());
    Ok(shared)
}

/// Serve the editor page until the process ends.
pub(crate) async fn serve(shared: Shared, port: u16) -> Result<(), String> {
    let router = Router::new()
        .route("/", get(index))
        .route("/ws", get(socket))
        .route("/{*path}", get(asset))
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("port {port}: {e}"))?;
    eprintln!("phys editor: http://localhost:{port}");
    axum::serve(listener, router)
        .await
        .map_err(|e| e.to_string())
}

/// Serve the editor page on a listener that is already bound (the desktop
/// window binds a free port first, so it knows where to point).
#[cfg(feature = "window")]
pub(crate) async fn serve_on(
    shared: Shared,
    listener: std::net::TcpListener,
) -> Result<(), String> {
    let router = Router::new()
        .route("/", get(index))
        .route("/ws", get(socket))
        .route("/{*path}", get(asset))
        .with_state(shared);
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    axum::serve(listener, router)
        .await
        .map_err(|e| e.to_string())
}

pub fn run(path: Option<PathBuf>, port: u16, audio: bool) -> Result<(), String> {
    let shared = build(path, audio)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(serve(shared, port))
}
