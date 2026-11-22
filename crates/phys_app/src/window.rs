//! The desktop window: the editor page in a native web view, with the
//! server running inside the same process. Closing the window quits.

use std::path::PathBuf;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

use crate::edit;

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

/// The standard menus. Without an Edit menu macOS gives a web view no
/// copy, paste or select-all shortcuts, and without an app menu no Cmd+Q.
fn menus() -> Result<muda::Menu, String> {
    use muda::{Menu, PredefinedMenuItem as Item, Submenu};
    let err = |e: muda::Error| e.to_string();
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
    menu.append_items(&[&app, &edit, &window]).map_err(err)?;
    Ok(menu)
}

pub fn run(path: Option<PathBuf>, audio: bool) -> Result<(), String> {
    settle_working_directory();
    let shared = edit::build(path, audio)?;
    // Any free port: the window is the only thing that needs to find it.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
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
            if let Err(e) = runtime.block_on(edit::serve_on(shared, listener)) {
                eprintln!("phys: server: {e}");
            }
        })
        .map_err(|e| e.to_string())?;

    // The window owns the main thread from here on, as macOS requires.
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("phys")
        .with_inner_size(LogicalSize::new(1360.0, 860.0))
        .with_min_inner_size(LogicalSize::new(820.0, 520.0))
        .build(&event_loop)
        .map_err(|e| e.to_string())?;
    let menu = menus()?;
    #[cfg(target_os = "macos")]
    menu.init_for_nsapp();
    let _webview = WebViewBuilder::new()
        .with_devtools(cfg!(debug_assertions))
        .with_url(format!("http://127.0.0.1:{port}/"))
        .build(&window)
        .map_err(|e| e.to_string())?;
    eprintln!("phys: window open (also at http://127.0.0.1:{port})");
    event_loop.run(move |event, _, flow| {
        // Keep the menu and the web view alive for as long as the loop runs.
        let _ = (&menu, &_webview);
        *flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *flow = ControlFlow::Exit;
        }
    })
}
