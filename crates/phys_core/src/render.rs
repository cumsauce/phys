//! Offline rendering: run a scene's score through the same `Sim` the audio
//! thread uses, into a buffer or a WAV file.

use std::path::Path;

use crate::scene::Scene;
use crate::sim::Sim;

pub struct Render {
    /// Interleaved stereo.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// Peak before any normalising, linear.
    pub peak: f32,
    pub blown: bool,
}

/// Render `seconds` of the scene, firing its score.
pub fn render(scene: &Scene, sample_rate: u32, seconds: f64) -> Result<Render, String> {
    let mut sim = Sim::compile(scene, f64::from(sample_rate))?;
    render_with(&mut sim, scene, seconds)
}

/// Render with a simulation that is already compiled (and maybe configured).
pub fn render_with(sim: &mut Sim, scene: &Scene, seconds: f64) -> Result<Render, String> {
    if !(seconds.is_finite() && seconds > 0.0 && seconds <= 3600.0) {
        return Err(format!("seconds: {seconds}: give 0..3600"));
    }
    let sr = sim.sample_rate();
    let frames = (seconds * sr).round() as usize;
    let mut events: Vec<(usize, usize, f64)> = Vec::new();
    for ev in &scene.score {
        let idx = sim
            .exciter(&ev.exciter)
            .ok_or_else(|| format!("score: no exciter named `{}`", ev.exciter))?;
        events.push(((ev.t * sr).round() as usize, idx, ev.strength));
    }
    events.sort_by_key(|e| e.0);
    let mut samples = vec![0.0f32; frames * 2];
    let mut at = 0usize;
    let mut next = 0usize;
    while at < frames {
        while next < events.len() && events[next].0 <= at {
            sim.excite(events[next].1, events[next].2);
            next += 1;
        }
        let until = events.get(next).map_or(frames, |e| e.0.min(frames));
        let until = until.max(at + 1).min(frames);
        sim.process(&mut samples[at * 2..until * 2]);
        at = until;
    }
    let peak = samples.iter().fold(0.0f32, |a, x| a.max(x.abs()));
    Ok(Render {
        samples,
        sample_rate: sr.round() as u32,
        peak,
        blown: sim.blown(),
    })
}

impl Render {
    /// Scale so the peak sits at `db` dBFS. Offline only: it needs the whole take.
    pub fn normalise(&mut self, db: f32) {
        if self.peak > 0.0 {
            let g = 10f32.powf(db / 20.0) / self.peak;
            self.samples.iter_mut().for_each(|x| *x *= g);
        }
    }

    /// The left and right channels summed to mono.
    pub fn mono(&self) -> Vec<f32> {
        self.samples
            .as_chunks::<2>()
            .0
            .iter()
            .map(|f| 0.5 * (f[0] + f[1]))
            .collect()
    }

    pub fn write_wav(&self, path: &Path) -> Result<(), String> {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: self.sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let err = |e: hound::Error| format!("{}: {e}", path.display());
        let mut w = hound::WavWriter::create(path, spec).map_err(err)?;
        for s in &self.samples {
            w.write_sample(*s).map_err(err)?;
        }
        w.finalize().map_err(err)
    }
}
