mod inference_manager;

use columba_backend::{
    hardware_profiles::detect_hardware,
    model_manager::{download_model, model_is_present, model_path, models_dir},
    model_selector::select_model_profile,
};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Emitter, Manager, Runtime};

const LLAMA_PORT: u16 = 8081;

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
    let dest = model_path(&name);
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
        ])
        .setup(|app| {
            // Resolve paths
            let exe_dir = std::env::current_exe()
                .expect("failed to locate current exe")
                .parent()
                .expect("exe has no parent")
                .to_path_buf();

            let app_data_dir = app
                .path()
                .app_data_dir()
                .expect("failed to resolve app data dir");

            let models_dir_path = app_data_dir.join("models");

            // Tell both backend and model_manager where models live
            unsafe {
                std::env::set_var("COLUMBA_MODELS_DIR", models_dir_path.to_string_lossy().as_ref());
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
    // Try exe-relative path first (bundled), then project-root path (dev).
    let candidates = [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("resources/model_manifest.json"))),
        Some(std::path::PathBuf::from("resources/model_manifest.json")),
    ];

    for candidate in candidates.into_iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(&candidate) {
            if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(&content) {
                return map;
            }
        }
    }

    serde_json::Map::new()
}
