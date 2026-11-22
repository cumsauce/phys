//! MIDI input: every connected device plays the scene's play points.
//!
//! The editor page cannot do this itself (the app's web view has no MIDI
//! support), so the notes are read here and go straight to the engine.

use std::collections::HashMap;
use std::time::Duration;

use midir::{MidiInput, MidiInputConnection};

use crate::edit::Shared;

fn handle(shared: &Shared, message: &[u8]) {
    let [status, note, velocity, ..] = *message else {
        return;
    };
    let Ok(mut app) = shared.lock() else { return };
    match status & 0xf0 {
        0x90 if velocity > 0 => app.note_on(note, velocity),
        0x80 | 0x90 => app.note_off(note),
        _ => {}
    }
}

/// Wait between scans. On macOS the wait runs this thread's run loop,
/// because that is where CoreMIDI tells the process that devices came or
/// went: without it, a device plugged in after launch is never seen.
fn pause(time: Duration) {
    #[cfg(target_os = "macos")]
    // SAFETY: runs the calling thread's own run loop for a bounded time.
    unsafe {
        use core_foundation_sys::runloop::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, time.as_secs_f64(), 0);
    }
    #[cfg(not(target_os = "macos"))]
    std::thread::sleep(time);
}

/// Watch for MIDI inputs coming and going, and listen to all of them.
pub(crate) fn start(shared: Shared) {
    let spawned = std::thread::Builder::new()
        .name("phys-midi".into())
        .spawn(move || {
            // Keyed by the port's id, not its name: a device that is unplugged
            // and plugged back in keeps its name but is a new port.
            let mut open: HashMap<String, (String, MidiInputConnection<()>)> = HashMap::new();
            loop {
                let Ok(scan) = MidiInput::new("phys") else {
                    return;
                };
                let ports = scan.ports();
                let ids: Vec<String> = ports.iter().map(|p| p.id()).collect();
                let before = open.len();
                open.retain(|id, _| ids.contains(id));
                let mut changed = open.len() != before;
                for (port, id) in ports.iter().zip(&ids) {
                    if open.contains_key(id) {
                        continue;
                    }
                    let name = scan.port_name(port).unwrap_or_else(|_| "MIDI input".into());
                    // Connecting consumes the client, so each input gets its own.
                    let Ok(client) = MidiInput::new("phys") else {
                        continue;
                    };
                    let target = shared.clone();
                    if let Ok(conn) = client.connect(
                        port,
                        "phys-in",
                        move |_, message, _| handle(&target, message),
                        (),
                    ) {
                        open.insert(id.clone(), (name, conn));
                        changed = true;
                    }
                }
                if changed && let Ok(mut app) = shared.lock() {
                    let mut list: Vec<String> =
                        open.values().map(|(name, _)| name.clone()).collect();
                    list.sort();
                    app.set_midi(list);
                }
                pause(Duration::from_millis(1500));
            }
        });
    if let Err(e) = spawned {
        eprintln!("phys: MIDI: {e}");
    }
}

/// Play one note on a virtual output, so a running phys hears it.
pub(crate) fn send_test(note: u8, velocity: u8) -> Result<(), String> {
    #[cfg(unix)]
    {
        use midir::os::unix::VirtualOutput;
        let out = midir::MidiOutput::new("phys test").map_err(|e| e.to_string())?;
        let mut conn = out.create_virtual("phys test").map_err(|e| e.to_string())?;
        // Long enough for a running phys to notice the new port.
        std::thread::sleep(Duration::from_secs(4));
        conn.send(&[0x90, note & 0x7f, velocity & 0x7f])
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(400));
        conn.send(&[0x80, note & 0x7f, 0])
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(600));
        println!("sent note {note} at velocity {velocity}");
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (note, velocity);
        Err("midi-send needs virtual MIDI ports, which this platform lacks".into())
    }
}
