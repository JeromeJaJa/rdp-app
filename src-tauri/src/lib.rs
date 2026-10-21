mod api;
mod bulk;
mod certs;
#[cfg(windows)]
mod clipboard;
mod companion;
mod core;
mod dpapi;
#[cfg(windows)]
mod frames;
mod keys;
#[cfg(windows)]
mod material;
mod profile;
mod store;
mod tearoff;

use std::sync::Arc;
use std::time::Duration;

use rdp_engine::{Engine, EngineEvent, EngineSettings, InputEvent, Viewport};
use serde_json::{Map, Value};
use tauri::{Emitter as _, Manager as _, State};

use crate::core::{ApiInfo, Core, ImportResult, Origin, ReplaceResult, UiState};
use store::{
    Connection, ConnectionPatch, ExportOptions, GroupInput, ImportOptions, ImportPreview, ImportSource, Library,
    NewConnection, OrderEntry, Store,
};

struct AppState {
    core: Arc<Core>,
    #[cfg(windows)]
    surfaces: Arc<frames::WebviewSurfaces>,
    tearoff: tearoff::Tearoff,
}

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

// ── Library ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn load_library(state: State<'_, AppState>) -> Library {
    state.core.library()
}

#[tauri::command]
fn add_connections(state: State<'_, AppState>, items: Vec<NewConnection>) -> CmdResult<Vec<Connection>> {
    state.core.add_connections(items, Origin::Ui).map_err(err)
}

#[tauri::command]
fn update_connection(state: State<'_, AppState>, id: String, patch: ConnectionPatch) -> CmdResult<Connection> {
    state.core.update_connection(&id, patch, Origin::Ui).map_err(err)
}

#[tauri::command]
fn set_connection_settings(
    state: State<'_, AppState>,
    id: String,
    settings: serde_json::Map<String, Value>,
) -> CmdResult<bool> {
    state
        .core
        .set_connection_settings(&id, settings, Origin::Ui)
        .map_err(err)
}

#[tauri::command]
fn set_group_settings(
    state: State<'_, AppState>,
    id: String,
    settings: serde_json::Map<String, Value>,
) -> CmdResult<store::Group> {
    state.core.set_group_settings(&id, settings, Origin::Ui).map_err(err)
}

#[tauri::command]
fn delete_connections(state: State<'_, AppState>, ids: Vec<String>) -> CmdResult<()> {
    state.core.delete_connections(&ids, Origin::Ui).map_err(err)
}

#[tauri::command]
fn reorder_connections(state: State<'_, AppState>, order: Vec<OrderEntry>) -> CmdResult<()> {
    state.core.reorder(order, Origin::Ui).map_err(err)
}

#[tauri::command]
fn save_groups(state: State<'_, AppState>, groups: Vec<GroupInput>) -> CmdResult<Library> {
    state.core.save_groups(groups, Origin::Ui).map_err(err)
}

#[tauri::command]
fn replace_group(state: State<'_, AppState>, group_id: String, items: Vec<NewConnection>) -> CmdResult<ReplaceResult> {
    state.core.replace_group(&group_id, items, Origin::Ui).map_err(err)
}

#[tauri::command]
fn save_settings(state: State<'_, AppState>, settings: Map<String, Value>) -> CmdResult<()> {
    state.core.save_settings(settings, Origin::Ui).map_err(err)
}

#[tauri::command]
fn export_data(state: State<'_, AppState>, options: ExportOptions) -> CmdResult<String> {
    state.core.export(&options).map_err(err)
}

#[tauri::command]
fn preview_import(
    state: State<'_, AppState>,
    source: ImportSource,
    options: ImportOptions,
) -> CmdResult<ImportPreview> {
    state.core.preview_import(&source, &options).map_err(err)
}

#[tauri::command]
fn apply_import(state: State<'_, AppState>, source: ImportSource, options: ImportOptions) -> CmdResult<ImportResult> {
    state.core.apply_import(&source, &options, Origin::Ui).map_err(err)
}

/// Reads a file picked in an open dialog (imports).
#[tauri::command]
fn read_text_file(path: String) -> CmdResult<String> {
    let bytes = std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("file is too large".into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Writes a file chosen in a save dialog (exports).
#[tauri::command]
fn write_text_file(path: String, content: String) -> CmdResult<()> {
    std::fs::write(&path, content).map_err(|e| format!("{path}: {e}"))
}

// ── Sessions ────────────────────────────────────────────────────────────────

#[tauri::command]
fn connect(state: State<'_, AppState>, ids: Vec<String>) {
    state.core.connect(&ids);
}

#[tauri::command]
fn disconnect(state: State<'_, AppState>, ids: Vec<String>) {
    state.core.disconnect(&ids);
}

#[tauri::command]
fn reconnect(state: State<'_, AppState>, ids: Vec<String>) {
    state.core.reconnect(&ids);
}

#[tauri::command]
fn running_sessions(state: State<'_, AppState>) -> Vec<String> {
    state.core.engine.running()
}

/// Current state of every live session (replayed by the UI after it loads).
#[tauri::command]
fn session_snapshot(state: State<'_, AppState>) -> Vec<EngineEvent> {
    state.core.engine.snapshot()
}

/// The calling window's session area was resized.
#[tauri::command]
fn engine_set_viewport(window: tauri::Window, state: State<'_, AppState>, width: u32, height: u32, scale: u32) {
    state.tearoff.set_viewport(
        window.label(),
        Viewport {
            width: width.clamp(200, 8192) as u16,
            height: height.clamp(200, 8192) as u16,
            scale: scale.clamp(100, 500),
        },
    );
}

/// What the calling window shows: its full-size session and (main window) grid thumbnails.
#[tauri::command]
fn engine_set_visibility(
    window: tauri::Window,
    state: State<'_, AppState>,
    active: Option<String>,
    thumbnails: Vec<String>,
) {
    state.tearoff.set_visibility(window.label(), active, thumbnails);
}

// ── Windows (tabs torn out of the main window) ──────────────────────────────

#[tauri::command]
fn window_layout(state: State<'_, AppState>) -> tearoff::Layout {
    state.tearoff.layout()
}

/// Moves a tab into a window ("main" or a detached window's label).
#[tauri::command]
fn window_move_tab(state: State<'_, AppState>, id: String, target: String, index: Option<usize>) -> CmdResult<()> {
    state.tearoff.move_tab(&id, &target, index).map_err(err)
}

/// Runs a blocking tear-off step off the UI thread and the async workers (building a
/// window waits on the UI thread; a drag runs until the button is released).
async fn tearoff_blocking<T: Send + 'static>(
    app: tauri::AppHandle,
    f: impl FnOnce(&tearoff::Tearoff) -> anyhow::Result<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(move || f(&app.state::<AppState>().tearoff))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

/// A tab drag started: has a window ready in case the tab is torn off.
#[tauri::command]
async fn window_prepare(app: tauri::AppHandle) -> CmdResult<()> {
    tearoff_blocking(app, |t| t.prepare()).await
}

/// Tears a tab (held `grab_x` CSS px into its `tab_w` width) out of the calling window
/// mid-drag; its window follows the pointer until the button is released.
///
/// Replies once the window is open, not when the drag ends: by then the calling window may
/// be gone (a tab dragged into another window empties and closes its own), and a reply to a
/// closed webview is dropped (wry's "PostMessage failed … Invalid window handle").
#[tauri::command]
async fn window_tear_tab(
    app: tauri::AppHandle,
    window: tauri::Window,
    id: String,
    grab_x: f64,
    tab_w: f64,
) -> CmdResult<()> {
    let source = window.label().to_owned();
    let grab = tearoff::Grab { x: grab_x, w: tab_w };
    let label = tearoff_blocking(app.clone(), move |t| t.tear(&id, &source, grab)).await?;
    tauri::async_runtime::spawn_blocking(move || app.state::<AppState>().tearoff.follow_torn(&label, grab));
    Ok(())
}

/// Drags the calling (detached) window by its only tab, held `grab_x` CSS px into its
/// `tab_w` width. Replies at once; the drag runs until release (see `window_tear_tab`).
#[tauri::command]
fn window_drag(app: tauri::AppHandle, window: tauri::Window, grab_x: f64, tab_w: f64) {
    let label = window.label().to_owned();
    let grab = tearoff::Grab { x: grab_x, w: tab_w };
    tauri::async_runtime::spawn_blocking(move || {
        let _ = app.state::<AppState>().tearoff.drag(&label, grab);
    });
}

/// Opens a new window holding the tab, next to the calling window.
#[tauri::command]
async fn window_detach_tab(app: tauri::AppHandle, window: tauri::Window, id: String) -> CmdResult<String> {
    let source = window.label().to_owned();
    tearoff_blocking(app, move |t| t.detach(&id, &source)).await
}

/// The calling window's material: solid, or the acrylic backdrop (experimental).
#[tauri::command]
fn window_set_material(window: tauri::WebviewWindow, acrylic: bool, dark: bool) {
    #[cfg(windows)]
    material::apply(&window, acrylic, dark);
    let _ = (window, acrylic, dark);
}

/// Where the calling window shows its tabs (CSS px): `areas` are drop targets (title bar,
/// sidebar), `slots` the individual top-strip tabs in order (for the live insertion point).
#[tauri::command]
fn window_set_tab_area(
    window: tauri::Window,
    state: State<'_, AppState>,
    areas: Vec<tearoff::Rect>,
    slots: Vec<tearoff::Rect>,
) {
    state.tearoff.set_tab_area(window.label(), areas, slots);
}

#[tauri::command]
fn window_reorder_tabs(window: tauri::Window, state: State<'_, AppState>, tabs: Vec<String>) {
    state.tearoff.reorder(window.label(), tabs);
}

#[tauri::command]
fn window_focus(state: State<'_, AppState>, label: String) {
    state.tearoff.focus(&label);
}

/// Focuses the window holding the tab and selects it there.
#[tauri::command]
fn window_show_tab(state: State<'_, AppState>, id: String) {
    state.tearoff.show_tab(&id);
}

/// Opens a connection as a tab in the main window (or focuses it if already open).
#[tauri::command]
fn window_open_tab(state: State<'_, AppState>, id: String, select: bool) {
    state.tearoff.open_tab(&id, select);
}

/// Replaces the main window's open tabs (restoring them on launch).
#[tauri::command]
fn window_set_main_tabs(state: State<'_, AppState>, tabs: Vec<String>) {
    state.tearoff.set_main_tabs(tabs);
}

/// Closes a connection's tab (the session is disconnected separately).
#[tauri::command]
fn window_close_tab(state: State<'_, AppState>, id: String) {
    state.tearoff.close_tab(&id);
}

/// Installs the companion on a connection's host over WinRM (experimental, manual trigger).
#[tauri::command]
async fn companion_install(state: State<'_, AppState>, id: String) -> CmdResult<String> {
    let core = std::sync::Arc::clone(&state.core);
    tauri::async_runtime::spawn_blocking(move || core.install_companion(&id))
        .await
        .map_err(|e| e.to_string())?
        .map(|outcome| format!("{outcome:?}"))
        .map_err(err)
}

#[tauri::command]
async fn companion_uninstall(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let core = std::sync::Arc::clone(&state.core);
    tauri::async_runtime::spawn_blocking(move || core.uninstall_companion(&id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}

/// Adds a host to the WinRM client's TrustedHosts (raises a UAC prompt), then retries the
/// companion install for that connection.
#[tauri::command]
async fn companion_trust_host(state: State<'_, AppState>, id: String, host: String) -> CmdResult<String> {
    let core = std::sync::Arc::clone(&state.core);
    tauri::async_runtime::spawn_blocking(move || {
        core.trust_host(&host)?;
        core.install_companion(&id).map(|outcome| format!("{outcome:?}"))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(err)
}

#[tauri::command]
fn engine_set_thumbnail_width(state: State<'_, AppState>, width: u32) {
    let current = state.core.engine.settings();
    // Bucket to 64px steps so slider drags don't reallocate every surface on each pixel.
    let width = width.clamp(160, 1920).div_ceil(64) * 64;
    if current.thumbnail_width != width {
        state.core.engine.update_settings(EngineSettings {
            thumbnail_width: width,
            ..current
        });
    }
}

#[tauri::command]
fn engine_input(state: State<'_, AppState>, id: String, events: Vec<InputEvent>) {
    state.core.engine.input(&id, events);
}

/// Sends a key combination (scancodes pressed in order, released in reverse).
#[tauri::command]
fn engine_send_keys(state: State<'_, AppState>, id: String, scancodes: Vec<u16>) {
    let mut events: Vec<InputEvent> = scancodes
        .iter()
        .map(|&code| InputEvent::Key { code, down: true })
        .collect();
    events.extend(
        scancodes
            .iter()
            .rev()
            .map(|&code| InputEvent::Key { code, down: false }),
    );
    state.core.engine.input(&id, events);
}

#[tauri::command]
fn engine_ctrl_alt_del(state: State<'_, AppState>, id: String) {
    state.core.engine.ctrl_alt_del(&id);
}

#[tauri::command]
fn engine_repost_surfaces(window: tauri::Window, state: State<'_, AppState>) -> CmdResult<()> {
    #[cfg(windows)]
    state.surfaces.repost_all(window.label()).map_err(err)?;
    let _ = (state, window);
    Ok(())
}

// ── Certificates, API, UI ───────────────────────────────────────────────────

#[tauri::command]
fn trust_certificate(state: State<'_, AppState>, endpoint: String, fingerprint: String) {
    state.core.trust_certificate(&endpoint, &fingerprint);
}

#[tauri::command]
fn list_certificates(state: State<'_, AppState>) -> Vec<certs::TrustedCertificate> {
    state.core.certs.list()
}

#[tauri::command]
fn forget_certificate(state: State<'_, AppState>, endpoint: String) {
    state.core.certs.forget(&endpoint);
}

#[tauri::command]
fn api_info(state: State<'_, AppState>) -> ApiInfo {
    state.core.api_info()
}

#[tauri::command]
fn regenerate_api_token(state: State<'_, AppState>) -> CmdResult<ApiInfo> {
    state.core.regenerate_api_token().map_err(err)
}

/// Opens an http(s) link in the default browser (the WebView cannot open windows).
#[tauri::command]
fn open_url(url: String) -> CmdResult<()> {
    let ok = (url.starts_with("http://") || url.starts_with("https://"))
        && !url.chars().any(|c| c.is_whitespace() || c == '"' || c.is_control());
    if !ok {
        return Err("only http(s) links can be opened".into());
    }
    #[cfg(windows)]
    std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", &url])
        .spawn()
        .map_err(|e| format!("open browser: {e}"))?;
    Ok(())
}

/// What the window shows (answers GET /api/v1/ui).
#[tauri::command]
fn report_ui_state(app_state: State<'_, AppState>, state: UiState) {
    app_state.core.set_ui_state(state);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// Logs to the console and to `<app log dir>/rdp-app.log` (rotated at startup), so
/// problems in release builds can be looked at afterwards.
fn init_logging(log_dir: Option<std::path::PathBuf>) {
    use tracing_subscriber::prelude::*;

    let filter = || {
        tracing_subscriber::EnvFilter::try_from_env("RDP_LOG")
            .unwrap_or_else(|_| "warn,rdp_engine=info,rdp_app_lib=info,rdp_clipboard=info".into())
    };
    let file = log_dir.and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("rdp-app.log");
        if std::fs::metadata(&path).is_ok_and(|m| m.len() > 10 * 1024 * 1024) {
            let _ = std::fs::rename(&path, dir.join("rdp-app.old.log"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
    });
    let file_layer = file.map(|f| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(f))
            .with_filter(filter())
    });
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().compact().with_filter(filter()))
        .with(file_layer)
        .try_init();
}

pub fn run() {
    tauri::Builder::default()
        // A second launch focuses the running window instead of starting a new app.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| match event {
            // Closing the main window quits, taking any detached windows with it.
            tauri::WindowEvent::CloseRequested { .. } if window.label() == frames::MAIN_WINDOW => {
                window.app_handle().exit(0);
            }
            // A detached window closed: its tabs go back to the main window. Done off the
            // event loop's dispatch of this event: it talks to webviews and emits events.
            tauri::WindowEvent::Destroyed if window.label().starts_with(tearoff::PREFIX) => {
                let app = window.app_handle().clone();
                let label = window.label().to_owned();
                std::thread::spawn(move || {
                    if let Some(state) = app.try_state::<AppState>() {
                        state.tearoff.window_closed(&label);
                    }
                });
            }
            _ => {}
        })
        .setup(|app| {
            init_logging(app.path().app_log_dir().ok());
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");
            let dir = app.path().app_data_dir()?;
            let store = Store::open(&dir)?;
            let certs = Arc::new(certs::CertStore::open(&dir));

            let (bus, _) = tokio::sync::broadcast::channel::<Value>(1024);
            let handle = app.handle().clone();
            let sink_bus = bus.clone();
            let events: rdp_engine::EventSink = Arc::new(move |event: EngineEvent| {
                let _ = handle.emit("session-event", &event);
                if let Ok(v) = serde_json::to_value(&event) {
                    let _ = sink_bus.send(v);
                }
                // Drive the companion auto-install off the same event stream.
                if let Some(state) = handle.try_state::<AppState>() {
                    state.core.on_engine_event(&event);
                }
            });

            #[cfg(windows)]
            let surfaces = frames::WebviewSurfaces::new(app.handle().clone());
            #[cfg(windows)]
            let provider: Arc<dyn rdp_engine::SurfaceProvider> = surfaces.clone();
            #[cfg(not(windows))]
            let provider: Arc<dyn rdp_engine::SurfaceProvider> = Arc::new(rdp_engine::surface::HeapProvider);

            let engine = Engine::new(provider, events, EngineSettings::default());
            engine.set_certificate_store(Some(certs.clone() as Arc<dyn rdp_engine::CertificateStore>));
            let core = Arc::new(Core::new(store, engine, app.handle().clone(), certs, bus));
            core.apply_settings();

            let tearoff = tearoff::Tearoff::new(
                app.handle().clone(),
                core.engine.clone(),
                #[cfg(windows)]
                surfaces.clone(),
            );
            app.manage(AppState {
                core,
                #[cfg(windows)]
                surfaces,
                tearoff,
            });

            #[cfg(windows)]
            if let Some(window) = app.get_webview_window(frames::MAIN_WINDOW) {
                configure_webview(&window);
            }

            // The window starts hidden to avoid a white flash; the UI shows it after
            // its first paint. Fall back in case the UI fails to load.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(4));
                if let Some(w) = handle.get_webview_window("main")
                    && !w.is_visible().unwrap_or(true)
                {
                    let _ = w.show();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_library,
            add_connections,
            update_connection,
            set_connection_settings,
            set_group_settings,
            delete_connections,
            reorder_connections,
            save_groups,
            replace_group,
            save_settings,
            export_data,
            preview_import,
            apply_import,
            read_text_file,
            write_text_file,
            connect,
            disconnect,
            reconnect,
            running_sessions,
            session_snapshot,
            engine_set_viewport,
            engine_set_visibility,
            engine_set_thumbnail_width,
            engine_input,
            engine_ctrl_alt_del,
            engine_repost_surfaces,
            window_layout,
            window_move_tab,
            window_prepare,
            window_tear_tab,
            window_drag,
            window_detach_tab,
            window_set_tab_area,
            window_set_material,
            window_reorder_tabs,
            window_focus,
            window_show_tab,
            window_open_tab,
            window_set_main_tabs,
            window_close_tab,
            companion_install,
            companion_uninstall,
            companion_trust_host,
            engine_send_keys,
            trust_certificate,
            list_certificates,
            forget_certificate,
            api_info,
            regenerate_api_token,
            report_ui_state,
            open_url,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event
                && let Some(state) = app.try_state::<AppState>()
            {
                state.core.engine.disconnect_all();
                // Let sessions close their sockets cleanly.
                std::thread::sleep(Duration::from_millis(250));
            }
        });
}

/// Browser accelerators (F5 reload, Ctrl+P print, Ctrl+F find, …) make no sense in
/// a desktop app and would steal keystrokes meant for remote sessions.
#[cfg(windows)]
pub(crate) fn configure_webview(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
    use windows_core::Interface as _;

    if cfg!(debug_assertions) {
        return; // keep reload/devtools while developing
    }
    let _ = window.with_webview(|pw| unsafe {
        if let Ok(core) = pw.controller().CoreWebView2()
            && let Ok(settings) = core.Settings()
        {
            if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
                let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
            }
            let _ = settings.SetAreDefaultContextMenusEnabled(false);
        }
    });
}
