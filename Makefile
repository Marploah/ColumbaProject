.PHONY: dev stop app app-build

dev:
	@trap 'kill 0' INT; \
	( cd backend && cargo run 2>&1 | sed 's/^/\033[36m[backend]\033[0m /' ) & \
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
