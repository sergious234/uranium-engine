CREATE TABLE IF NOT EXISTS instances (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    game_version    TEXT NOT NULL,
    icon            TEXT DEFAULT 'Grass',
    game_dir        TEXT NOT NULL,
    status          TEXT DEFAULT 'ready',
    created_at      TEXT NOT NULL,
    last_played     TEXT,
    playtime_seconds INTEGER DEFAULT 0,
	java_runtime    TEXT NOT NULL,
	java_args       TEXT DEFAULT '',
	modpack_source  TEXT NOT NULL DEFAULT 'vanilla',
	modpack_path    TEXT,
	loader          TEXT,
	loader_version  TEXT,
	loader_profile  TEXT
);
