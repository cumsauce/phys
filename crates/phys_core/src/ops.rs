//! Editing: a batch of named upserts applied atomically, with undo.
//!
//! The UI and the MCP server both edit a scene through `Doc::apply`, so they
//! cannot disagree about what an edit means.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::scene::{Event, Exciter, Joint, Listener, Part, Scene, Settings};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Part,
    Joint,
    Exciter,
    Listener,
}

/// One edit. `part`, `joint`, `exciter` and `listener` insert, or replace the
/// item of the same name.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Op {
    Part(Part),
    Joint(Joint),
    Exciter(Exciter),
    Listener(Listener),
    Settings(Settings),
    Score { events: Vec<Event> },
    Name { name: String },
    Delete { kind: Kind, name: String },
}

fn upsert<T>(list: &mut Vec<T>, item: T, name: impl Fn(&T) -> &str) {
    match list.iter().position(|x| name(x) == name(&item)) {
        Some(i) => list[i] = item,
        None => list.push(item),
    }
}

fn remove<T>(
    list: &mut Vec<T>,
    kind: &str,
    name: &str,
    of: impl Fn(&T) -> &str,
) -> Result<(), String> {
    let i = list
        .iter()
        .position(|x| of(x) == name)
        .ok_or_else(|| format!("delete: no {kind} named `{name}`"))?;
    list.remove(i);
    Ok(())
}

/// Apply ops to a copy of `scene`; the result is validated.
pub fn apply(scene: &Scene, ops: Vec<Op>) -> Result<Scene, String> {
    let mut s = scene.clone();
    for op in ops {
        match op {
            Op::Part(p) => upsert(&mut s.parts, p, |p| &p.name),
            Op::Joint(j) => upsert(&mut s.joints, j, |j| &j.name),
            Op::Exciter(e) => upsert(&mut s.exciters, e, |e| &e.name),
            Op::Listener(l) => upsert(&mut s.listeners, l, |l| &l.name),
            Op::Settings(st) => s.settings = st,
            Op::Score { events } => s.score = events,
            Op::Name { name } => s.name = name,
            Op::Delete { kind, name } => match kind {
                Kind::Part => remove(&mut s.parts, "part", &name, |p| &p.name)?,
                Kind::Joint => remove(&mut s.joints, "joint", &name, |j| &j.name)?,
                Kind::Exciter => remove(&mut s.exciters, "exciter", &name, |e| &e.name)?,
                Kind::Listener => remove(&mut s.listeners, "listener", &name, |l| &l.name)?,
            },
        }
    }
    s.validate()?;
    Ok(s)
}

/// A scene with its edit history.
#[derive(Clone, Debug, Default)]
pub struct Doc {
    scene: Scene,
    undo: Vec<Scene>,
    redo: Vec<Scene>,
}

impl Doc {
    pub fn new(scene: Scene) -> Result<Doc, String> {
        scene.validate()?;
        Ok(Doc {
            scene,
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// All of `ops`, or none of them.
    pub fn apply(&mut self, ops: Vec<Op>) -> Result<(), String> {
        let next = apply(&self.scene, ops)?;
        if next != self.scene {
            self.undo.push(std::mem::replace(&mut self.scene, next));
            self.redo.clear();
        }
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        match self.undo.pop() {
            Some(prev) => {
                self.redo.push(std::mem::replace(&mut self.scene, prev));
                true
            }
            None => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self.redo.pop() {
            Some(next) => {
                self.undo.push(std::mem::replace(&mut self.scene, next));
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(json: &str) -> Vec<Op> {
        serde_json::from_str(json).expect("ops parse")
    }

    #[test]
    fn a_batch_is_atomic_and_undoable() {
        let mut doc = Doc::default();
        doc.apply(ops(
            r#"[{"type": "part", "name": "bar", "shape": {"kind": "bar", "length": 0.3, "width": 0.03, "thickness": 0.01}},
                {"type": "exciter", "name": "hit", "kind": "impulse", "on": {"part": "bar", "at": [0.2]}}]"#,
        ))
        .expect("applies");
        assert_eq!(doc.scene().parts.len(), 1);
        // The second op names a part that is not there: nothing changes.
        let err = doc
            .apply(ops(
                r#"[{"type": "part", "name": "other", "shape": {"kind": "mass", "mass": 1}},
                    {"type": "joint", "name": "j", "kind": "spring", "a": {"part": "nope"}}]"#,
            ))
            .expect_err("rejected");
        assert!(err.contains("nope"), "{err}");
        assert_eq!(doc.scene().parts.len(), 1);
        assert!(doc.undo());
        assert!(doc.scene().parts.is_empty());
        assert!(doc.redo());
        assert_eq!(doc.scene().exciters.len(), 1);
    }

    #[test]
    fn an_unknown_field_is_an_error() {
        let r: Result<Vec<Op>, _> = serde_json::from_str(
            r#"[{"type": "part", "name": "bar", "shape": {"kind": "bar", "length": 0.3, "width": 0.03, "thickness": 0.01, "colour": 1}}]"#,
        );
        assert!(r.is_err());
    }
}
