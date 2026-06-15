# uranium-engine

HTTP + WebSocket server that wraps [`uranium-rs`](https://github.com/anomalyco/uranium-rs) so any language (Python, JS, C#, etc.) can build a Minecraft launcher GUI on top of it.

## Quick Start

```bash
cargo run
```

Server binds to `127.0.0.1:13715`.

## REST Endpoints

| Method | Path | Description | Status |
|---|---|---|---|
| `GET` | `/health` | Server health check | 200 |
| `GET` | `/ws` | WebSocket upgrade | 101 |
| `GET` | `/instances` | List all instances | 200 |
| `GET` | `/instances/{id}` | Get single instance | 200 / 404 |
| `POST` | `/instances` | Create new instance | 202 |
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
  "playtime_seconds": 7200
}
```

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
| `instance:progress` | `{instance_id, phase, remaining}` | Download step completed |
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
                print(f"{event['data']['phase']}: {event['data']['remaining']} remaining")
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

```bash
cargo build --release
```

Generate API docs:
```bash
cargo doc --no-deps --open
```

## License

MIT
