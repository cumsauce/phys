//! phys_core: connected modal parts, compiled into a streaming simulation.
//!
//! The flow is `Scene` (a JSON document) -> `bake` (each part becomes a mode
//! table plus mode shapes) -> `Sim::compile` (numbers only) -> `Sim::process`
//! (causal, block based, no allocation).
//!
//! Units are SI: metres, kilograms, seconds, newtons. Y is up.

pub mod analysis;
pub mod bake;
pub mod material;
pub mod math;
pub mod ops;
pub mod parts;
pub mod render;
pub mod scene;
pub mod sim;

pub use ops::{Doc, Op};
pub use scene::Scene;
pub use sim::Sim;
