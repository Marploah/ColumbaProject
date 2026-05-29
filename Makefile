MODEL     = resources/models/Qwen3-4B-Q4_K_M.gguf
LLAMA_BIN = bin/llama-server
LLAMA_URL = http://127.0.0.1:8081/v1
HF_MODEL  = https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf

.PHONY: dev stop app app-build setup

setup:
	@mkdir -p bin resources/models
	@if [ ! -f $(LLAMA_BIN) ]; then \
	  echo "Downloading llama-server (latest llama.cpp release)..."; \
	  TAG=$$(curl -sfL https://api.github.com/repos/ggerganov/llama.cpp/releases/latest \
	    | grep '"tag_name"' | cut -d'"' -f4); \
	  ARCH=$$(uname -m); OS=$$(uname -s); \
	  case "$$OS-$$ARCH" in \
	    Linux-x86_64)  ASSET="llama-$$TAG-bin-ubuntu-x64.tar.gz" ;; \
	    Linux-aarch64) ASSET="llama-$$TAG-bin-ubuntu-arm64.tar.gz" ;; \
	    Darwin-arm64)  ASSET="llama-$$TAG-bin-macos-arm64.tar.gz" ;; \
	    Darwin-x86_64) ASSET="llama-$$TAG-bin-macos-x64.tar.gz" ;; \
	    *) echo "Unsupported platform: $$OS-$$ARCH"; exit 1 ;; \
	  esac; \
	  curl -L --progress-bar -o /tmp/llama.tar.gz \
	    "https://github.com/ggerganov/llama.cpp/releases/download/$$TAG/$$ASSET"; \
	  tar -xz --strip-components=1 -C bin/ -f /tmp/llama.tar.gz; \
	  chmod +x $(LLAMA_BIN); \
	  rm -f /tmp/llama.tar.gz; \
	  echo "llama-server installed ($$TAG)."; \
	else \
	  echo "llama-server already present, skipping."; \
	fi
	@MODEL_OK=0; \
	if [ -f $(MODEL) ]; then \
	  SIZE=$$(stat -c%s $(MODEL) 2>/dev/null || stat -f%z $(MODEL) 2>/dev/null || echo 0); \
	  if [ "$$SIZE" -gt 2000000000 ]; then MODEL_OK=1; fi; \
	fi; \
	if [ "$$MODEL_OK" -eq 0 ]; then \
	  echo "Downloading Qwen3-4B-Q4_K_M.gguf (~2.6 GB)..."; \
	  curl -L --progress-bar -o $(MODEL) $(HF_MODEL); \
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

app:
	@trap 'kill 0' INT; \
	( cd frontend && npm run dev 2>&1 | sed 's/^/\033[33m[frontend]\033[0m /' ) & \
	( sleep 2 && cd src-tauri && cargo tauri dev 2>&1 | sed 's/^/\033[36m[tauri]\033[0m /' ) & \
	wait

app-build:
	cd frontend && npm run build
	cd src-tauri && cargo tauri build
