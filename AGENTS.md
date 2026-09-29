# AGENTS.md — uranium-engine

HTTP + WebSocket server wrapping `uranium-rs` for launcher GUIs. Single crate (`src/main.rs` binary + `src/lib.rs` lib), edition 2024. Binds `127.0.0.1:13715`.

## Commands

- `cargo run` — serve (tracing filter via `RUST_LOG`, default `info`).
- `cargo test --offline` — full suite (unit + `tests/`). Prefer `--offline`; deps are vendored/locked.
- Focused: `cargo test --test api <name>`, `cargo test --test mrpack`, `cargo test events::`, `cargo test launcher::`.
- Before finishing: `cargo fmt && cargo clippy --all-targets && cargo test --offline`.
- Known pre-existing lint: `tests/error.rs` uses `ErrorKind::Other` (clippy `io_other_error`); leave it.

## Structure

- `src/main.rs` — tracing, `paths::ensure_dirs`, `db::init`, `AppState`, router, bind.
- `src/state.rs` — `AppState { db: Mutex<Connection>, event_tx: broadcast(256), running }`. One `rusqlite` connection (bundled) behind a std `Mutex`: lock briefly, never hold across `.await`.
- `src/routes/` — `instances.rs` (vanilla CRUD + background download), `modpacks.rs` (mrpack install + loader repair), `launcher.rs` (launch/terminate/running), `settings.rs`, `ws.rs`. Registered in `routes/mod.rs` (`router()` + `ApiDoc`).
- `src/launcher.rs` — launch pipeline: `validate_launchable` → `load_merged_root` → `ensure_java_runtime` → `build_launch_config` → `build_java_command`. New launch params go on `LaunchConfig` only.
- `src/events.rs` — `AppEvent` (`#[serde(tag="event", content="data")]`) + `PhaseProgressTracker`.
- `src/db/` — `schema.sql` + `mod.rs::MIGRATION_COLUMNS` (idempotent `PRAGMA table_info` backfill). Add new columns in **both**.
- `src/paths.rs` — XDG dirs via `dirs` crate (`~/.config`, `~/.local/share`, `~/.cache`). Tests do **not** override these.
- Sibling lib: `uranium-rs = { path = "../uranium-rs" }`. Engine can't fix lib bugs; after a lib pull run `cargo update -p uranium-rs` + full suite.

## Conventions / gotchas

- OpenAPI (`utoipa`): every new route/schema must be registered in `routes/mod.rs::ApiDoc`, or `/docs` drifts (there's a drift test).
- `POST /instances` and `/instances/mrpack` return **202** and finish in `tokio::spawn` background tasks emitting WS events. Broadcast cap is 256 with **no replay** — tests/GUI must connect WS **before** POST.
- `instance:progress` is `{instance_id, phase, remaining, total?}` in batch units (`ceil(queue/threads)`). `total` is `None`/absent while the queue is empty (use `serde(default, skip_serializing_if)`); per-phase latch via `PhaseProgressTracker::update` (max-seen, reset on phase change).
- Status semantics: mrpack failure → `error`; loader-repair failure (`POST /instances/{id}/loader`) keeps `ready`. Never invent `InstanceStatus` variants.
- Launch gate: `loader.is_some() && loader_profile.is_none()` → 400. GUI gates Play on `loader_profile`. Forge/NeoForge packs are 400 (no installer); repair endpoint is Fabric/Quilt only.
- `load_merged_root` merges `inheritsFrom` at `serde_json::Value` level (loader profiles omit required `Root` fields): depth cap 8 + cycle guard, child-first libs deduped by exact `name`, jar owner = nearest `downloads.client`, Maven-coordinate fallback for `downloads: null` libs. Classpath joins with `:` (Unix-only).
- `build_java_command` applies version `jvm_args` only when `JVM_ARGS_ON` is set. Tokens use offline placeholders (`player1`, zero UUID).
- Tests (`tests/api.rs`, `tests/mrpack.rs`) spin a real axum server on `127.0.0.1:0` with a `tempfile` DB + 50ms sleep — but **settings tests read/write the real `~/.config/uranium-engine/config.toml`**. Don't run settings tests if local config matters, or back it up.
- License conflict (unresolved): `Cargo.toml` says `GPL-2.0`, `README.md` says MIT. Don't change either without asking.
- No CI, no `opencode.json`, no formatter/linter config beyond defaults. `ENGINE_PLAN.md` is stale design prose (vanilla-only scope, old line counts) — trust code over it.
