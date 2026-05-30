use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// Where models are stored. Tauri sets COLUMBA_MODELS_DIR to the app data dir;
/// make dev falls back to the checked-in resources/models/ directory.
pub fn models_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("COLUMBA_MODELS_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from("resources/models")
}

pub fn model_path(name: &str) -> PathBuf {
    models_dir().join(name)
}

const MIN_VALID_BYTES: u64 = 50 * 1024 * 1024; // 50 MB
const GGUF_MAGIC: &[u8; 4] = b"GGUF";

pub fn model_is_present(name: &str) -> bool {
    let path = model_path(name);
    let Ok(meta) = path.metadata() else { return false };
    if meta.len() < MIN_VALID_BYTES { return false }
    // Validate GGUF magic bytes to catch truncated/HTML error downloads.
    std::fs::File::open(&path)
        .and_then(|mut f| {
            use std::io::Read;
            let mut buf = [0u8; 4];
            f.read_exact(&mut buf)?;
            Ok(buf)
        })
        .map(|buf| &buf == GGUF_MAGIC)
        .unwrap_or(false)
}

/// Download a single model file with byte-level progress reporting.
/// `on_progress(downloaded_bytes, total_bytes)` is called after each chunk.
/// `total_bytes` may be 0 if the server does not send Content-Length.
pub async fn download_model(
    url: &str,
    dest: &Path,
    on_progress: impl Fn(u64, u64),
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("failed to create models directory")?;
    }

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url)
        .header("User-Agent", "Columba/0.1 (model-downloader)")
        .send()
        .await
        .context("download request failed")?;

    if !response.status().is_success() {
        anyhow::bail!("server returned {} for {}", response.status(), url);
    }

    let total = response.content_length().unwrap_or(0);

    // Write to a temp file first; rename to dest on success so partial
    // downloads never corrupt the model slot.
    let tmp = dest.with_extension("part");
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .context("failed to create temp file")?;

    let mut downloaded = 0u64;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("error reading download stream")?;
        file.write_all(&chunk)
            .await
            .context("error writing to temp file")?;
        downloaded += chunk.len() as u64;
        on_progress(downloaded, total);
    }

    file.flush().await.context("failed to flush temp file")?;
    drop(file);

    tokio::fs::rename(&tmp, dest)
        .await
        .context("failed to rename temp file to final path")?;

    Ok(())
}
