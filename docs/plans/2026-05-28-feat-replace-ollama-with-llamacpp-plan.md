---
title: "feat: Replace Ollama with bundled llama.cpp server"
type: feat
status: active
date: 2026-05-28
origin: docs/brainstorms/2026-05-28-replace-ollama-with-llamacpp-brainstorm.md
---

# feat: Replace Ollama with Bundled llama.cpp Server

## Overview

Remove Ollama as a runtime dependency. Bundle `llama-server` (llama.cpp HTTP server) and a Qwen3 4B Q4_K_M GGUF model. Both `make dev` and the Tauri desktop app use the bundled binary — no external daemon required.

The wire protocol is unchanged: both Ollama and llama-server expose an OpenAI-compatible `/v1/chat/completions` endpoint. This is a naming + spawn-wiring refactor, not an HTTP layer change.

## Proposed Solution

1. **`make setup`** downloads llama-server (latest llama.cpp GitHub release) and Qwen3-4B-Q4_K_M.gguf from HuggingFace into `bin/` and `resources/models/`.
2. **`make dev`** conditionally spawns `./bin/llama-server`, sets `COLUMBA_LLAMA_SERVER_URL=http://127.0.0.1:8081/v1` + `COLUMBA_EXECUTION_MODE=Local`, then starts backend + frontend as before.
3. **Backend** renames `Provider::Ollama` → `Provider::LlamaCpp` and all `ollama` constructors/fields to `llama_cpp` / `llama_server_url`. Default URL changes from `:11434` to `:8081`.
4. **Tauri** updates the hardcoded model filename from `Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf` to `Qwen3-4B-Q4_K_M.gguf`. Spawn logic unchanged.
5. **Frontend** renames `ollamaUrl` state/localStorage/UI/JSON body field to `llamaServerUrl` / `llama_server_url`, updates defaults and label text.

(see brainstorm: docs/brainstorms/2026-05-28-replace-ollama-with-llamacpp-brainstorm.md)

## Implementation Steps

### 1. `Makefile`

All download logic lives in the `setup` target. Add `setup` target + update `dev`.

```makefile
MODEL      = resources/models/Qwen3-4B-Q4_K_M.gguf
LLAMA_BIN  = bin/llama-server
LLAMA_URL  = http://127.0.0.1:8081/v1
HF_MODEL_URL = https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf

.PHONY: dev stop app app-build setup

setup:
	@mkdir -p bin resources/models
	@if [ ! -f $(LLAMA_BIN) ]; then \
	  echo "Downloading llama-server..."; \
	  TAG=$$(curl -s https://api.github.com/repos/ggerganov/llama.cpp/releases/latest \
	    | grep '"tag_name"' | cut -d'"' -f4); \
	  ARCH=$$(uname -m); OS=$$(uname -s); \
	  case "$$OS-$$ARCH" in \
	    Linux-x86_64)  ASSET="llama-$$TAG-bin-ubuntu-x64.zip" ;; \
	    Linux-aarch64) ASSET="llama-$$TAG-bin-ubuntu-arm64.zip" ;; \
	    Darwin-arm64)  ASSET="llama-$$TAG-bin-macos-arm64.zip" ;; \
	    Darwin-x86_64) ASSET="llama-$$TAG-bin-macos-x64.zip" ;; \
	    *) echo "Unsupported platform: $$OS-$$ARCH" && exit 1 ;; \
	  esac; \
	  curl -L -o /tmp/llama.zip \
	    "https://github.com/ggerganov/llama.cpp/releases/download/$$TAG/$$ASSET"; \
	  unzip -j /tmp/llama.zip "*/llama-server" -d bin/ || \
	    unzip -j /tmp/llama.zip "llama-server" -d bin/; \
	  chmod +x $(LLAMA_BIN); \
	  rm /tmp/llama.zip; \
	  echo "llama-server installed."; \
	else \
	  echo "llama-server already present, skipping."; \
	fi
	@if [ ! -f $(MODEL) ]; then \
	  echo "Downloading Qwen3-4B-Q4_K_M.gguf (~2.6 GB)..."; \
	  curl -L --progress-bar -o $(MODEL) $(HF_MODEL_URL); \
	  echo "Model downloaded."; \
	else \
	  echo "Model already present, skipping."; \
	fi

dev:
	@trap 'kill 0' INT; \
	if [ -f $(LLAMA_BIN) ] && [ -f $(MODEL) ]; then \
	  $(LLAMA_BIN) -m $(MODEL) --port 8081 --host 127.0.0.1 -c 4096 2>&1 \
	    | sed 's/^/\033[35m[llama]\033[0m /' & \
	  sleep 2; \
	fi; \
	( cd backend && COLUMBA_LLAMA_SERVER_URL=$(LLAMA_URL) COLUMBA_EXECUTION_MODE=Local \
	    cargo run 2>&1 | sed 's/^/\033[36m[backend]\033[0m /' ) & \
	( cd frontend && npm run dev 2>&1 | sed 's/^/\033[33m[frontend]\033[0m /' ) & \
	wait
```

`app` and `app-build` targets unchanged (Tauri handles its own spawn).

### 3. `.gitignore`

Add:
```
/bin/
/resources/models/
```

### 4. `backend/src/ai.rs`

Rename only — no HTTP logic changes.

| Old | New |
|-----|-----|
| `Provider::Ollama` | `Provider::LlamaCpp` |
| `AiBroker::ollama(model)` | `AiBroker::llama_cpp(model)` |
| `AiBroker::ollama_at(model, url)` | `AiBroker::llama_cpp_at(model, url)` |
| `api_key = "ollama"` | `api_key = "no-key"` |
| `provider: Provider::Ollama` (line 83) | `provider: Provider::LlamaCpp` |

The `chat_completion` wildcard arm (`_ =>`) already routes both Ollama and OpenAI to `openai_completion` — no logic change needed there.

### 5. `backend/src/lib.rs`

Four changes:

**a) `AnalyzeRequest` struct** (line 61):
```rust
// old
ollama_url: Option<String>,
// new
llama_server_url: Option<String>,
```

**b) `AgentTarget::Local` branch** (lines 446–448):
```rust
// old
let url = env::var("COLUMBA_LLAMA_SERVER_URL")
    .unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
AiBroker::ollama_at(model, url)
// new
let url = env::var("COLUMBA_LLAMA_SERVER_URL")
    .unwrap_or_else(|_| "http://127.0.0.1:8081/v1".to_string());
AiBroker::llama_cpp_at(model, url)
```

**c) Cloud-fallback branch** (lines 450–452):
```rust
// old
AiBroker::ollama("llama3.2:3b".to_string())
// new — fallback to local llama-server, model name from env or default Qwen3
warn!("OPENAI_API_KEY is unset; falling back to local llama-server");
let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "qwen3-4b".to_string());
let url = env::var("COLUMBA_LLAMA_SERVER_URL")
    .unwrap_or_else(|_| "http://127.0.0.1:8081/v1".to_string());
AiBroker::llama_cpp_at(model, url)
```

**d) `analyze` handler** (lines 761–764):
```rust
// old
let broker = match request.ollama_url {
    Some(url) => AiBroker::ollama_at(model, url),
// new
let broker = match request.llama_server_url {
    Some(url) => AiBroker::llama_cpp_at(model, url),
```

### 6. `src-tauri/src/lib.rs`

Single change — model filename (line 25):
```rust
// old
.join("Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf");
// new
.join("Qwen3-4B-Q4_K_M.gguf");
```

No other changes — port `8081`, context `4096`, and spawn logic all stay identical (see brainstorm: decisions section).

### 7. `frontend/src/App.tsx`

Four rename sites:

**State init** (line 71–73):
```tsx
// old
const [ollamaUrl, setOllamaUrl] = useState(
  () => localStorage.getItem('columba_ollama_url') ?? 'http://localhost:11434/v1',
);
// new
const [llamaServerUrl, setLlamaServerUrl] = useState(
  () => localStorage.getItem('columba_llama_server_url') ?? 'http://127.0.0.1:8081/v1',
);
```

**localStorage write** (line 220):
```tsx
// old
localStorage.setItem('columba_ollama_url', ollamaUrl);
// new
localStorage.setItem('columba_llama_server_url', llamaServerUrl);
```

**API body** (line 249):
```tsx
// old
if (ollamaUrl) body.ollama_url = ollamaUrl;
// new
if (llamaServerUrl) body.llama_server_url = llamaServerUrl;
```

**Settings UI** (lines 452–460):
```tsx
// old
<label className="select-label">
  Ollama base URL
  <input value={ollamaUrl} onChange={(e) => setOllamaUrl(e.target.value)}
         placeholder="http://localhost:11434/v1" />
</label>
// new
<label className="select-label">
  llama-server URL
  <input value={llamaServerUrl} onChange={(e) => setLlamaServerUrl(e.target.value)}
         placeholder="http://127.0.0.1:8081/v1" />
</label>
```

### 8. `CLAUDE.md`

- Update `COLUMBA_LLAMA_SERVER_URL` default from `:11434/v1` to `:8081/v1`
- Replace Ollama references in env var table with llama-server
- Add `make setup` to Commands section
- Update architecture note about Local mode fallback

## Acceptance Criteria

- [ ] `make setup` downloads `bin/llama-server` and `resources/models/Qwen3-4B-Q4_K_M.gguf` directly from Makefile; idempotent (skips if files exist)
- [ ] `make dev` spawns llama-server on `:8081` when binary + model present, starts backend with `COLUMBA_EXECUTION_MODE=Local`
- [ ] `make dev` still works without binary/model (skips llama-server, backend uses cloud or errors cleanly)
- [ ] No `Ollama` / `ollama` identifiers remain in Rust source (`cargo check` clean)
- [ ] `POST /api/analyze` accepts `llama_server_url` field and routes correctly
- [ ] Frontend settings panel shows "llama-server URL" with default `http://127.0.0.1:8081/v1`
- [ ] Tauri app uses `Qwen3-4B-Q4_K_M.gguf` model filename
- [ ] `.gitignore` excludes `bin/` and `resources/models/`

## Dependencies & Risks

- **HuggingFace URL stability**: direct `/resolve/main/` URLs are stable but not versioned. If Qwen3 4B GGUF repo renames the file, setup breaks. Mitigation: pin the exact filename in the script comment.
- **llama.cpp release asset naming**: GitHub asset names have changed historically. Script must use the API to find the correct asset dynamically rather than hardcoding a name pattern.
- **`sleep 2` in Makefile**: llama-server needs startup time before backend starts. 2s is usually enough but on slow machines may not be. If backend can't reach llama-server it logs a warning on first request — not a crash.
- **localStorage key migration**: existing users have `columba_ollama_url` in localStorage. Old key will be ignored; they get the new default `:8081`. No migration code needed — just a UX note.

## Sources & References

- **Origin brainstorm:** [docs/brainstorms/2026-05-28-replace-ollama-with-llamacpp-brainstorm.md](../brainstorms/2026-05-28-replace-ollama-with-llamacpp-brainstorm.md) — Key decisions carried forward: (1) spawn stays in process manager not backend, (2) Qwen3 4B Q4_K_M model, (3) auto-download via setup script
- Provider enum: `backend/src/ai.rs:24–28`
- Tauri spawn block: `src-tauri/src/lib.rs:20–61`
- Frontend API body: `frontend/src/App.tsx:244–254`
- Frontend settings UI: `frontend/src/App.tsx:452–460`
