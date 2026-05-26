---
date: 2026-05-26
topic: tauri-desktop-app
---

# Port ColumbaProject a App de Escritorio (Tauri)

## What We're Building

Convertir ColumbaProject de webapp (browser + servidor separado) a app de escritorio nativa usando Tauri v2. El frontend React se embebe en una ventana WebView nativa. El backend Axum sigue corriendo como servidor HTTP interno, lanzado como tarea Tokio desde el proceso Tauri.

## Why This Approach

**Enfoque A — Tauri con Axum embebido** elegido sobre:
- Comandos nativos Tauri (requiere refactor masivo de todos los handlers HTTP)
- Sidecar (dos binarios, distribución compleja)

Mínimo cambio al código existente. Frontend hace fetch a `127.0.0.1:8080` igual que ahora. WebSocket de Binance sin cambios.

## Key Decisions

- **Tauri v2**: versión estable actual, mejor soporte Linux/Windows/macOS.
- **Backend como lib crate**: `backend/` se convierte en `lib.rs` + `main.rs`. `src-tauri/` depende del crate como dependencia local y llama `columba_backend::run()` en el `setup` hook.
- **Frontend sin cambios**: URLs de fetch y WebSocket quedan como están (`http://127.0.0.1:8080`).
- **Puerto**: mantener 8080, configurable via `BIND_ADDR` como ya existe.
- **Estructura final**:
  ```
  ColumbaProject/
    backend/         (lib + bin — sin cambios de lógica)
    frontend/src/    (React — sin cambios)
    src-tauri/       (nuevo — Tauri app shell)
  ```

## Open Questions

- ¿Target de distribución? Solo Linux, o también Windows/macOS.
- ¿Ícono de app y nombre de ventana personalizado?

## Next Steps

→ Implementar directamente (diseño suficientemente claro para no necesitar plan formal)
