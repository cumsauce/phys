//! The desktop window: the editor page in a native web view, with the
//! server running inside the same process. Closing the window quits.
//!
//! File dialogs live here because only the main thread may show them: the
//! File menu and the page's own file buttons both end up in `file_action`.

use std::path::PathBuf;

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem as Item, Submenu};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::edit::{self, FileAction, Shared, UiEvent};

enum User {
    Ui(UiEvent),
    Menu(muda::MenuId),
}

/// A double-clicked app starts in `/`, where nothing can be saved. Work in
/// `~/Music/phys` instead, so Save and Render take have somewhere to write.
fn settle_working_directory() {
    let at_root = std::env::current_dir().is_ok_and(|d| d.parent().is_none());
    if !at_root {
        return;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let dir = PathBuf::from(home).join("Music").join("phys");
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::env::set_current_dir(&dir);
        }
    }
}

struct Menus {
    menu: Menu,
    new: MenuItem,
    open: MenuItem,
    save: MenuItem,
    save_as: MenuItem,
}

/// The menus. Without an Edit menu macOS gives a web view no copy, paste or
/// select-all shortcuts, and without an app menu no Cmd+Q.
fn menus() -> Result<Menus, String> {
    let err = |e: muda::Error| e.to_string();
    let key = |mods: Modifiers, code: Code| Some(Accelerator::new(Some(mods), code));
    let cmd = Modifiers::SUPER;
    let new = MenuItem::new("New", true, key(cmd, Code::KeyN));
    let open = MenuItem::new("Open…", true, key(cmd, Code::KeyO));
    let save = MenuItem::new("Save", true, key(cmd, Code::KeyS));
    let save_as = MenuItem::new("Save As…", true, key(cmd | Modifiers::SHIFT, Code::KeyS));
    let menu = Menu::new();
    let app = Submenu::with_items(
        "phys",
        true,
        &[
            &Item::about(Some("About phys"), None),
            &Item::separator(),
            &Item::hide(None),
            &Item::hide_others(None),
            &Item::separator(),
            &Item::quit(None),
        ],
    )
    .map_err(err)?;
    let file = Submenu::with_items(
        "File",
        true,
        &[&new, &open, &Item::separator(), &save, &save_as],
    )
    .map_err(err)?;
    let edit = Submenu::with_items(
        "Edit",
        true,
        &[
            &Item::cut(None),
            &Item::copy(None),
            &Item::paste(None),
            &Item::select_all(None),
        ],
    )
    .map_err(err)?;
    let window = Submenu::with_items(
        "Window",
        true,
        &[
            &Item::minimize(None),
            &Item::fullscreen(None),
            &Item::close_window(None),
        ],
    )
    .map_err(err)?;
    menu.append_items(&[&app, &file, &edit, &window])
        .map_err(err)?;
    Ok(Menus {
        menu,
        new,
        open,
        save,
        save_as,
    })
}

fn confirm(title: &str, text: &str) -> bool {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(title)
        .set_description(text)
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show()
        == rfd::MessageDialogResult::Ok
}

fn scene_dialog() -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new().add_filter("phys scene", &["json"]);
    match std::env::current_dir() {
        Ok(dir) => dialog.set_directory(dir),
        Err(_) => dialog,
    }
}

/// Carry out a file action, asking with dialogs for whatever it needs. The
/// lock is never held while a dialog is up.
fn file_action(shared: &Shared, action: FileAction) {
    let state = shared
        .lock()
        .map(|a| (a.dirty(), a.path.clone(), a.doc.scene().name.clone()));
    let Ok((dirty, path, name)) = state else {
        return;
    };
    let discard = || {
        !dirty
            || confirm(
                "Unsaved changes",
                "The scene has changes that are not saved. Discard them?",
            )
    };
    let result = match action {
        FileAction::New => discard().then(|| shared.lock().map(|mut a| a.new_scene())),
        FileAction::Open => (discard())
            .then(|| scene_dialog().pick_file())
            .flatten()
            .map(|p| shared.lock().map(|mut a| a.open_path(&p))),
        FileAction::Save if path.is_some() => Some(shared.lock().map(|mut a| a.save())),
        FileAction::Save | FileAction::SaveAs => {
            let suggested = path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| {
                    format!("{}.json", if name.is_empty() { "untitled" } else { &name })
                });
            scene_dialog()
                .set_file_name(suggested)
                .save_file()
                .map(|p| shared.lock().map(|mut a| a.save_to(p)))
        }
    };
    // The page shows the outcome in its status line.
    let message = match result {
        Some(Ok(Ok(note))) => serde_json::json!({"type": "note", "message": note}),
        Some(Ok(Err(e))) => {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("phys")
                .set_description(&e)
                .show();
            serde_json::json!({"type": "error", "message": e})
        }
        _ => return,
    };
    if let Ok(a) = shared.lock() {
        let _ = a.events.send(message.to_string());
    }
}

pub fn run(path: Option<PathBuf>, audio: bool) -> Result<(), String> {
    settle_working_directory();
    let shared = edit::build(path, audio)?;
    // Any free port: the window is the only thing that needs to find it.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let served = shared.clone();
    std::thread::Builder::new()
        .name("phys-server".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => return eprintln!("phys: server: {e}"),
            };
            if let Err(e) = runtime.block_on(edit::serve_on(served, listener)) {
                eprintln!("phys: server: {e}");
            }
        })
        .map_err(|e| e.to_string())?;

    // The window owns the main thread from here on, as macOS requires.
    let event_loop = EventLoopBuilder::<User>::with_user_event().build();
    let title = shared.lock().map(|a| a.title()).unwrap_or_default();
    let window = WindowBuilder::new()
        .with_title(format!("phys — {title}"))
        .with_inner_size(LogicalSize::new(1360.0, 860.0))
        .with_min_inner_size(LogicalSize::new(820.0, 520.0))
        .build(&event_loop)
        .map_err(|e| e.to_string())?;
    let menus = menus()?;
    #[cfg(target_os = "macos")]
    menus.menu.init_for_nsapp();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = proxy.send_event(User::Menu(e.id));
    }));
    let proxy = event_loop.create_proxy();
    if let Ok(mut a) = shared.lock() {
        a.ui = Some(Box::new(move |e| {
            let _ = proxy.send_event(User::Ui(e));
        }));
    }
    let webview = WebViewBuilder::new()
        .with_devtools(cfg!(debug_assertions))
        .with_url(format!("http://127.0.0.1:{port}/"))
        .build(&window)
        .map_err(|e| e.to_string())?;
    eprintln!("phys: window open (also at http://127.0.0.1:{port})");
    event_loop.run(move |event, _, flow| {
        // Keep the web view alive for as long as the loop runs.
        let _ = &webview;
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(User::Ui(UiEvent::Title(t))) => {
                window.set_title(&format!("phys — {t}"))
            }
            Event::UserEvent(User::Ui(UiEvent::File(action))) => file_action(&shared, action),
            Event::UserEvent(User::Menu(id)) => {
                let action = [
                    (&menus.new, FileAction::New),
                    (&menus.open, FileAction::Open),
                    (&menus.save, FileAction::Save),
                    (&menus.save_as, FileAction::SaveAs),
                ]
                .into_iter()
                .find(|(item, _)| *item.id() == id);
                if let Some((_, action)) = action {
                    file_action(&shared, action);
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let dirty = shared.lock().is_ok_and(|a| a.dirty());
                if !dirty
                    || confirm(
                        "Unsaved changes",
                        "The scene has changes that are not saved. Quit anyway?",
                    )
                {
                    *flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    })
}
