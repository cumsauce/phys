//! Real-time output.
//!
//! The audio callback owns the simulation. Everything else talks to it
//! through a lock-free queue of `Command`s, and reads back a few atomics.
//! The callback never locks, allocates or frees: a replaced simulation is
//! handed back through a second queue to be dropped elsewhere.
//!
//! `Core` is the device-free part (and what the tests drive); `Engine` wraps
//! it in a cpal output stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use phys_core::sim::Sim;
use rtrb::{Consumer, Producer, RingBuffer};

/// Frames over which a replaced simulation fades out and the new one in.
const FADE: usize = 1024;
/// Most frames rendered in one go; the scratch buffers are this long.
const BLOCK: usize = 2048;

pub enum Command {
    /// Fire an exciter: a mallet's speed in m/s, or an impulse in N.s.
    Excite { exciter: usize, strength: f64 },
    /// Hit a part with the roaming mallet; see `Sim::strike`.
    Strike {
        start: usize,
        weights: Vec<f64>,
        speed: f64,
        mass: f64,
        stiffness: f64,
    },
    /// Pluck a part with the roaming finger; see `Sim::pluck`.
    Pluck {
        start: usize,
        weights: Vec<f64>,
        pull: f64,
        time: f64,
    },
    /// Start or change the roaming bow; see `Sim::bow`.
    Bow {
        start: usize,
        weights: Vec<f64>,
        speed: f64,
        pressure: f64,
    },
    /// Lift the roaming bow.
    BowStop,
    /// Replace the simulation (after an edit), with a short crossfade. It
    /// carries on from the old one's motion when their modes match.
    Swap(Box<Sim>),
    /// Linear output gain.
    Gain(f32),
}

/// Things the audio thread is done with, sent back to be freed elsewhere.
enum Trash {
    Sim(#[allow(dead_code)] Box<Sim>),
    Weights(#[allow(dead_code)] Vec<f64>),
}

/// How many mode amplitudes are published for display.
pub const LEVELS: usize = 4096;

struct Shared {
    /// f32 bits: amplitude of each of the first `LEVELS` modes.
    levels: Vec<AtomicU32>,
    /// f32 bits: time spent rendering / time the audio lasts, smoothed.
    load: AtomicU32,
    /// f32 bits: the worst single callback since the last `status`.
    load_peak: AtomicU32,
    /// f32 bits: output peak since the last `status`, before the limiter.
    peak: AtomicU32,
    blown: AtomicBool,
    frames: AtomicU64,
}

impl Default for Shared {
    fn default() -> Self {
        Shared {
            levels: (0..LEVELS).map(|_| AtomicU32::new(0)).collect(),
            load: AtomicU32::new(0),
            load_peak: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            blown: AtomicBool::new(false),
            frames: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    /// Fraction of real time the simulation takes: above 1 it cannot keep up.
    pub load: f32,
    pub load_peak: f32,
    /// Output peak since the last call, linear, before the limiter.
    pub peak: f32,
    /// The simulation went non-finite and was reset.
    pub blown: bool,
    pub frames: u64,
}

/// Leaves quiet signals alone and bends anything above 0.7 under 1.
#[inline]
fn limit(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.7 {
        x
    } else {
        x.signum() * (0.7 + 0.3 * ((a - 0.7) / 0.3).tanh())
    }
}

/// The audio-thread side, free of any device.
pub struct Core {
    sim: Box<Sim>,
    /// A replaced simulation, still fading out, and the frames it has left.
    old: Option<(Box<Sim>, usize)>,
    gain: f32,
    commands: Consumer<Command>,
    trash: Producer<Trash>,
    shared: Arc<Shared>,
    scratch: Vec<f32>,
    sample_rate: f64,
}

/// The control side: send commands, read status.
pub struct Control {
    commands: Producer<Command>,
    trash: Consumer<Trash>,
    shared: Arc<Shared>,
}

/// A core and its control, connected.
pub fn pair(sim: Sim, gain: f32) -> (Core, Control) {
    let (ctx, crx) = RingBuffer::new(1024);
    let (ttx, trx) = RingBuffer::new(16);
    let shared = Arc::new(Shared::default());
    let sample_rate = sim.sample_rate();
    (
        Core {
            sim: Box::new(sim),
            old: None,
            gain,
            commands: crx,
            trash: ttx,
            shared: shared.clone(),
            scratch: vec![0.0; 2 * BLOCK],
            sample_rate,
        },
        Control {
            commands: ctx,
            trash: trx,
            shared,
        },
    )
}

impl Core {
    fn retire(&mut self, sim: Box<Sim>) {
        // If the trash queue is full the box is dropped here: a rare
        // deallocation on the audio thread beats leaking it.
        let _ = self.trash.push(Trash::Sim(sim));
    }

    /// Render interleaved stereo into `out`.
    pub fn fill(&mut self, out: &mut [f32]) {
        let t0 = Instant::now();
        while let Ok(cmd) = self.commands.pop() {
            match cmd {
                Command::Excite { exciter, strength } => self.sim.excite(exciter, strength),
                Command::Gain(g) => self.gain = g,
                Command::Strike {
                    start,
                    weights,
                    speed,
                    mass,
                    stiffness,
                } => {
                    self.sim.strike(start, &weights, speed, mass, stiffness);
                    let _ = self.trash.push(Trash::Weights(weights));
                }
                Command::Pluck {
                    start,
                    weights,
                    pull,
                    time,
                } => {
                    self.sim.pluck(start, &weights, pull, time);
                    let _ = self.trash.push(Trash::Weights(weights));
                }
                Command::Bow {
                    start,
                    weights,
                    speed,
                    pressure,
                } => {
                    self.sim.bow(start, &weights, speed, pressure);
                    let _ = self.trash.push(Trash::Weights(weights));
                }
                Command::BowStop => self.sim.bow_stop(),
                Command::Swap(mut new) => {
                    new.adopt(&self.sim);
                    let prev = std::mem::replace(&mut self.sim, new);
                    if let Some((older, _)) = self.old.replace((prev, FADE)) {
                        self.retire(older);
                    }
                }
            }
        }
        let mut peak = 0.0f32;
        for chunk in out.chunks_mut(2 * BLOCK) {
            self.sim.process(chunk);
            if let Some((old, left)) = self.old.as_mut() {
                // Crossfade: the new simulation in, the old one out.
                let frames = chunk.len() / 2;
                let tail = &mut self.scratch[..chunk.len()];
                old.process(tail);
                for i in 0..frames {
                    let remaining = left.saturating_sub(i);
                    let w_old = remaining as f32 / FADE as f32;
                    let w_new = 1.0 - w_old;
                    chunk[2 * i] = chunk[2 * i] * w_new + tail[2 * i] * w_old;
                    chunk[2 * i + 1] = chunk[2 * i + 1] * w_new + tail[2 * i + 1] * w_old;
                }
                *left = left.saturating_sub(frames);
                if *left == 0
                    && let Some((old, _)) = self.old.take()
                {
                    self.retire(old);
                }
            }
            for s in chunk.iter_mut() {
                let x = *s * self.gain;
                peak = peak.max(x.abs());
                *s = limit(x);
            }
        }
        let frames = out.len() / 2;
        if frames > 0 {
            let load = (t0.elapsed().as_secs_f64() * self.sample_rate / frames as f64) as f32;
            let sh = &self.shared;
            let prev = f32::from_bits(sh.load.load(Ordering::Relaxed));
            sh.load
                .store((prev + 0.1 * (load - prev)).to_bits(), Ordering::Relaxed);
            let worst = f32::from_bits(sh.load_peak.load(Ordering::Relaxed)).max(load);
            sh.load_peak.store(worst.to_bits(), Ordering::Relaxed);
            let top = f32::from_bits(sh.peak.load(Ordering::Relaxed)).max(peak);
            sh.peak.store(top.to_bits(), Ordering::Relaxed);
            sh.frames.fetch_add(frames as u64, Ordering::Relaxed);
            if self.sim.blown() {
                sh.blown.store(true, Ordering::Relaxed);
            }
            let n = self.sim.modes().min(LEVELS);
            for (i, slot) in sh.levels[..n].iter().enumerate() {
                slot.store((self.sim.amplitude(i) as f32).to_bits(), Ordering::Relaxed);
            }
        }
    }
}

impl Control {
    /// False when the queue is full (the audio thread is not running).
    pub fn send(&mut self, cmd: Command) -> bool {
        self.commands.push(cmd).is_ok()
    }

    pub fn excite(&mut self, exciter: usize, strength: f64) -> bool {
        self.send(Command::Excite { exciter, strength })
    }

    /// Replace the running simulation. It must be compiled at the engine's
    /// sample rate.
    pub fn swap(&mut self, sim: Sim) -> bool {
        self.send(Command::Swap(Box::new(sim)))
    }

    /// The published amplitude of each mode in `modes`, appended to `out`.
    pub fn levels(&self, modes: &[usize], out: &mut Vec<f32>) {
        out.extend(modes.iter().map(|&i| {
            self.shared
                .levels
                .get(i)
                .map_or(0.0, |a| f32::from_bits(a.load(Ordering::Relaxed)))
        }));
    }

    /// Read the meters (resetting the peaks) and free retired simulations.
    pub fn status(&mut self) -> Status {
        while self.trash.pop().is_ok() {}
        let sh = &self.shared;
        Status {
            load: f32::from_bits(sh.load.load(Ordering::Relaxed)),
            load_peak: f32::from_bits(sh.load_peak.swap(0, Ordering::Relaxed)),
            peak: f32::from_bits(sh.peak.swap(0, Ordering::Relaxed)),
            blown: sh.blown.load(Ordering::Relaxed),
            frames: sh.frames.load(Ordering::Relaxed),
        }
    }
}

/// The default output device's name and sample rate.
pub fn output() -> Result<(String, u32), String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output device")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "the default output".into());
    let config = device
        .default_output_config()
        .map_err(|e| format!("{name}: {e}"))?;
    let config: cpal::StreamConfig = config.into();
    Ok((name, config.sample_rate))
}

/// A running output stream. Dropping it stops the sound.
pub struct Engine {
    _stream: cpal::Stream,
    pub control: Control,
    pub device: String,
    pub sample_rate: u32,
}

impl Engine {
    /// Start playing `sim`, which must be compiled at `output()`'s rate.
    pub fn start(sim: Sim, gain: f32) -> Result<Engine, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no audio output device")?;
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "the default output".into());
        let supported = device
            .default_output_config()
            .map_err(|e| format!("{name}: {e}"))?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let sample_rate = config.sample_rate;
        if (sim.sample_rate() - f64::from(sample_rate)).abs() > 0.5 {
            return Err(format!(
                "the simulation is compiled at {} Hz but {name} runs at {sample_rate} Hz",
                sim.sample_rate()
            ));
        }
        let (core, control) = pair(sim, gain);
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(&device, config, core),
            cpal::SampleFormat::I16 => build::<i16>(&device, config, core),
            cpal::SampleFormat::U16 => build::<u16>(&device, config, core),
            cpal::SampleFormat::I32 => build::<i32>(&device, config, core),
            other => Err(format!("unsupported sample format {other}")),
        }
        .map_err(|e| format!("{name}: {e}"))?;
        stream.play().map_err(|e| format!("{name}: {e}"))?;
        Ok(Engine {
            _stream: stream,
            control,
            device: name,
            sample_rate,
        })
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut core: Core,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = (config.channels as usize).max(1);
    let mut stereo = vec![0.0f32; 2 * BLOCK];
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                for chunk in data.chunks_mut(BLOCK * channels) {
                    let frames = chunk.len() / channels;
                    let buf = &mut stereo[..2 * frames];
                    core.fill(buf);
                    for (frame, lr) in chunk.chunks_mut(channels).zip(buf.chunks(2)) {
                        if channels == 1 {
                            frame[0] = T::from_sample(0.5 * (lr[0] + lr[1]));
                        } else {
                            frame[0] = T::from_sample(lr[0]);
                            frame[1] = T::from_sample(lr[1]);
                            for extra in &mut frame[2..] {
                                *extra = T::from_sample(0.0);
                            }
                        }
                    }
                }
            },
            |e| eprintln!("phys: audio stream: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phys_core::scene::Scene;

    fn sim(f0: f64) -> Sim {
        let scene = Scene::from_json(&format!(
            r#"{{
          "parts": [{{"name": "s", "shape": {{"kind": "string", "length": 0.65, "f0": {f0}}}}}],
          "exciters": [{{"name": "hit", "kind": "impulse", "on": {{"part": "s", "at": [0.3]}}}}],
          "listeners": [{{"name": "pu", "kind": "pickup", "on": {{"part": "s", "at": [0.4]}}}}]
        }}"#
        ))
        .expect("scene parses");
        Sim::compile(&scene, 48_000.0).expect("compiles")
    }

    #[test]
    fn commands_reach_the_simulation_and_meters_come_back() {
        let (mut core, mut control) = pair(sim(220.0), 1.0);
        let mut buf = vec![0.0f32; 2 * 512];
        core.fill(&mut buf);
        assert!(buf.iter().all(|x| *x == 0.0));
        assert!(control.excite(0, 0.001));
        core.fill(&mut buf);
        assert!(buf.iter().any(|x| *x != 0.0));
        let st = control.status();
        assert!(st.peak > 0.0 && st.frames == 1024 && !st.blown);
    }

    #[test]
    fn a_swap_crossfades_and_hands_the_old_simulation_back() {
        let (mut core, mut control) = pair(sim(220.0), 1.0);
        let mut buf = vec![0.0f32; 2 * 256];
        control.excite(0, 0.001);
        core.fill(&mut buf);
        let before = buf[buf.len() - 2];
        assert!(control.swap(sim(330.0)));
        core.fill(&mut buf);
        // The first frame after the swap is still almost all the old sound.
        assert!(buf[0] != 0.0 && (buf[0] - before).abs() < before.abs().max(1e-9) * 2.0);
        for _ in 0..(FADE / 256) + 1 {
            core.fill(&mut buf);
        }
        // A different string cannot take over the old one's motion: once
        // the fade is over, silence.
        assert!(buf.iter().all(|x| *x == 0.0));
        assert_eq!(control.trash.slots(), 1);
        control.status();
        assert_eq!(control.trash.slots(), 0);
    }

    #[test]
    fn the_limiter_keeps_loud_output_under_full_scale() {
        for x in [-30.0f32, -1.0, -0.71, 0.5, 0.9, 4.0, 1e6] {
            let y = limit(x);
            assert!(y.abs() <= 1.0 && y.signum() == x.signum());
        }
        assert_eq!(limit(0.3), 0.3);
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;
    use phys_core::scene::Scene;
    use phys_core::sim::Opts;

    const PLATE: &str = r#"{
      "parts": [{"name": "top", "shape": {"kind": "plate", "size": [0.4, 0.3], "thickness": 0.003}, "material": "wood"}],
      "listeners": [{"name": "mic", "kind": "mic", "pos": [0.1, 0.4, PAN]}]
    }"#;

    #[test]
    fn a_strike_lands_anywhere_and_an_edit_keeps_it_ringing() {
        let opts = Opts {
            prune: false,
            hand: true,
        };
        let scene = Scene::from_json(&PLATE.replace("PAN", "0.3")).expect("parses");
        let (sim, rig) = Sim::compile_with(&scene, 48_000.0, &opts).expect("compiles");
        let (mut core, mut control) = pair(sim, 1.0);
        let mut buf = vec![0.0f32; 2 * 2048];
        control.send(Command::Strike {
            start: rig.start[0],
            weights: rig.weights(0, &[0.3, 0.6], None),
            speed: 2.0,
            mass: 0.004,
            stiffness: 1e7,
        });
        core.fill(&mut buf);
        assert!(buf.iter().any(|x| *x != 0.0));
        let mut levels = Vec::new();
        control.levels(&[0, 1, 2], &mut levels);
        assert!(levels.iter().all(|a| *a > 0.0), "{levels:?}");

        // The roaming finger and bow reach the simulation too.
        control.status();
        control.send(Command::Pluck {
            start: rig.start[0],
            weights: rig.weights(0, &[0.6, 0.3], None),
            pull: 0.002,
            time: 0.01,
        });
        control.send(Command::Bow {
            start: rig.start[0],
            weights: rig.weights(0, &[0.9, 0.5], None),
            speed: 0.2,
            pressure: 0.3,
        });
        for _ in 0..12 {
            core.fill(&mut buf);
        }
        let bowed = control.status().peak;
        control.send(Command::BowStop);
        for _ in 0..60 {
            core.fill(&mut buf);
        }
        control.status();
        core.fill(&mut buf);
        let lifted = control.status().peak;
        assert!(
            bowed > 0.0 && lifted < bowed * 0.5,
            "bowed {bowed}, lifted {lifted}"
        );

        // Move the microphone: same modes, so the plate keeps ringing.
        control.send(Command::Strike {
            start: rig.start[0],
            weights: rig.weights(0, &[0.3, 0.6], None),
            speed: 2.0,
            mass: 0.004,
            stiffness: 1e7,
        });
        core.fill(&mut buf);
        let moved = Scene::from_json(&PLATE.replace("PAN", "0.5")).expect("parses");
        let (sim, _) = Sim::compile_with(&moved, 48_000.0, &opts).expect("compiles");
        control.swap(sim);
        for _ in 0..3 {
            core.fill(&mut buf);
        }
        assert!(buf.iter().any(|x| *x != 0.0), "the edit silenced the plate");
    }
}
