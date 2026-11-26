//! CLASSETS app shell: wraps the `classets-core` engine in Tauri commands, runs the sync
//! loop in the background, and (on desktop) lives in the system tray.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use classets_core::api::{Browse, Folder, Me, PendingPairing};
use classets_core::{Engine, EngineConfig, EngineError, EngineStatus, PairStart, PairStatus, SyncReport};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State};
use tokio::sync::{oneshot, Mutex};

const MAX_LOG: usize = 50;

#[derive(Default)]
struct AppState {
    engine: Mutex<Option<Arc<Engine>>>,
    stop: Mutex<Option<oneshot::Sender<()>>>,
    paused: AtomicBool,
    me: Mutex<Option<Me>>,
    log: Mutex<Vec<LogEntry>>,
    data_dir: std::sync::OnceLock<PathBuf>,
}

#[derive(Clone, Serialize)]
struct LogEntry {
    at: String,
    text: String,
    ok: bool,
}

#[derive(Serialize)]
struct UiState {
    configured: bool,
    paired: bool,
    syncing: bool,
    paused: bool,
    os: &'static str,
    mobile: bool,
    server_url: Option<String>,
    device_name: Option<String>,
    sync_root: Option<String>,
    default_device_name: String,
    default_sync_root: String,
    status: Option<EngineStatus>,
    me: Option<Me>,
    log: Vec<LogEntry>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

fn is_mobile() -> bool {
    cfg!(any(target_os = "ios", target_os = "android"))
}

fn data_dir(state: &AppState) -> PathBuf {
    state.data_dir.get().cloned().unwrap_or_else(|| PathBuf::from("."))
}

fn make_engine(config: EngineConfig) -> Result<Engine, EngineError> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let store = classets_core::KeyringStore::new(format!("app.classets.desktop:{}", config.data_dir.display()));
        return Engine::with_secret_store(config, Box::new(store));
    }
    #[allow(unreachable_code)]
    Engine::new(config)
}

async fn push_log(state: &AppState, text: String, ok: bool) {
    let mut log = state.log.lock().await;
    log.insert(0, LogEntry { at: chrono::Local::now().to_rfc3339(), text, ok });
    log.truncate(MAX_LOG);
}

fn describe(r: &SyncReport) -> String {
    let mut parts = vec![];
    if r.uploaded > 0 { parts.push(format!("{} uploaded", r.uploaded)); }
    if r.downloaded > 0 { parts.push(format!("{} downloaded", r.downloaded)); }
    if r.deleted > 0 { parts.push(format!("{} deleted", r.deleted)); }
    if r.conflicts > 0 { parts.push(format!("{} conflict copies kept", r.conflicts)); }
    if !r.errors.is_empty() { parts.push(format!("{} problems", r.errors.len())); }
    if parts.is_empty() { "Up to date".into() } else { parts.join(", ") }
}

/// Start the background sync loop (desktop only; phones never keep folders in sync).
async fn start_loop(app: &AppHandle, engine: Arc<Engine>) {
    if is_mobile() {
        return;
    }
    let state = app.state::<AppState>();
    if state.paused.load(Ordering::SeqCst) {
        return;
    }
    let mut stop = state.stop.lock().await;
    if stop.is_some() {
        return;
    }
    let (tx, rx) = oneshot::channel::<()>();
    *stop = Some(tx);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let shutdown = async move {
            let _ = rx.await;
        };
        let result = engine.run_loop(shutdown).await;
        let state = app.state::<AppState>();
        *state.stop.lock().await = None;
        if let Err(e) = result {
            let text = match &e {
                EngineError::Revoked { wipe: true } => "This device was revoked. Its synced files were removed.".to_string(),
                EngineError::Revoked { .. } => "This device was revoked on the web.".to_string(),
                other => format!("Sync stopped: {other}"),
            };
            push_log(&state, text, false).await;
        }
    });
}

async fn stop_loop(state: &AppState) {
    if let Some(tx) = state.stop.lock().await.take() {
        let _ = tx.send(());
    }
}

async fn engine(state: &AppState) -> Result<Arc<Engine>, String> {
    state.engine.lock().await.clone().ok_or_else(|| "Not set up yet".to_string())
}

#[tauri::command]
async fn app_state(state: State<'_, AppState>, app: AppHandle) -> Result<UiState, String> {
    let eng = state.engine.lock().await.clone();
    let home = app.path().home_dir().map_err(err)?;
    let default_root = if is_mobile() { data_dir(&state).join("files") } else { home.join("CLASSETS") };
    let host = gethostname::gethostname().to_string_lossy().trim_end_matches(".local").to_string();
    Ok(UiState {
        configured: eng.is_some(),
        paired: eng.as_ref().map(|e| e.is_paired()).unwrap_or(false),
        syncing: state.stop.lock().await.is_some(),
        paused: state.paused.load(Ordering::SeqCst),
        os: std::env::consts::OS,
        mobile: is_mobile(),
        server_url: eng.as_ref().map(|e| e.config().server_url.clone()),
        device_name: eng.as_ref().map(|e| e.config().device_name.clone()),
        sync_root: eng.as_ref().map(|e| e.config().sync_root.display().to_string()),
        default_device_name: if is_mobile() { (if cfg!(target_os = "ios") { "iPhone" } else { "Android phone" }).to_string() } else { host },
        default_sync_root: default_root.display().to_string(),
        status: eng.as_ref().map(|e| e.status()),
        me: state.me.lock().await.clone(),
        log: state.log.lock().await.clone(),
    })
}

#[tauri::command]
async fn configure(state: State<'_, AppState>, server_url: String, device_name: String, sync_root: String) -> Result<(), String> {
    let server_url = server_url.trim().trim_end_matches('/').to_string();
    if !(server_url.starts_with("https://") || server_url.starts_with("http://")) {
        return Err("The server address must start with https://".into());
    }
    let root = PathBuf::from(sync_root.trim());
    std::fs::create_dir_all(&root).map_err(|e| format!("Can't use that folder: {e}"))?;
    let config = EngineConfig { server_url, sync_root: root, data_dir: data_dir(&state), device_name: device_name.trim().to_string() };
    config.save().map_err(err)?;
    stop_loop(&state).await;
    *state.engine.lock().await = Some(Arc::new(make_engine(config).map_err(err)?));
    Ok(())
}

#[tauri::command]
async fn pair_start(state: State<'_, AppState>) -> Result<PairStart, String> {
    engine(&state).await?.pair_start().await.map_err(err)
}

/// Returns "pending", "approved" or the reason it ended.
#[tauri::command]
async fn pair_poll(state: State<'_, AppState>, app: AppHandle) -> Result<String, String> {
    let eng = engine(&state).await?;
    match eng.pair_poll().await.map_err(err)? {
        PairStatus::Pending => Ok("pending".into()),
        PairStatus::Approved { .. } => {
            if let Ok(api) = eng.api() {
                *state.me.lock().await = api.me().await.ok();
            }
            push_log(&state, "This device was approved.".into(), true).await;
            start_loop(&app, eng).await;
            Ok("approved".into())
        }
        PairStatus::Expired { reason } => Ok(reason),
    }
}

#[tauri::command]
async fn sync_now(state: State<'_, AppState>) -> Result<SyncReport, String> {
    let eng = engine(&state).await?;
    match eng.sync_once().await {
        Ok(r) => {
            push_log(&state, describe(&r), r.errors.is_empty()).await;
            for e in &r.errors {
                push_log(&state, e.clone(), false).await;
            }
            Ok(r)
        }
        Err(e) => {
            push_log(&state, e.to_string(), false).await;
            Err(err(e))
        }
    }
}

#[tauri::command]
async fn set_paused(state: State<'_, AppState>, app: AppHandle, paused: bool) -> Result<(), String> {
    state.paused.store(paused, Ordering::SeqCst);
    if paused {
        stop_loop(&state).await;
        push_log(&state, "Syncing paused.".into(), true).await;
    } else if let Ok(eng) = engine(&state).await {
        push_log(&state, "Syncing resumed.".into(), true).await;
        start_loop(&app, eng).await;
    }
    Ok(())
}

#[tauri::command]
async fn me(state: State<'_, AppState>) -> Result<Me, String> {
    let m = engine(&state).await?.api().map_err(err)?.me().await.map_err(err)?;
    *state.me.lock().await = Some(m.clone());
    Ok(m)
}

#[tauri::command]
async fn folders(state: State<'_, AppState>) -> Result<Vec<Folder>, String> {
    engine(&state).await?.api().map_err(err)?.folders().await.map_err(err)
}

#[tauri::command]
async fn browse(state: State<'_, AppState>, path: String) -> Result<Browse, String> {
    engine(&state).await?.api().map_err(err)?.browse(&path).await.map_err(err)
}

/// A one-file link for downloading or opening a file (opened by the system browser).
#[tauri::command]
async fn download_link(state: State<'_, AppState>, version_id: i64) -> Result<String, String> {
    Ok(engine(&state).await?.api().map_err(err)?.download_url(version_id).await.map_err(err)?.url)
}

/// Upload bytes picked in the app (phone photos, files) into a folder.
#[tauri::command]
async fn upload_bytes(state: State<'_, AppState>, folder: String, name: String, bytes: Vec<u8>) -> Result<String, String> {
    use classets_core::api::{FileIntent, IntentResponse};
    let eng = engine(&state).await?;
    let api = eng.api().map_err(err)?;
    let name = name.replace(['/', '\\'], "_");
    let tmp = data_dir(&state).join(format!("upload-{}.tmp", chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)));
    tokio::fs::write(&tmp, &bytes).await.map_err(err)?;
    let sha = hex::encode(Sha256::digest(&bytes));
    let intent = FileIntent {
        path: format!("{}/{}", folder.trim_end_matches('/'), name),
        sha256: sha,
        size: bytes.len() as u64,
        modified_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        base_version_id: None,
    };
    let result = async {
        match api.file_intent(&intent).await.map_err(err)? {
            IntentResponse::Done { .. } => Ok::<_, String>(()),
            IntentResponse::Upload { upload_id, url, method, headers, .. } => {
                api.upload(&method, &url, &headers, &tmp).await.map_err(err)?;
                api.file_commit(&upload_id).await.map_err(err)?;
                Ok(())
            }
        }
    }
    .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result.map(|_| intent.path)
}

#[tauri::command]
async fn pending_pairings(state: State<'_, AppState>) -> Result<Vec<PendingPairing>, String> {
    engine(&state).await?.api().map_err(err)?.pending_pairings().await.map_err(err)
}

#[tauri::command]
async fn decide_pairing(state: State<'_, AppState>, code: String, approve: bool) -> Result<(), String> {
    let api = engine(&state).await?.api().map_err(err)?;
    if approve {
        api.approve_pairing(&code).await.map_err(err)?;
    } else {
        api.deny_pairing(&code).await.map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
async fn sign_out(state: State<'_, AppState>) -> Result<(), String> {
    stop_loop(&state).await;
    if let Some(eng) = state.engine.lock().await.take() {
        eng.sign_out().map_err(err)?;
    }
    *state.me.lock().await = None;
    let _ = std::fs::remove_file(data_dir(&state).join("config.json"));
    Ok(())
}

#[cfg(desktop)]
fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tauri::tray::TrayIconBuilder;

    let open = MenuItem::with_id(app, "open", "Open CLASSETS", true, None::<&str>)?;
    let sync = MenuItem::with_id(app, "sync", "Sync now", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "Pause / resume syncing", true, None::<&str>)?;
    let folder = MenuItem::with_id(app, "folder", "Open CLASSETS folder", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit CLASSETS", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &sync, &pause, &folder, &PredefinedMenuItem::separator(app)?, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("CLASSETS")
        .menu(&menu)
        .on_menu_event(|app, event| {
            let app = app.clone();
            match event.id.as_ref() {
                "open" => {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.show();
                        let _ = w.set_focus();
                    }
                }
                "sync" => {
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>();
                        if let Ok(eng) = engine(&state).await {
                            let r = eng.sync_once().await;
                            push_log(&state, r.map(|r| describe(&r)).unwrap_or_else(|e| e.to_string()), true).await;
                        }
                    });
                }
                "pause" => {
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>();
                        let now = !state.paused.load(Ordering::SeqCst);
                        state.paused.store(now, Ordering::SeqCst);
                        if now {
                            stop_loop(&state).await;
                        } else if let Ok(eng) = engine(&state).await {
                            start_loop(&app, eng).await;
                        }
                    });
                }
                "folder" => {
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>();
                        if let Ok(eng) = engine(&state).await {
                            use tauri_plugin_opener::OpenerExt;
                            let _ = app.opener().open_path(eng.config().sync_root.display().to_string(), None::<&str>);
                        }
                    });
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let state = app.state::<AppState>();
            let _ = state.data_dir.set(dir.clone());

            // Resume a configured device.
            if let Ok(mut config) = EngineConfig::load(&dir) {
                config.data_dir = dir.clone();
                if let Ok(eng) = make_engine(config) {
                    let eng = Arc::new(eng);
                    let paired = eng.is_paired();
                    let handle = app.handle().clone();
                    tauri::async_runtime::block_on(async {
                        *state.engine.lock().await = Some(eng.clone());
                    });
                    if paired {
                        tauri::async_runtime::spawn(async move {
                            let state = handle.state::<AppState>();
                            if let Ok(api) = eng.api() {
                                *state.me.lock().await = api.me().await.ok();
                            }
                            start_loop(&handle, eng).await;
                        });
                    }
                }
            }

            #[cfg(desktop)]
            setup_tray(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps CLASSETS syncing in the tray.
            #[cfg(desktop)]
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            #[cfg(not(desktop))]
            let _ = (window, event);
        })
        .invoke_handler(tauri::generate_handler![
            app_state,
            configure,
            pair_start,
            pair_poll,
            sync_now,
            set_paused,
            me,
            folders,
            browse,
            download_link,
            upload_bytes,
            pending_pairings,
            decide_pairing,
            sign_out
        ])
        .run(tauri::generate_context!())
        .expect("error while running the CLASSETS app");
}
