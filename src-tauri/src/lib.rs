mod inference_manager;

use columba_backend::{
    hardware_profiles::detect_hardware,
    model_manager::{download_model, model_is_present, safe_model_path, validate_model_download_url},
    model_selector::select_model_profile,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{Emitter, Manager, Runtime};

const LLAMA_PORT: u16 = 8081;
const MODEL_MANIFEST_ENV: &str = "COLUMBA_MODEL_MANIFEST";
const EMBEDDED_MODEL_MANIFEST: &str = include_str!("../../resources/model_manifest.json");

struct LlamaServerProcess(Mutex<Option<std::process::Child>>);

// ──────────────────────────────────────────────────────────────────────────────
// Tauri commands
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub url: String,
    pub size_mb: u64,
    pub present: bool,
}

#[derive(Serialize)]
pub struct SetupStatus {
    pub tier: String,
    pub ram_gb: f32,
    pub vram_gb: Option<f32>,
    pub physical_cores: usize,
    pub models: Vec<ModelEntry>,
    pub ready: bool,
}

#[tauri::command]
fn get_setup_status() -> SetupStatus {
    let hw = detect_hardware();
    let profile = select_model_profile(&hw);

    let manifest = load_manifest();
    let tier_key = match hw.tier {
        columba_backend::hardware_profiles::HardwareTier::LowEnd => "low_end",
        columba_backend::hardware_profiles::HardwareTier::MidRange => "mid_range",
        columba_backend::hardware_profiles::HardwareTier::HighEnd => "high_end",
    };

    let tier_manifest = manifest
        .get(tier_key)
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let mut models = Vec::new();
    for role in ["main", "draft"] {
        if let Some(entry) = tier_manifest.get(role).and_then(|v| v.as_object()) {
            let name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let size_mb = entry
                .get("size_mb")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let present = model_is_present(&name);
            models.push(ModelEntry { name, url, size_mb, present });
        }
    }

    // Also include profile models not in manifest (safety check)
    for model_name in [&profile.main_model, &profile.draft_model] {
        if !models.iter().any(|m| &m.name == model_name) {
            models.push(ModelEntry {
                name: model_name.clone(),
                url: String::new(),
                size_mb: 0,
                present: model_is_present(model_name),
            });
        }
    }

    let ready = models.iter().all(|m| m.present);

    SetupStatus {
        tier: tier_key.to_string(),
        ram_gb: hw.ram_gb,
        vram_gb: hw.vram_gb,
        physical_cores: hw.physical_cores,
        models,
        ready,
    }
}

#[derive(Clone, Serialize)]
struct DownloadProgress {
    name: String,
    downloaded_bytes: u64,
    total_bytes: u64,
}

#[tauri::command]
async fn download_missing_model<R: Runtime>(
    name: String,
    url: String,
    window: tauri::Window<R>,
) -> Result<(), String> {
    validate_model_download_url(&url).map_err(|e| e.to_string())?;
    let dest = safe_model_path(&name).map_err(|e| e.to_string())?;
    let win = window.clone();
    let model_name = name.clone();

    download_model(&url, &dest, move |downloaded, total| {
        let _ = win.emit(
            "download_progress",
            DownloadProgress {
                name: model_name.clone(),
                downloaded_bytes: downloaded,
                total_bytes: total,
            },
        );
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn start_llama_server(app: tauri::AppHandle) -> Result<(), String> {
    let hw = detect_hardware();
    let profile = select_model_profile(&hw);

    if !model_is_present(&profile.main_model) || !model_is_present(&profile.draft_model) {
        return Err("models not yet fully downloaded".to_string());
    }

    let exe_dir = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .ok_or("exe has no parent directory")?
        .to_path_buf();

    let llama_bin = exe_dir.join(if cfg!(windows) { "llama-server.exe" } else { "llama-server" });
    if !llama_bin.exists() {
        return Err("llama-server binary not found".to_string());
    }

    let models_dir = std::env::var("COLUMBA_MODELS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| exe_dir.join("models"));

    let state = app.state::<LlamaServerProcess>();
    if let Some(mut old) = state.0.lock().unwrap().take() {
        let _ = old.kill();
    }

    match inference_manager::spawn_llama_server(&llama_bin, &models_dir, &profile, LLAMA_PORT) {
        Ok(child) => {
            unsafe {
                std::env::set_var(
                    "COLUMBA_LLAMA_SERVER_URL",
                    inference_manager::server_url(LLAMA_PORT),
                );
                std::env::set_var("COLUMBA_EXECUTION_MODE", "Local");
            }
            *state.0.lock().unwrap() = Some(child);
            Ok(())
        }
        Err(e) => Err(format!("failed to launch llama-server: {e}")),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Entry point
// ──────────────────────────────────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(LlamaServerProcess(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            get_setup_status,
            download_missing_model,
            start_llama_server,
        ])
        .setup(|app| {
            // Resolve paths
            let exe_dir = std::env::current_exe()
                .expect("failed to locate current exe")
                .parent()
                .expect("exe has no parent")
                .to_path_buf();
            let resource_dir = app.path().resource_dir().ok();

            if let Some(manifest_path) = resolve_manifest_path(&exe_dir, resource_dir.as_deref()) {
                unsafe {
                    std::env::set_var(
                        MODEL_MANIFEST_ENV,
                        manifest_path.to_string_lossy().as_ref(),
                    );
                }
            } else {
                eprintln!(
                    "failed to resolve bundled model manifest; first-run download buttons may be disabled"
                );
            }

            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("failed to resolve app data dir");

            let storage_root = resolve_storage_root(&exe_dir, &app_data_dir);
            let models_dir_path = storage_root.join("models");

            std::fs::create_dir_all(&storage_root)
                .expect("failed to create portable storage directory");
            std::fs::create_dir_all(&models_dir_path)
                .expect("failed to create portable model directory");

            // Tell both backend and model_manager where persistent data lives.
            unsafe {
                std::env::set_var("COLUMBA_STORAGE_DIR", storage_root.to_string_lossy().as_ref());
                std::env::set_var("COLUMBA_MODELS_DIR", models_dir_path.to_string_lossy().as_ref());
                std::env::set_var(
                    "COLUMBA_TRADE_LOG",
                    storage_root.join("columba_trades.db").to_string_lossy().as_ref(),
                );
            }

            // Detect hardware and choose model profile
            let hw = detect_hardware();
            let profile = select_model_profile(&hw);

            let llama_bin = exe_dir.join(if cfg!(windows) {
                "llama-server.exe"
            } else {
                "llama-server"
            });

            let main_present = model_is_present(&profile.main_model);
            let draft_present = model_is_present(&profile.draft_model);

            if llama_bin.exists() && main_present && draft_present {
                match inference_manager::spawn_llama_server(
                    &llama_bin,
                    &models_dir_path,
                    &profile,
                    LLAMA_PORT,
                ) {
                    Ok(child) => {
                        unsafe {
                            std::env::set_var(
                                "COLUMBA_LLAMA_SERVER_URL",
                                inference_manager::server_url(LLAMA_PORT),
                            );
                            std::env::set_var("COLUMBA_EXECUTION_MODE", "Local");
                        }
                        *app.state::<LlamaServerProcess>().0.lock().unwrap() = Some(child);
                    }
                    Err(e) => eprintln!("failed to launch llama-server: {e}"),
                }
            } else {
                eprintln!(
                    "llama-server or models missing — will show setup screen. \
                     llama_bin={} main={} draft={}",
                    llama_bin.exists(),
                    main_present,
                    draft_present,
                );
            }

            tauri::async_runtime::spawn(async {
                if let Err(e) = columba_backend::run().await {
                    eprintln!("backend error: {e:?}");
                }
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(mut child) = window
                    .state::<LlamaServerProcess>()
                    .0
                    .lock()
                    .unwrap()
                    .take()
                {
                    let _ = child.kill();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn load_manifest() -> serde_json::Map<String, serde_json::Value> {
    if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(EMBEDDED_MODEL_MANIFEST) {
        return map;
    }

    for candidate in manifest_candidates() {
        if let Ok(content) = std::fs::read_to_string(&candidate) {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&content) {
                return map;
            }
        }
    }

    eprintln!(
        "failed to load model manifest from any known path; download controls may be unavailable"
    );
    serde_json::Map::new()
}

fn manifest_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(path) = std::env::var(MODEL_MANIFEST_ENV) {
        candidates.push(PathBuf::from(path));
    }

    if let Ok(exe_dir) = std::env::current_exe().and_then(|p| {
        p.parent()
            .map(|d| d.to_path_buf())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "exe parent missing"))
    }) {
        candidates.push(exe_dir.join("model_manifest.json"));
        candidates.push(exe_dir.join("resources/model_manifest.json"));
        candidates.push(exe_dir.join("_up_/resources/model_manifest.json"));
    }

    candidates.push(PathBuf::from("resources/model_manifest.json"));
    candidates.push(PathBuf::from("../resources/model_manifest.json"));
    candidates.push(PathBuf::from("_up_/resources/model_manifest.json"));

    candidates
}

fn resolve_manifest_path(exe_dir: &Path, resource_dir: Option<&Path>) -> Option<PathBuf> {
    let mut candidates = Vec::new();

    if let Ok(path) = std::env::var(MODEL_MANIFEST_ENV) {
        candidates.push(PathBuf::from(path));
    }

    if let Some(resource_dir) = resource_dir {
        candidates.push(resource_dir.join("model_manifest.json"));
        candidates.push(resource_dir.join("resources/model_manifest.json"));
        candidates.push(resource_dir.join("_up_/resources/model_manifest.json"));
    }

    candidates.push(exe_dir.join("model_manifest.json"));
    candidates.push(exe_dir.join("resources/model_manifest.json"));
    candidates.push(exe_dir.join("_up_/resources/model_manifest.json"));
    candidates.push(PathBuf::from("resources/model_manifest.json"));
    candidates.push(PathBuf::from("../resources/model_manifest.json"));
    candidates.push(PathBuf::from("_up_/resources/model_manifest.json"));

    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn resolve_storage_root(exe_dir: &Path, app_data_dir: &Path) -> PathBuf {
    if let Ok(dir) = std::env::var("COLUMBA_STORAGE_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("COLUMBA_PORTABLE_ROOT") {
        return PathBuf::from(dir);
    }
    if let Ok(appimage) = std::env::var("APPIMAGE") {
        if let Some(parent) = Path::new(&appimage).parent() {
            return parent.join("ColumbaData");
        }
    }
    if exe_dir.join("portable.flag").exists() {
        return exe_dir.join("ColumbaData");
    }
    app_data_dir.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "columba-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn resolve_manifest_prefers_resource_dir() {
        let exe_dir = unique_temp_dir("exe");
        let resource_dir = unique_temp_dir("resources");
        std::fs::create_dir_all(&exe_dir).expect("create exe temp dir");
        std::fs::create_dir_all(&resource_dir).expect("create resource temp dir");

        let manifest_path = resource_dir.join("model_manifest.json");
        std::fs::write(&manifest_path, "{}").expect("write manifest");

        let resolved = resolve_manifest_path(&exe_dir, Some(&resource_dir));
        assert_eq!(resolved.as_deref(), Some(manifest_path.as_path()));
    }

    #[test]
    fn embedded_manifest_is_available() {
        let manifest = load_manifest();
        assert!(manifest.contains_key("low_end"));
        assert!(manifest.contains_key("mid_range"));
        assert!(manifest.contains_key("high_end"));
    }
}
