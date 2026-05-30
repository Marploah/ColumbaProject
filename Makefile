LLAMA_BIN  = bin/llama-server
LLAMA_URL  = http://127.0.0.1:8081/v1
MODELS_DIR = resources/models

# ── Model catalogue ────────────────────────────────────────────────────────────
HF_QWEN3_4B   = https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf
HF_QWEN3_0_6B = https://huggingface.co/ggml-org/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q4_K_M.gguf
HF_QWEN3_8B   = https://huggingface.co/Qwen/Qwen3-8B-GGUF/resolve/main/Qwen3-8B-Q4_K_M.gguf
HF_QWEN3_1_7B = https://huggingface.co/ggml-org/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf
HF_QWEN3_14B  = https://huggingface.co/ggml-org/Qwen3-14B-GGUF/resolve/main/Qwen3-14B-Q4_K_M.gguf

# ── Tier detection (shell-based approximation) ────────────────────────────────
# Tier thresholds (GB):  high-end ≥20 GB VRAM  |  mid-range ≥12 GB RAM  |  else low-end
DETECT_TIER = \
  if command -v nvidia-smi >/dev/null 2>&1; then \
    VRAM=$$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits 2>/dev/null \
      | awk '{if($$1>max) max=$$1} END{print int(max/1024)}'); \
    if [ "$$VRAM" -ge 20 ] 2>/dev/null; then echo "high_end"; \
    elif [ "$$VRAM" -ge 8 ] 2>/dev/null; then echo "mid_range"; \
    else echo "low_end"; fi; \
  else \
    RAM=$$(awk '/MemTotal/{print int($$2/1024/1024)}' /proc/meminfo 2>/dev/null \
      || sysctl -n hw.memsize 2>/dev/null | awk '{print int($$1/1024/1024/1024)}'); \
    if [ "$$RAM" -ge 24 ] 2>/dev/null; then echo "mid_range"; \
    elif [ "$$RAM" -ge 12 ] 2>/dev/null; then echo "mid_range"; \
    else echo "low_end"; fi; \
  fi

.PHONY: dev stop app app-build setup setup-tier detect-tier

# ── detect-tier: print detected hardware tier ────────────────────────────────
detect-tier:
	@$(DETECT_TIER)

# ── setup: download llama-server + models for detected tier ──────────────────
setup:
	@mkdir -p $(MODELS_DIR) bin
	@$(MAKE) _llama_server
	@TIER=$$($(DETECT_TIER)); \
	echo "Hardware tier: $$TIER"; \
	$(MAKE) _models_for_tier TIER=$$TIER

# ── setup-tier: force a specific tier (make setup-tier TIER=mid_range) ───────
setup-tier:
	@mkdir -p $(MODELS_DIR) bin
	@$(MAKE) _llama_server
	@$(MAKE) _models_for_tier TIER=$(TIER)

_llama_server:
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

_models_for_tier:
	@case "$(TIER)" in \
	  high_end) \
	    $(MAKE) _download_model FILE="Qwen3-14B-Q4_K_M.gguf" URL="$(HF_QWEN3_14B)" MIN_MB=8000; \
	    $(MAKE) _download_model FILE="Qwen3-4B-Q4_K_M.gguf"  URL="$(HF_QWEN3_4B)"  MIN_MB=2000; \
	    ;; \
	  mid_range) \
	    $(MAKE) _download_model FILE="Qwen3-8B-Q4_K_M.gguf"   URL="$(HF_QWEN3_8B)"   MIN_MB=4000; \
	    $(MAKE) _download_model FILE="Qwen3-1.7B-Q4_K_M.gguf" URL="$(HF_QWEN3_1_7B)" MIN_MB=1000; \
	    ;; \
	  *) \
	    $(MAKE) _download_model FILE="Qwen3-4B-Q4_K_M.gguf"   URL="$(HF_QWEN3_4B)"   MIN_MB=2000; \
	    $(MAKE) _download_model FILE="Qwen3-0.6B-Q4_K_M.gguf" URL="$(HF_QWEN3_0_6B)" MIN_MB=300; \
	    ;; \
	esac

_download_model:
	@MODEL_OK=0; \
	if [ -f "$(MODELS_DIR)/$(FILE)" ]; then \
	  SIZE=$$(stat -c%s "$(MODELS_DIR)/$(FILE)" 2>/dev/null || stat -f%z "$(MODELS_DIR)/$(FILE)" 2>/dev/null || echo 0); \
	  if [ "$$SIZE" -gt $$(($(MIN_MB) * 1000000)) ]; then MODEL_OK=1; fi; \
	fi; \
	if [ "$$MODEL_OK" -eq 0 ]; then \
	  echo "Downloading $(FILE)..."; \
	  curl -L --progress-bar -o "$(MODELS_DIR)/$(FILE)" "$(URL)"; \
	  echo "$(FILE) downloaded."; \
	else \
	  echo "$(FILE) already present, skipping."; \
	fi

# ── dev: detect tier, spawn llama-server + backend + frontend ────────────────
dev:
	@trap 'kill 0' INT; \
	TIER=$$($(DETECT_TIER)); \
	echo "Starting with tier: $$TIER"; \
	case "$$TIER" in \
	  high_end)  MAIN="Qwen3-14B-Q4_K_M.gguf"; DRAFT="Qwen3-4B-Q4_K_M.gguf";   CTX=16384; NGL=999 ;; \
	  mid_range) MAIN="Qwen3-8B-Q4_K_M.gguf";  DRAFT="Qwen3-1.7B-Q4_K_M.gguf"; CTX=8192;  NGL=0   ;; \
	  *)          MAIN="Qwen3-4B-Q4_K_M.gguf";  DRAFT="Qwen3-0.6B-Q4_K_M.gguf"; CTX=4096;  NGL=0   ;; \
	esac; \
	THREADS=$$(awk '/^processor/{n++} END{printf "%d", int(n*0.7+0.5)}' /proc/cpuinfo 2>/dev/null || echo 4); \
	is_valid_gguf() { [ -f "$$1" ] && [ "$$(head -c 4 "$$1")" = "GGUF" ]; }; \
	if [ -f "$(LLAMA_BIN)" ] && is_valid_gguf "$(MODELS_DIR)/$$MAIN" && is_valid_gguf "$(MODELS_DIR)/$$DRAFT"; then \
	  $(LLAMA_BIN) \
	    -m "$(MODELS_DIR)/$$MAIN" \
	    --model-draft "$(MODELS_DIR)/$$DRAFT" \
	    -c "$$CTX" -t "$$THREADS" -ngl "$$NGL" \
	    --parallel 2 --cont-batching \
	    --port 8081 --host 127.0.0.1 2>&1 \
	    | sed 's/^/\033[35m[llama]\033[0m /' & \
	  sleep 2; \
	fi; \
	( cd backend && COLUMBA_LLAMA_SERVER_URL=$(LLAMA_URL) COLUMBA_EXECUTION_MODE=Local \
	    COLUMBA_MODELS_DIR=../resources/models \
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
