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

`cargo run --release -- scene.json` opens a scene file. Closing the window quits.

The File menu has New, Open, Save and Save As with the system file dialogs, the title bar shows the scene, its file and a `*` while there are unsaved changes, and New, Open and quitting ask before discarding them. The same four buttons are on the page; in a browser, where there is no file dialog, they ask for a path.

In a browser instead:

```bash
cargo run --release -- edit
```

Then open http://localhost:7878. `edit scene.json` opens (and saves to) a file; without one it starts from a wood plate and a microphone and saves to `phys-scene.json`.

In the editor:

**Building**

- **Add**: choose a part or a microphone, then click where it should go: the ground, or a part to sit just above it.
- **Select** (S): click to select, Shift-click to select several. Drag the gizmo to move (W) or rotate (E); moves snap to 1 cm and turns to 15 degrees unless Alt is held. Several selected parts move together.
- **Resize handles**: a selected part shows white handles at its ends, edges or rim. Dragging one resizes from that side while the far side stays put.
- **Numbers**: every field can be typed, or changed by dragging sideways on its label (Shift for fine steps).
- **Joint** (J): click two points to tie them with a spring; click empty space second to tie the first to the ground. Change it to a contact, or give it free play to make it rattle, in the panel.
- **Pickup** (P): click a part to put a contact pickup there.
- **Dragging attachments**: a joint's ends, a pickup or a play point can be dragged along its part or onto another. Points snap to the ends, the middle and simple fractions (1/2, 1/3, 1/4, ...), which are the places that decide which harmonics sound; Alt turns that off.
- **Duplicate** (Cmd+D) copies the selection with everything attached to it. **Repeat** in a part's panel makes N more at a spacing, each a number of semitones above the last, with their joints landing at shifted points on whatever they were tied to: six strings on a top is one action.
- **Right-click** a part or a spot for a menu: add a play point, pickup or joint there, duplicate, rename, delete. Deleting a part takes its attachments with it.
- **Views**: F frames the selection; 1, 3 and 7 look from the front, side and top.

**Playing**

- **Play** (H): excite a part by hand, in one of three ways chosen in the panel. In this tool the left button plays and the right button turns the view.
  - Strike: click to hit with a mallet (speed, hardness, mass), or flick: press, sweep and release, and the sweep's speed sets how hard.
  - Pluck: click to pull a point aside and let go, or drag across strings to strum them.
  - Bow: hold the mouse down to bow, slide to move the bow, scroll to press harder, let go to lift. Near one end of a string works best.
- **Play points**: a spot bound to a computer key or a MIDI note. Right-click a spot and choose "Add play point here", then press the key for it; or use **Assign keys** to give each selected part (or every string and bar) a key, left to right along a keyboard row. Keys play in the Play tool: Shift harder, Alt softer, and a bow sounds while its key is held. Key caps are drawn at the points. Play points are saved with the scene as its exciters.
- **MIDI**: every connected MIDI input is heard. In a play point's panel, Learn binds the next note you play; velocity sets how hard, and releasing the note lifts a bow.
- **Space** hushes everything.
- **Render take** replays everything you played (mouse, keys and MIDI), offline, into `out/<scene name>.wav`.
- Parts bend with their lowest modes as they ring, slowed down and exaggerated.

The sound is made by the `phys` process on the machine's audio output, not by the browser. Editing one part restarts only that part: everything else keeps ringing.

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
- `ui/`: the editor page, as plain ES modules (`app.js` wires together `viewport`, `handles`, `build`, `play`, `inspector`, `net` and `state`), with three.js 0.170 vendored in `ui/vendor`; no build step. A debug build serves these files from disk, so the page can be edited live.
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

- Double-clicking a scene file in Finder does not open it in phys; use File > Open, or pass the path on the command line.
- The MCP server does not open the desktop window; it serves the page for a browser.

- Plates are simply supported; a free-edge plate is not modelled.
- Parts do not move as rigid bodies: a bar rings but does not bounce or fall.
- A microphone hears each part through a rough radiation weight per mode, not a real radiation model. Contact pickups are exact for the model.
- The editor sets its output level by test-striking every part after each edit, so making a part louder or quieter by editing it is evened out.
- There is one roaming bow: two parts cannot be bowed by hand at once (named bow exciters can overlap).
- The vibration display is scaled to the loudest part, so a quiet part next to a loud one barely moves.
- Thickness has no handle; it is typed or dragged on its label.
- Key caps of play points that sit close together overlap.
- A spring near the end of a string raises its pitch (a few percent in `string_on_plate`), as it would physically.
