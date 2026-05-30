use columba_backend::model_selector::ModelProfile;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Spawn llama-server with speculative decoding enabled.
///
/// Uses `--model-draft` for the draft model and `--parallel 2` for concurrent
/// requests. GPU layers are set to 999 (all layers offloaded) when a GPU is
/// detected, or 0 for CPU-only inference.
pub fn spawn_llama_server(
    llama_bin: &Path,
    models_dir: &Path,
    profile: &ModelProfile,
    port: u16,
) -> std::io::Result<Child> {
    let main_path = models_dir.join(&profile.main_model);
    let draft_path = models_dir.join(&profile.draft_model);

    let mut args: Vec<String> = vec![
        "-m".to_string(),
        main_path.to_string_lossy().into_owned(),
        "--model-draft".to_string(),
        draft_path.to_string_lossy().into_owned(),
        "-c".to_string(),
        profile.context_size.to_string(),
        "-t".to_string(),
        profile.threads.to_string(),
        "-ngl".to_string(),
        profile.gpu_layers.to_string(),
        "--parallel".to_string(),
        "2".to_string(),
        "--cont-batching".to_string(),
        "--flash-attn".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
    ];

    // Only pass --flash-attn on CUDA/GPU paths; CPU flash-attn is unsupported
    // in some llama.cpp builds. Remove the flag for pure-CPU tiers.
    if profile.gpu_layers == 0 {
        args.retain(|a| a != "--flash-attn");
    }

    Command::new(llama_bin)
        .args(&args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Build the full llama-server URL for a given port.
pub fn server_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/v1")
}
