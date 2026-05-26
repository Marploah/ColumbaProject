use std::sync::Mutex;
use tauri::Manager;

struct LlamaServerProcess(Mutex<Option<std::process::Child>>);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(LlamaServerProcess(Mutex::new(None)))
        .setup(|app| {
            let exe_dir = std::env::current_exe()
                .expect("failed to locate current exe")
                .parent()
                .expect("exe has no parent directory")
                .to_path_buf();

            let resource_dir = app.path().resource_dir()
                .expect("failed to locate resource directory");

            let llama_bin = exe_dir.join(
                if cfg!(windows) { "llama-server.exe" } else { "llama-server" },
            );
            let model_path = resource_dir
                .join("models")
                .join("Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf");

            let model_is_real = model_path
                .metadata()
                .map(|m| m.len() > 1_000_000)
                .unwrap_or(false);

            if llama_bin.exists() && model_is_real {
                match std::process::Command::new(&llama_bin)
                    .args([
                        "-m",
                        model_path.to_str().unwrap_or_default(),
                        "--port",
                        "8081",
                        "--host",
                        "127.0.0.1",
                        "-c",
                        "4096",
                    ])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        // Safety: called before the backend Tokio runtime spawns.
                        unsafe {
                            std::env::set_var(
                                "COLUMBA_LLAMA_SERVER_URL",
                                "http://127.0.0.1:8081/v1",
                            );
                            std::env::set_var("COLUMBA_EXECUTION_MODE", "Local");
                        }
                        *app.state::<LlamaServerProcess>().0.lock().unwrap() = Some(child);
                    }
                    Err(e) => eprintln!("failed to launch llama-server: {e}"),
                }
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
