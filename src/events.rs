use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Events broadcast by the server to all connected WebSocket clients.
///
/// Each variant serializes as a JSON object with `"event"` and `"data"` keys
/// using `#[serde(tag = "event", content = "data")]`.
///
/// ```json
/// {"event":"instance:progress","data":{"instance_id":"...","phase":"...","remaining":0}}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum AppEvent {
    /// Emitted during background instance download to report the current phase
    /// and number of remaining download requests.
    ///
    /// `phase` is one of: `GettingSources`, `DownloadingVersion`,
    /// `DownloadingAssests`, `DownloadingLibraries`, `DownloadingRuntime`,
    /// `CheckingFiles`.
    #[serde(rename = "instance:progress")]
    InstanceProgress {
        instance_id: String,
        phase: String,
        remaining: usize,
    },

    /// Emitted when an instance download completes successfully.
    /// The instance status is updated to `"ready"` and `launcher_profiles.json`
    /// has been written.
    #[serde(rename = "instance:completed")]
    InstanceCompleted { instance_id: String },

    /// Emitted when an instance download fails.
    /// The instance status is set to `"error"`.
    #[serde(rename = "instance:error")]
    InstanceError {
        instance_id: String,
        error: String,
    },

    /// Emitted when a Minecraft game process is successfully launched.
    /// `pid` is the OS process ID of the Java process.
    #[serde(rename = "game:started")]
    GameStarted {
        instance_id: String,
        pid: u32,
    },

    /// Emitted when a Minecraft game process exits.
    /// `playtime_seconds` is the wall-clock duration the process ran.
    /// The database playtime for this instance is accumulated.
    #[serde(rename = "game:exited")]
    GameExited {
        instance_id: String,
        exit_code: i32,
        playtime_seconds: u64,
    },

    /// Emitted for each line of stdout/stderr from a running Minecraft
    /// process. `stream` is `"stdout"` or `"stderr"`, `line` is the raw
    /// line (without trailing newline).
    #[serde(rename = "game:output")]
    GameOutput {
        instance_id: String,
        stream: String,
        line: String,
    },
}

/// Create a new broadcast channel for [`AppEvent`]s with capacity 256.
pub fn new_event_channel() -> (broadcast::Sender<AppEvent>, broadcast::Receiver<AppEvent>) {
    broadcast::channel(256)
}
