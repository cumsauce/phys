//! Measurements of a rendered take: its strongest partials and how fast it
//! dies away. Numbers for reports and for agents that cannot hear.

use std::f64::consts::PI;

/// In-place radix-2 FFT; `re.len()` must be a power of two.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut size = 2;
    while size <= n {
        let ang = -2.0 * PI / size as f64;
        for start in (0..n).step_by(size) {
            for k in 0..size / 2 {
                let (s, c) = (ang * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + size / 2);
                let (tr, ti) = (re[b] * c - im[b] * s, re[b] * s + im[b] * c);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        size *= 2;
    }
}

/// The `count` strongest spectral peaks of the take from `from` seconds on:
/// (Hz, dB relative to the strongest).
pub fn partials(mono: &[f32], sample_rate: u32, from: f64, count: usize) -> Vec<(f64, f64)> {
    let start = ((from * f64::from(sample_rate)) as usize).min(mono.len());
    let x = &mono[start..];
    let n = x.len().min(1 << 16).next_power_of_two().max(2);
    let n = if n > x.len() { n / 2 } else { n };
    if n < 64 {
        return Vec::new();
    }
    let mut re: Vec<f64> = (0..n)
        .map(|i| f64::from(x[i]) * (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()))
        .collect();
    let mut im = vec![0.0; n];
    fft(&mut re, &mut im);
    let mag: Vec<f64> = (0..n / 2)
        .map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt())
        .collect();
    let mut peaks: Vec<(f64, f64)> = Vec::new();
    for i in 2..n / 2 - 1 {
        if mag[i] > mag[i - 1] && mag[i] >= mag[i + 1] && mag[i] > 0.0 {
            // Parabolic interpolation on the log magnitudes.
            let (a, b, c) = (
                mag[i - 1].max(1e-300).ln(),
                mag[i].ln(),
                mag[i + 1].max(1e-300).ln(),
            );
            let shift = 0.5 * (a - c) / (a - 2.0 * b + c);
            let hz = (i as f64 + shift) * f64::from(sample_rate) / n as f64;
            peaks.push((hz, mag[i]));
        }
    }
    peaks.sort_by(|a, b| b.1.total_cmp(&a.1));
    let top = peaks.first().map_or(1.0, |p| p.1);
    peaks.truncate(count);
    peaks
        .iter()
        .map(|&(hz, m)| (hz, 20.0 * (m / top).log10()))
        .collect()
}

/// Seconds from the loudest 10 ms to the last 10 ms within `drop_db` of it.
pub fn decay_time(mono: &[f32], sample_rate: u32, drop_db: f64) -> f64 {
    let win = (sample_rate as usize / 100).max(1);
    let rms: Vec<f64> = mono
        .chunks(win)
        .map(|c| (c.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    let Some((peak_at, peak)) = rms
        .iter()
        .copied()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(&b.1))
    else {
        return 0.0;
    };
    let floor = peak * 10f64.powf(-drop_db / 20.0);
    let last = rms.iter().rposition(|x| *x > floor).unwrap_or(peak_at);
    (last.saturating_sub(peak_at)) as f64 * win as f64 / f64::from(sample_rate)
}

/// The frequency of the strongest spectral peak within `span` (a fraction)
/// of `near`, read from the first 1.4 s or so of the take. Good to a few
/// hundredths of a hertz for a steady tone.
pub fn peak_near(mono: &[f32], sample_rate: u32, near: f64, span: f64) -> Option<f64> {
    const N: usize = 1 << 16;
    let take = mono.len().min(N);
    if take < 1024 || near.is_nan() || near <= 0.0 {
        return None;
    }
    let mut re = vec![0.0; N];
    for (i, r) in re.iter_mut().take(take).enumerate() {
        *r = f64::from(mono[i]) * (0.5 - 0.5 * (2.0 * PI * i as f64 / take as f64).cos());
    }
    let mut im = vec![0.0; N];
    fft(&mut re, &mut im);
    let bin = f64::from(sample_rate) / N as f64;
    let lo = ((near * (1.0 - span) / bin) as usize).max(1);
    let hi = ((near * (1.0 + span) / bin).ceil() as usize).min(N / 2 - 2);
    let mag = |i: usize| (re[i] * re[i] + im[i] * im[i]).sqrt();
    let best = (lo..=hi).max_by(|a, b| mag(*a).total_cmp(&mag(*b)))?;
    if mag(best) <= 0.0 {
        return None;
    }
    let (a, b, c) = (
        mag(best - 1).max(1e-300).ln(),
        mag(best).ln(),
        mag(best + 1).max(1e-300).ln(),
    );
    let shift = if (a - 2.0 * b + c).abs() > 1e-12 {
        0.5 * (a - c) / (a - 2.0 * b + c)
    } else {
        0.0
    };
    Some((best as f64 + shift.clamp(-0.5, 0.5)) * bin)
}

/// The pitch of a short stretch of sound, 40 Hz to 2 kHz, or `None` when it
/// has no clear one. The normalised square difference of McLeod and
/// Wyvill: the first strong peak of how well the signal matches a shifted
/// copy of itself.
pub fn pitch(mono: &[f32], sample_rate: u32) -> Option<f64> {
    let sr = f64::from(sample_rate);
    let n = mono.len();
    let (lo, hi) = ((sr / 2000.0) as usize, ((sr / 40.0) as usize).min(n / 2));
    if hi <= lo + 2 {
        return None;
    }
    let x: Vec<f64> = mono.iter().map(|v| f64::from(*v)).collect();
    let mut nsdf = vec![0.0; hi + 1];
    for (tau, out) in nsdf.iter_mut().enumerate().skip(1) {
        let (mut acf, mut energy) = (0.0, 0.0);
        for i in 0..n - tau {
            acf += x[i] * x[i + tau];
            energy += x[i] * x[i] + x[i + tau] * x[i + tau];
        }
        *out = if energy > 0.0 {
            2.0 * acf / energy
        } else {
            0.0
        };
    }
    // The highest point of each stretch above zero, after the first dip.
    let mut peaks: Vec<usize> = Vec::new();
    let mut tau = lo.max(1);
    while tau < hi && nsdf[tau] > 0.0 {
        tau += 1;
    }
    while tau < hi {
        while tau < hi && nsdf[tau] <= 0.0 {
            tau += 1;
        }
        let mut best = tau;
        while tau < hi && nsdf[tau] > 0.0 {
            if nsdf[tau] > nsdf[best] {
                best = tau;
            }
            tau += 1;
        }
        if best < hi && nsdf[best] > 0.0 {
            peaks.push(best);
        }
    }
    let top = peaks.iter().map(|p| nsdf[*p]).fold(0.0, f64::max);
    if top < 0.6 {
        return None;
    }
    let pick = *peaks.iter().find(|p| nsdf[**p] >= 0.9 * top)?;
    let (a, b, c) = (nsdf[pick - 1], nsdf[pick], nsdf[(pick + 1).min(hi)]);
    let shift = if (a - 2.0 * b + c).abs() > 1e-12 {
        0.5 * (a - c) / (a - 2.0 * b + c)
    } else {
        0.0
    };
    Some(sr / (pick as f64 + shift.clamp(-0.5, 0.5)))
}

/// A spectrogram as RGB pixels: (width, height, bytes).
///
/// Time runs left to right over the whole take. Frequency is logarithmic,
/// 40 Hz at the bottom to 20 kHz (or Nyquist) at the top. Brightness is
/// level over an 80 dB range below the loudest point. Faint horizontal lines
/// mark 100 Hz, 1 kHz and 10 kHz; faint vertical lines mark each second.
pub fn spectrogram(
    mono: &[f32],
    sample_rate: u32,
    width: usize,
    height: usize,
) -> (usize, usize, Vec<u8>) {
    const WIN: usize = 4096;
    let sr = f64::from(sample_rate);
    let (lo, hi) = (40.0f64, 20_000f64.min(0.5 * sr));
    let mut db = vec![-200.0f64; width * height];
    let mut top = -200.0f64;
    let (mut re, mut im) = (vec![0.0; WIN], vec![0.0; WIN]);
    for x in 0..width {
        let centre = (x as f64 + 0.5) / width as f64 * mono.len() as f64;
        let start = centre as isize - (WIN / 2) as isize;
        for i in 0..WIN {
            let j = start + i as isize;
            let s = if j >= 0 && (j as usize) < mono.len() {
                f64::from(mono[j as usize])
            } else {
                0.0
            };
            re[i] = s * (0.5 - 0.5 * (2.0 * PI * i as f64 / WIN as f64).cos());
            im[i] = 0.0;
        }
        fft(&mut re, &mut im);
        for y in 0..height {
            // The strongest bin within this pixel's band.
            let f0 = lo * (hi / lo).powf(y as f64 / height as f64);
            let f1 = lo * (hi / lo).powf((y + 1) as f64 / height as f64);
            let b0 = ((f0 * WIN as f64 / sr) as usize).clamp(1, WIN / 2 - 1);
            let b1 = ((f1 * WIN as f64 / sr).ceil() as usize).clamp(b0 + 1, WIN / 2);
            let mut m = 0.0f64;
            for b in b0..b1 {
                m = m.max(re[b] * re[b] + im[b] * im[b]);
            }
            let v = 10.0 * m.max(1e-20).log10();
            db[(height - 1 - y) * width + x] = v;
            top = top.max(v);
        }
    }
    let row_of = |hz: f64| -> Option<usize> {
        let y = ((hz / lo).ln() / (hi / lo).ln() * height as f64) as isize;
        (0..height as isize)
            .contains(&y)
            .then(|| height - 1 - y as usize)
    };
    let lines: Vec<usize> = [100.0, 1000.0, 10_000.0]
        .into_iter()
        .filter_map(row_of)
        .collect();
    let seconds = mono.len() as f64 / sr;
    let mut rgb = Vec::with_capacity(3 * width * height);
    for y in 0..height {
        for x in 0..width {
            let t = ((db[y * width + x] - top + 80.0) / 80.0).clamp(0.0, 1.0);
            // Black, through purple and orange, to pale yellow.
            let mut c = [
                (t * 1.6).min(1.0).powf(1.2),
                ((t - 0.35) * 1.7).clamp(0.0, 1.0),
                (0.55 * (t * 3.2).min(1.0) * (1.0 - t) * 2.0 + (t - 0.8).max(0.0) * 3.0)
                    .clamp(0.0, 1.0),
            ];
            let second = (x as f64 / width as f64 * seconds).floor()
                != ((x + 1) as f64 / width as f64 * seconds).floor();
            if lines.contains(&y) || second {
                c = c.map(|v| v + 0.22 * (1.0 - v));
            }
            rgb.extend(c.map(|v| (v * 255.0) as u8));
        }
    }
    (width, height, rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_sine_and_its_decay() {
        let sr = 48_000u32;
        let x: Vec<f32> = (0..sr)
            .map(|i| {
                let t = f64::from(i) / f64::from(sr);
                ((2.0 * PI * 441.3 * t).sin() * (-t * 6.9).exp()) as f32
            })
            .collect();
        let p = partials(&x, sr, 0.0, 3);
        assert!((p[0].0 - 441.3).abs() < 0.5, "{:?}", p);
        // exp(-6.9 t) falls 60 dB per second: 40 dB in two thirds of one.
        let d = decay_time(&x, sr, 40.0);
        assert!((d - 0.667).abs() < 0.05, "{d}");
        let f = peak_near(&x, sr, 440.0, 0.05).expect("a peak");
        assert!((f - 441.3).abs() < 0.2, "{f}");
    }

    #[test]
    fn hears_the_pitch_of_a_harmonic_tone_and_none_in_noise() {
        let sr = 44_100u32;
        let tone: Vec<f32> = (0..6000)
            .map(|i| {
                let t = f64::from(i) / f64::from(sr);
                (1..6)
                    .map(|h| (2.0 * PI * 146.83 * f64::from(h) * t).sin() / f64::from(h))
                    .sum::<f64>() as f32
            })
            .collect();
        let f = pitch(&tone, sr).expect("a pitch");
        assert!((f - 146.83).abs() < 0.5, "{f}");
        // A fixed pseudo-random sequence has no pitch.
        let mut seed = 12345u32;
        let noise: Vec<f32> = (0..6000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / 8_388_608.0 - 1.0
            })
            .collect();
        assert!(pitch(&noise, sr).is_none());
    }
}
