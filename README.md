# phys

Build a sounding object out of connected parts, excite it, and hear it.

A scene is a JSON document of parts (string, bar, plate, membrane, point mass), joints (springs, rattling springs, contacts), exciters (mallets, impulses, plucks, bows) and listeners (microphones, contact pickups). Each part is baked into a table of vibration modes; the simulation couples those modes through the joints and runs sample by sample, live or offline.

Status: milestones 1 to 4 of the plan (physics core, offline render, real-time playback, 3D web editor, MCP server). Mesh parts, air columns and moving bodies are not built yet.

## Run

As a desktop app (macOS):

```bash
scripts/bundle-macos.sh --install
```

builds `phys.app` and copies it to /Applications. It is not signed, so the first time right-click it and choose Open. Opened this way it keeps its files (saved scenes, rendered takes) in `~/Music/phys`.

From a terminal, the same window:

```bash
cargo run --release
```

`cargo run --release -- app scene.json` opens a scene file. Closing the window quits.

In a browser instead:

```bash
cargo run --release -- edit
```

Then open http://localhost:7878. `edit scene.json` opens (and saves to) a file; without one it starts from a wood plate and a microphone and saves to `phys-scene.json`.

In the editor:

- **Play** (H): excite a part by hand, in one of three ways chosen in the panel. Strike: click to hit it with a mallet (speed, hardness, mass). Pluck: click to pull the point aside and let go (pull distance). Bow: hold the mouse down to bow it there, let go to lift (bow speed, pressure); near one end of a string works best.
- **Select** (S): click a part, joint or listener to edit its numbers on the right; drag the gizmo to move (W) or rotate (E) a part or a microphone. With nothing selected the panel lists everything in the scene.
- **Joint** (J): click two points to tie them with a spring; click empty space second to tie the first point to the ground. Change it to a contact, or give it free play to make it rattle, in the panel.
- **Pickup** (P): click a part to put a contact pickup there.
- **Render take** replays every strike you made, offline, into `out/<scene name>.wav`.
- Parts bend with their lowest modes as they ring, slowed down and exaggerated.

The sound is made by the `phys` process on the machine's audio output, not by the browser. Edits that keep the same modes (moving a microphone, changing a joint) leave things ringing; edits that change a part restart it.

### For agents (MCP)

```bash
cargo run --release -- mcp
```

serves MCP tools on stdio and the same editor page alongside, sharing one scene and one audio engine, so an agent and a person can work on the same thing. `--headless` serves the tools only (no page, no audio). `.mcp.json` registers it for Claude Code sessions in this folder.

| Tool | What it does |
|---|---|
| `scene_summary` | Everything in the scene, warnings, and whether audio and the page are live |
| `apply_ops` | An atomic batch of upserts and deletes; one undo step |
| `describe_op` | An example and the JSON schema of each op type |
| `strike`, `excite` | Strike, pluck or bow a point by hand, or fire a named exciter, live |
| `listen` | Render offline; returns peak, decay times, strongest partials and a spectrogram |
| `verify` | Render offline and run PASS / FAIL checks (partial, decay, audible, stable) |
| `undo`, `redo`, `save`, `open`, `new_scene` | Housekeeping |
| `screenshot` | The editor page's viewport, when a page is open |

Other commands:

```bash
cargo run --release -- render examples/string_on_plate.json out/string_on_plate.wav
```

```bash
cargo run --release -- play examples/mallet_on_bar.json
```

`play` runs the scene on the default output, plays its score, then takes strikes from the keyboard (`1` Enter, `2 3.5` Enter for exciter 2 at strength 3.5, `q` to quit). It sets the level by rendering the score offline first and aiming the peak at -12 dBFS; `--gain dB` overrides that.

`render` writes a WAV normalised to -6 dBFS (`--raw` keeps the physical level) and prints the strongest partials and the decay time.

```bash
cargo test --workspace
```

## Layout

- `crates/phys_core`: scene document, parts, baking, the simulation, offline render. No audio device, no UI.
- `crates/phys_audio`: the simulation on a cpal audio thread, fed by a lock-free command queue.
- `crates/phys_app`: the `phys` command line, the editor's server (one WebSocket: ops and strikes in, the scene and meters out) and the MCP tools.
- `ui/`: the editor page. Plain ES modules and three.js 0.170 vendored in `ui/vendor`; no build step. A debug build serves these files from disk, so the page can be edited live.
- `examples/`: scenes. `crates/phys_core/examples/`: the solver comparison and the capacity benchmark.

## How joints are solved

Modes are stepped with the exact update of a damped oscillator. Joints come in two kinds:

- Linear springs and their dampers are solved implicitly through a small matrix inverted at compile time. They are stable at any stiffness.
- Contacts, rattling springs and cubic springs share one auxiliary variable for their stored energy, after Ducceschi, Bilbao and Webb, "Real-Time Modal Synthesis of Nonlinearly Interconnected Networks" (DAFx 2023). The update is explicit and the discrete energy cannot grow.

Two things here go beyond that paper and are this project's own: the gauge (the energy offset under the square root) is reset to the energy in the scene each time a contact engages, and any energy an engagement over-returns is taken back by scaling the state. The first restores second-order accuracy for Hertz contacts; the second keeps the total from ever rising.

A pluck is a force that swells as a finger pulls the point aside, then lets go in under a millisecond. A bow is friction against a moving surface, gripping at low relative speed and letting go as the part slips; each step it is treated as a damper on the relative speed with its coefficient frozen, after Russo, Ducceschi and Bilbao, "Efficient Simulation of the Bowed String in Modal Form" (DAFx 2022). That keeps the update explicit: each bow adds one rank-one term, solved together with the auxiliary variable's in a small system.

The `explicit` solver (`"settings": {"solver": "explicit"}`) applies joint forces as they stand with automatic oversampling, as sounder's rig does. It is kept as a reference.

### Solver comparison

`cargo run --release -p phys_core --example solvers`. Error is the RMS difference from the explicit solver at 64x over the first 30 ms of a strike. Energy growth is the largest single-step rise over 2 s.

| Scene | Solver | Error at 1x | at 2x | at 4x | Energy growth |
|---|---|---|---|---|---|
| Hard mallet on a bar | sav | 6.0% | 1.2% | 0.3% | none |
| | explicit | 9.5% | 1.8% | 0.4% | 12% of the starting energy in one step at 1x (its energy measure is only approximate) |
| String tied to a plate, 5e6 N/m | sav | 13.9% | 3.2% | 0.8% | none |
| | explicit | blows up | blows up | 0.7% | n/a |

`sav` is the default, at 1x. Raise `settings.oversample` for hard contacts where the strike's exact spectrum matters.

### Capacity

`cargo run --release -p phys_core --example capacity`, one core of this machine, 48 kHz, 1x, with one rattling joint and one linear joint:

| Modes | Times real time |
|---|---|
| 250 | 16.8 |
| 500 | 7.6 |
| 1000 | 3.6 |
| 2000 | 1.7 |
| 4000 | 0.8 |

The state is double precision and nothing is hand-vectorised yet.

## Known limits

- The desktop window has no File menu: a scene is opened by passing its path, and Save writes to that path (or `phys-scene.json`).
- The MCP server does not open the desktop window; it serves the page for a browser.

- Plates are simply supported; a free-edge plate is not modelled.
- Parts do not move as rigid bodies: a bar rings but does not bounce or fall.
- A microphone hears each part through a rough radiation weight per mode, not a real radiation model. Contact pickups are exact for the model.
- An edit that changes a part's modes crossfades to a silent new simulation.
- The editor sets its output level by test-striking every part after each edit, so making a part louder or quieter by editing it is evened out.
- Strike speed comes from the panel, not from how fast you drag, and a bow stays where you first pressed.
- There is one roaming bow: two parts cannot be bowed by hand at once (named bow exciters can overlap).
- The vibration display is scaled to the loudest part, so a quiet part next to a loud one barely moves.
- Parts cannot be renamed or resized with handles in the editor; sizes are typed in.
- A spring near the end of a string raises its pitch (a few percent in `string_on_plate`), as it would physically.
