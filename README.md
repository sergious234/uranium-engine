# uranium-engine

HTTP + WebSocket server that wraps [`uranium-rs`](https://github.com/anomalyco/uranium-rs) so any language (Python, JS, C#, etc.) can build a Minecraft launcher GUI on top of it.

## Quick Start

```bash
URANIUM_API_TOKEN="$(openssl rand -hex 32)" cargo run
```

Server binds to `127.0.0.1:13715`.
Set `URANIUM_API_TOKEN` to a secret value before starting the server. Send it as
`X-Uranium-Token` with every HTTP request; WebSocket clients use
`/ws?token=<value>`. Requests without the token receive `401 Unauthorized`.
The Tauri launcher creates and passes its own token when it starts the sidecar.

## REST Endpoints

| Method | Path | Description | Status |
|---|---|---|---|
| `GET` | `/health` | Server health check | 200 |
| `GET` | `/ws` | WebSocket upgrade | 101 |
| `GET` | `/instances` | List all instances | 200 |
| `GET` | `/instances/{id}` | Get single instance | 200 / 404 |
| `POST` | `/instances` | Create new instance | 202 |
| `POST` | `/instances/mrpack` | Create new instance from a Modrinth pack | 202 |
| `POST` | `/instances/{id}/loader` | Install the loader for a modpack instance | 202 |
| `PATCH` | `/instances/{id}` | Rename / change icon | 200 / 404 |
| `DELETE` | `/instances/{id}` | Delete instance | 200 / 404 |
| `POST` | `/launch/{id}` | Launch Minecraft | 200 / 400 / 404 |
| `POST` | `/terminate/{id}` | Kill running game | 200 / 404 |
| `GET` | `/running` | List running instance IDs | 200 |
| `GET` | `/settings` | Get config | 200 |
| `PUT` | `/settings` | Update config | 200 |

### POST /instances

```json
// Request
{ "name": "My 1.21", "version": "1.21", "icon": "Diamond" }

// Response (202 Accepted)
{ "instance_id": "550e8400-e29b-41d4-a716-446655440000" }
```

### Instance object

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "name": "My 1.21",
  "game_version": "1.21",
  "icon": "Diamond",
  "game_dir": "/home/user/.local/share/uranium-engine/instances/550e8400-...",
  "status": "ready",
  "created_at": "2026-06-15T10:30:00Z",
  "last_played": "2026-06-20T18:45:00Z",
  "playtime_seconds": 7200,
  "modpack_source": "vanilla",
  "modpack_path": null,
  "loader": null,
  "loader_version": null
}
```

`modpack_source` is `"vanilla"` or `"mrpack"`. For modpack instances
`modpack_path` holds the source `.mrpack` and `loader`/`loader_version`
record the pack's requirement (e.g. `"Fabric"` / `"0.16.9"`).
`loader_profile` holds the installed profile id (e.g.
`"fabric-loader-0.16.9-1.21"`) — the GUI gates Play on it being non-null.

### POST /instances/mrpack

```json
// Request (mrpack_path is a local server-side path)
{ "name": "Fabulously Optimized", "mrpack_path": "/home/user/Downloads/pack.mrpack" }

// Optional: "icon", "java_args", "version_override"
// (version_override is required when the pack declares no minecraft version)

// Response (202 Accepted)
{ "instance_id": "550e8400-e29b-41d4-a716-446655440000" }
```

The install runs in the background: vanilla Minecraft for the pack's
`dependencies.minecraft`, the declared loader (Fabric/Quilt), side-matching
overrides, then `env`-filtered mod files. Progress arrives over WebSocket
(`instance:progress` with `phase` one of `ResolvingManifest`,
`InstallingMinecraft`, `InstallingLoader`, `CopyingOverrides`,
`DownloadingFiles`, `Verifying`). Each frame carries `remaining`/`total`
batch counts for its phase — render progress as `done = total - remaining`
(`total` is absent until the phase queue is populated).

Forge/NeoForge packs are rejected (`400`) — their installers are not
implemented yet.

### POST /instances/{id}/loader

Installs (or re-installs) the loader for an existing modpack instance —
for rows stuck at `"ready"` with a declared loader but no installed profile.
A loader failure keeps status `"ready"`.

```json
// Response (202 Accepted)
{ "instance_id": "550e8400-e29b-41d4-a716-446655440000" }
```

### Launching modded instances

`POST /launch/{id}` resolves `loader_profile` when set, flattening the
profile's `inheritsFrom` chain (loader libraries first, loader
`mainClass`/`arguments` win, vanilla jar). Launch is rejected with `400`
when a declared loader has no installed profile.

### PATCH /instances/{id}

```json
{ "name": "New Name", "icon": "Emerald" }
```

### POST /launch/{id}

Response:
```json
{ "pid": 98765 }
```

### GET /running

```json
{ "running": [{ "instance_id": "uuid", "pid": 98765 }] }
```

### Settings

```json
// GET /settings, PUT /settings
{
  "java_path": null,
  "max_memory": "2G",
  "jvm_args": null,
  "window_width": 854,
  "window_height": 480,
  "show_launcher": false
}
```

## WebSocket Events

Connect to `ws://127.0.0.1:13715/ws`. All events are JSON with `{"event":"...","data":{...}}` format.

| Event | Data | When |
|---|---|---|---|
| `instance:progress` | `{instance_id, phase, remaining, total?}` | Download step completed (`done = total - remaining`; `total` absent while unknown) |
| `instance:completed` | `{instance_id}` | Download finished successfully |
| `instance:error` | `{instance_id, error}` | Download failed |
| `game:started` | `{instance_id, pid}` | Game process spawned |
| `game:output` | `{instance_id, stream, line}` | Each line of game stdout/stderr |
| `game:exited` | `{instance_id, exit_code, playtime_seconds}` | Game process exited |

### Example session (Python)

```python
import asyncio, json, requests
import websockets

async def main():
    async with websockets.connect("ws://127.0.0.1:13715/ws") as ws:
        # Create an instance
        r = requests.post("http://127.0.0.1:13715/instances",
            json={"name": "My 1.21", "version": "1.21"})
        instance_id = r.json()["instance_id"]

        # Watch progress
        async for msg in ws:
            event = json.loads(msg)
            if event["event"] == "instance:progress":
                data = event['data']
                total = data.get('total')
                if total:
                    done = total - data['remaining']
                    print(f"{data['phase']}: {done}/{total} batches")
                else:
                    print(f"{data['phase']}: {data['remaining']} remaining")
            elif event["event"] == "instance:completed":
                print("Done! Launching...")
                requests.post(f"http://127.0.0.1:13715/launch/{instance_id}")
            elif event["event"] == "instance:error":
                print(f"Error: {event['data']['error']}")
                break

asyncio.run(main())
```

## Configuration

Files follow XDG Base Directory Specification:

| Path | Default | Purpose |
|---|---|---|
| Config | `~/.config/uranium-engine/config.toml` | Settings file |
| Database | `~/.local/share/uranium-engine/data.db` | SQLite instance store |
| Cache | `~/.cache/uranium-engine/` | Temporary download data |
| Instances | `~/.local/share/uranium-engine/instances/` | Per-instance game directories |

## Build

Clone with submodules (vendored `uranium-rs`):

```bash
git clone --recurse-submodules <url>
```

```bash
cargo build --release
```

Generate API docs:
```bash
cargo doc --no-deps --open
```

## License

GPL-2.0
