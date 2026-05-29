---
date: 2026-05-28
topic: replace-ollama-with-llamacpp
---

# Replace Ollama with Bundled llama.cpp

## What We're Building

Remove Ollama as a runtime dependency. Bundle `llama-server` (llama.cpp HTTP server) and a Qwen3 4B GGUF model directly with the project. Both `make dev` and the Tauri desktop app use the bundled binary — no external daemon required.

## Why This Approach

Tauri already bundles `llama-server` and spawns it before the backend starts. The gap is `make dev` (standalone backend) still defaults to Ollama on `:11434`. Moving spawn logic into `backend/src/lib.rs` closes that gap — Tauri sets `COLUMBA_LLAMA_SERVER_URL` before launching the backend, so the backend skips re-spawning when that var is present.

## Key Decisions

- **Spawn location**: Lift `llama-server` spawn from `src-tauri/src/lib.rs` into `backend/src/lib.rs`. Skip spawn if `COLUMBA_LLAMA_SERVER_URL` already set (Tauri path).
- **Dev binary path**: `{project_root}/bin/llama-server` (gitignored)
- **Dev model path**: `{project_root}/resources/models/Qwen3-4B-Q4_K_M.gguf` (gitignored)
- **Model**: Qwen3 4B Q4_K_M quantization — ~2.6 GB, good balance of quality vs VRAM
- **`make setup`**: shell script `scripts/setup.sh` auto-downloads llama-server binary from llama.cpp GitHub releases + model from HuggingFace (`Qwen/Qwen3-4B-GGUF`)
- **Tauri**: update hardcoded model filename in `src-tauri/src/lib.rs` to `Qwen3-4B-Q4_K_M.gguf`
- **Provider rename**: `Provider::Ollama` → `Provider::LlamaCpp` in `backend/src/ai.rs`; rename `AiBroker::ollama` / `ollama_at` constructors accordingly
- **Fallback model string**: remove `llama3.2:3b` hardcoded default; use empty-model path with clear error log

## Open Questions

- llama.cpp release tag to pin (latest stable vs specific version)?
- Context window size: keep `4096` or increase for Qwen3 4B?

## Next Steps

→ `/workflows:plan` for implementation details
