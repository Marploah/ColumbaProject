---
date: 2026-05-26
topic: windows-offline-bundle
---

# Bundle Offline para Windows (llama-server + modelo)

## What We're Building

Distribuidor Windows autónomo: instala Columba con llama-server y Meta-Llama-3.1-8B-Instruct-Q4_K_M (~4.9 GB) bundleados. No requiere internet después de instalar. GitHub Actions genera el `.exe`/`.msi` automáticamente.

## Why This Approach

**llama-server (llama.cpp) sidecar + GGUF crudo** elegido sobre:
- Ollama bundleado (formato de blobs opaco, 150 MB overhead, complejo)
- Ollama + OLLAMA_MODELS redirect (mismo problema de blobs)

Bundle directo: un `.gguf` + `llama-server.exe` + Tauri. Sin intermediarios.

## Key Decisions

- **Modelo**: Meta-Llama-3.1-8B-Instruct-Q4_K_M (~4.9 GB). `llama3.2:7b` no existe — Llama 3.2 es 1B/3B/11B. Más cercano es Llama 3.1 8B.
- **Detección bundled**: `model_path.metadata().len() > 1_000_000` — placeholder 0-byte = modo dev, archivo real = modo producción.
- **Env vars condicionadas**: `COLUMBA_LLAMA_SERVER_URL` y `COLUMBA_EXECUTION_MODE` se setean SOLO si el spawn de llama-server tiene éxito. Evita redirigir al backend a un puerto vacío.
- **Puerto llama-server**: 8081 (backend Axum en 8080).
- **Placeholders en git**: `binaries/llama-server-*` y `resources/models/*.gguf` vacíos → satisfacen el build check de Tauri. CI descarga y sobrescribe con binarios reales.
- **Build**: GitHub Actions `windows-latest` con caché de `actions/cache` para llama-server y modelo.
- **Cleanup**: `on_window_event(CloseRequested)` mata el proceso llama-server al cerrar.

## Files Changed

- `backend/src/lib.rs` — `AgentTarget::Local` lee `COLUMBA_LLAMA_SERVER_URL` (fallback: `localhost:11434`)
- `src-tauri/src/lib.rs` — spawn llama-server, manage `Child`, kill on close
- `src-tauri/tauri.conf.json` — `externalBin`, `resources`, `webviewInstallMode`
- `.github/workflows/build-windows.yml` — CI workflow completo

## Next Steps

→ Hacer push a GitHub para disparar el workflow
→ Verificar que el `.exe` instalador funciona en Windows
