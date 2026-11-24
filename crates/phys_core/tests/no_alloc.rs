//! The audio thread must never allocate: count allocations across `process`.
//! This lives in its own test binary so no other test allocates meanwhile.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use phys_core::scene::Scene;
use phys_core::sim::Sim;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn process_and_excite_do_not_allocate() {
    let scene = Scene::from_json(
        r#"{
      "parts": [
        {"name": "top", "shape": {"kind": "plate", "size": [0.4, 0.3], "thickness": 0.003}, "material": "wood"},
        {"name": "string", "shape": {"kind": "string", "length": 0.65, "f0": 196}},
        {"name": "screw", "shape": {"kind": "mass", "mass": 0.005}}
      ],
      "joints": [
        {"name": "bridge", "kind": "spring", "a": {"part": "string", "at": [0.95]}, "b": {"part": "top", "at": [0.7, 0.45]}, "stiffness": 2e4, "damping": 1},
        {"name": "loose", "kind": "spring", "a": {"part": "screw"}, "b": {"part": "top", "at": [0.2, 0.2]}, "stiffness": 2e5, "gap": 0.0001}
      ],
      "exciters": [
        {"name": "pick", "kind": "mallet", "on": {"part": "string", "at": [0.18]}, "mass": 0.002},
        {"name": "tap", "kind": "impulse", "on": {"part": "top", "at": [0.3, 0.3]}},
        {"name": "felt", "kind": "damper", "on": {"part": "string", "at": [0.18]}}
      ],
      "listeners": [
        {"name": "mic", "kind": "mic", "pos": [0.1, 0.4, 0.3]},
        {"name": "pu", "kind": "pickup", "on": {"part": "top", "at": [0.5, 0.5]}}
      ]
    }"#,
    )
    .expect("scene parses");
    let mut sim = Sim::compile(&scene, 48_000.0).expect("compiles");
    let mut buf = vec![0.0f32; 2 * 512];
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 0..50 {
        if i % 10 == 0 {
            sim.excite(0, 2.0);
            sim.excite(1, 0.01);
            // The damper lifts and falls: the joints are factored again.
            sim.excite(2, if i % 20 == 0 { 1.0 } else { 0.0 });
        }
        sim.process(&mut buf);
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(after - before, 0, "process allocated");
    assert!(buf.iter().any(|x| *x != 0.0));
}
