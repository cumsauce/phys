//! phys command line.
//!
//!   phys render <scene.json> [out.wav] [--seconds 3] [--rate 48000]
//!               [--solver sav|explicit] [--oversample N] [--raw]
//!   phys play <scene.json> [--seconds N] [--gain dB]
//!   phys edit [scene.json] [--port 7878] [--no-audio]
//!   phys mcp [scene.json] [--port 7878] [--no-audio] [--headless]
//!   phys info <scene.json>

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

mod edit;
mod mcp;

use phys_core::render::render_with;
use phys_core::scene::{Scene, Solver};
use phys_core::sim::Sim;

const USAGE: &str = "usage:
  phys render <scene.json> [out.wav] [--seconds 3] [--rate 48000] [--solver sav|explicit] [--oversample N] [--raw]
  phys play <scene.json> [--seconds N] [--gain dB]
  phys edit [scene.json] [--port 7878] [--no-audio]
  phys mcp [scene.json] [--port 7878] [--no-audio] [--headless]
  phys info <scene.json>

mcp serves the MCP tools on stdio and the editor page alongside (--headless: tools only).
edit opens the 3D editor in a browser tab at http://localhost:7878.
render writes a WAV normalised to -6 dBFS; --raw keeps the simulation's own level.
play runs the scene live on the default output: it plays the score, then takes
strikes from the keyboard. With --seconds it plays that long and exits.";

struct Args {
    command: String,
    scene: PathBuf,
    out: Option<PathBuf>,
    seconds: f64,
    seconds_given: bool,
    gain_db: Option<f64>,
    rate: u32,
    solver: Option<Solver>,
    oversample: Option<u32>,
    raw: bool,
    port: u16,
    audio: bool,
    headless: bool,
}

fn parse() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().ok_or(USAGE)?;
    let mut args = Args {
        command,
        scene: PathBuf::new(),
        out: None,
        seconds: 3.0,
        seconds_given: false,
        gain_db: None,
        rate: 48_000,
        solver: None,
        oversample: None,
        raw: false,
        port: 7878,
        audio: true,
        headless: false,
    };
    let mut free = Vec::new();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--seconds" => {
                args.seconds = value("--seconds")?
                    .parse()
                    .map_err(|_| "--seconds: give a number")?;
                args.seconds_given = true;
            }
            "--gain" => {
                args.gain_db = Some(value("--gain")?.parse().map_err(|_| "--gain: give dB")?)
            }
            "--rate" => args.rate = value("--rate")?.parse().map_err(|_| "--rate: give Hz")?,
            "--oversample" => {
                args.oversample = Some(
                    value("--oversample")?
                        .parse()
                        .map_err(|_| "--oversample: give 1..64")?,
                )
            }
            "--solver" => {
                args.solver = Some(match value("--solver")?.as_str() {
                    "sav" => Solver::Sav,
                    "explicit" => Solver::Explicit,
                    other => return Err(format!("--solver {other}: give sav or explicit")),
                })
            }
            "--raw" => args.raw = true,
            "--no-audio" => args.audio = false,
            "--headless" => args.headless = true,
            "--port" => {
                args.port = value("--port")?
                    .parse()
                    .map_err(|_| "--port: give a port")?
            }
            flag if flag.starts_with("--") => return Err(format!("unknown flag {flag}\n{USAGE}")),
            _ => free.push(a),
        }
    }
    let mut free = free.into_iter();
    if args.command == "edit" || args.command == "mcp" {
        args.scene = free.next().map(PathBuf::from).unwrap_or_default();
        return Ok(args);
    }
    args.scene = free.next().map(PathBuf::from).ok_or(USAGE)?;
    args.out = free.next().map(PathBuf::from);
    Ok(args)
}

fn load(args: &Args) -> Result<Scene, String> {
    let text = std::fs::read_to_string(&args.scene)
        .map_err(|e| format!("{}: {e}", args.scene.display()))?;
    let mut scene = Scene::from_json(&text)?;
    if let Some(s) = args.solver {
        scene.settings.solver = s;
        scene.settings.oversample = args.oversample;
    } else if args.oversample.is_some() {
        scene.settings.oversample = args.oversample;
    }
    Ok(scene)
}

fn describe(sim: &Sim) {
    let info = sim.info();
    println!(
        "{} modes, {:?} solver at {}x, {} linear + {} nonlinear joints",
        info.modes, info.solver, info.oversample, info.linear_joints, info.nonlinear_joints
    );
    for (name, kept, baked) in &info.parts {
        println!("  {name}: {kept} of {baked} modes in use");
    }
    for w in &info.warnings {
        println!("  warning: {w}");
    }
}

fn meters(engine: &mut phys_audio::Engine) {
    let st = engine.control.status();
    println!(
        "  load {:.0}% (worst {:.0}%), peak {:.1} dB{}",
        100.0 * st.load,
        100.0 * st.load_peak,
        20.0 * f64::from(st.peak.max(1e-30)).log10(),
        if st.blown {
            ", BLEW UP (state reset)"
        } else {
            ""
        }
    );
}

fn play(args: &Args, scene: &Scene) -> Result<(), String> {
    let (device, rate) = phys_audio::output()?;
    let sim = Sim::compile(scene, f64::from(rate))?;
    describe(&sim);
    if scene.exciters.is_empty() {
        return Err("the scene has no exciters: nothing to play".into());
    }
    // The simulation's own level is physical and tiny. Without --gain, run
    // the score (or one hit of each exciter) offline and aim its peak at
    // -12 dBFS.
    let gain = match args.gain_db {
        Some(db) => 10f32.powf(db as f32 / 20.0),
        None => {
            let mut probe = scene.clone();
            if probe.score.is_empty() {
                probe.score = scene
                    .exciters
                    .iter()
                    .map(|e| phys_core::scene::Event {
                        t: 0.0,
                        exciter: e.name.clone(),
                        strength: 1.0,
                    })
                    .collect();
            }
            let peak = render_with(&mut sim.clone(), &probe, 2.0)?.peak;
            if peak > 0.0 { 0.25 / peak } else { 1.0 }
        }
    };
    let mut events: Vec<(f64, usize, f64)> = scene
        .score
        .iter()
        .filter_map(|ev| sim.exciter(&ev.exciter).map(|i| (ev.t, i, ev.strength)))
        .collect();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut engine = phys_audio::Engine::start(sim, gain)?;
    println!(
        "playing on {device} at {rate} Hz, gain {:.1} dB",
        20.0 * f64::from(gain).log10()
    );
    let t0 = Instant::now();
    let wait_until = |t: f64| {
        let now = t0.elapsed().as_secs_f64();
        if t > now {
            std::thread::sleep(std::time::Duration::from_secs_f64(t - now));
        }
    };
    for (t, exciter, strength) in events {
        if args.seconds_given && t > args.seconds {
            break;
        }
        wait_until(t);
        engine.control.excite(exciter, strength);
    }
    if args.seconds_given {
        wait_until(args.seconds);
        meters(&mut engine);
        return Ok(());
    }
    let names: Vec<String> = scene
        .exciters
        .iter()
        .enumerate()
        .map(|(i, e)| format!("{}) {}", i + 1, e.name))
        .collect();
    println!(
        "exciters: {}\ntype a number and Enter to strike (add a strength, e.g. `1 3.5`); Enter repeats; q quits",
        names.join("  ")
    );
    let (mut last, mut line) = ((0usize, 1.0f64), String::new());
    loop {
        line.clear();
        let read = std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        let mut words = line.split_whitespace();
        match words.next() {
            _ if read == 0 => break,
            Some("q") => break,
            Some(w) => match w.parse::<usize>() {
                Ok(n) if (1..=scene.exciters.len()).contains(&n) => {
                    let strength = words.next().and_then(|s| s.parse().ok()).unwrap_or(last.1);
                    last = (n - 1, strength);
                }
                _ => {
                    println!("  give 1..{} or q", scene.exciters.len());
                    continue;
                }
            },
            None => {}
        }
        engine.control.excite(last.0, last.1);
        std::thread::sleep(std::time::Duration::from_millis(150));
        meters(&mut engine);
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args = parse()?;
    if args.command == "edit" || args.command == "mcp" {
        let path = (!args.scene.as_os_str().is_empty()).then(|| args.scene.clone());
        return if args.command == "edit" {
            edit::run(path, args.port, args.audio)
        } else {
            mcp::run(path, args.port, args.audio && !args.headless, args.headless)
        };
    }
    let scene = load(&args)?;
    if args.command == "play" {
        return play(&args, &scene);
    }
    let mut sim = Sim::compile(&scene, f64::from(args.rate))?;
    match args.command.as_str() {
        "info" => {
            describe(&sim);
            Ok(())
        }
        "render" => {
            describe(&sim);
            let t0 = Instant::now();
            let mut r = render_with(&mut sim, &scene, args.seconds)?;
            let took = t0.elapsed().as_secs_f64();
            println!(
                "rendered {:.2} s in {:.3} s ({:.0}x real time), peak {:.1} dB{}",
                args.seconds,
                took,
                args.seconds / took.max(1e-9),
                20.0 * f64::from(r.peak.max(1e-30)).log10(),
                if r.blown {
                    ", BLEW UP (state reset)"
                } else {
                    ""
                }
            );
            let mono = r.mono();
            let list: Vec<String> = phys_core::analysis::partials(&mono, r.sample_rate, 0.0, 6)
                .iter()
                .map(|(hz, db)| format!("{hz:.0} Hz ({db:.0} dB)"))
                .collect();
            println!("strongest partials: {}", list.join(", "));
            println!(
                "falls 40 dB in {:.2} s",
                phys_core::analysis::decay_time(&mono, r.sample_rate, 40.0)
            );
            if !args.raw {
                r.normalise(-6.0);
            }
            let out = args
                .out
                .clone()
                .unwrap_or_else(|| args.scene.with_extension("wav"));
            r.write_wav(&out)?;
            println!("wrote {}", out.display());
            Ok(())
        }
        other => Err(format!("unknown command {other}\n{USAGE}")),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
