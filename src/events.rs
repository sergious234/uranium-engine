use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use utoipa::ToSchema;

/// Events broadcast by the server to all connected WebSocket clients.
///
/// Each variant serializes as a JSON object with `"event"` and `"data"` keys
/// using `#[serde(tag = "event", content = "data")]`.
///
/// ```json
/// {"event":"instance:progress","data":{"instance_id":"...","phase":"...","remaining":0,"total":120}}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "event", content = "data")]
pub enum AppEvent {
    /// Emitted during background instance download to report the current phase
    /// and number of remaining download requests.
    ///
    /// `phase` is one of: `GettingSources`, `DownloadingVersion`,
    /// `DownloadingAssets`, `DownloadingLibraries`, `DownloadingRuntime`,
    /// `CheckingFiles` (vanilla installs) or `ResolvingManifest`,
    /// `InstallingMinecraft`, `InstallingLoader`, `CopyingOverrides`,
    /// `DownloadingFiles`, `Verifying` (modpack installs).
    ///
    /// `remaining` and `total` share the same batch units
    /// (`ceil(queue_len / threads)`). `total` is the latched batch count at
    /// phase entry so clients can render `done = total - remaining`;
    /// `None` (absent on the wire) means the total is not known yet
    /// (e.g. the vanilla queue hasn't been populated on the first poll).
    #[serde(rename = "instance:progress")]
    InstanceProgress {
        instance_id: String,
        phase: String,
        remaining: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<usize>,
    },

    /// Emitted when an instance download completes successfully.
    /// The instance status is updated to `"ready"` and `launcher_profiles.json`
    /// has been written.
    #[serde(rename = "instance:completed")]
    InstanceCompleted { instance_id: String },

    /// Emitted when an instance download fails.
    /// The instance status is set to `"error"`.
    #[serde(rename = "instance:error")]
    InstanceError { instance_id: String, error: String },

    /// Emitted when a Minecraft game process is successfully launched.
    /// `pid` is the OS process ID of the Java process.
    #[serde(rename = "game:started")]
    GameStarted { instance_id: String, pid: u32 },

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

/// Latches a stable per-phase batch total from a shrinking `remaining` count.
///
/// Lib queues start empty (0 before init), jump once populated, then drain —
/// and `requests_left()` also grows on re-queues/retries. Tracking
/// `max(seen)` per phase yields the denominator for `done = total - remaining`.
/// Phase changes reset the latch. Returns `None` while nothing has been
/// queued yet (`total == 0`), so the event omits `total` instead of sending a
/// misleading `0`.
#[derive(Debug, Default)]
pub struct PhaseProgressTracker {
    phase: String,
    total: usize,
}

impl PhaseProgressTracker {
    /// Record a poll for `phase` with `remaining` batches; returns the total
    /// to send alongside it (`None` while unknown).
    pub fn update(&mut self, phase: &str, remaining: usize) -> Option<usize> {
        if self.phase != phase {
            self.phase = phase.to_string();
            self.total = remaining;
        } else if remaining > self.total {
            self.total = remaining;
        }
        if self.total == 0 {
            None
        } else {
            Some(self.total)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_latches_max_and_resets_on_phase_change() {
        let mut t = PhaseProgressTracker::default();
        // Pre-init poll: queue empty, total unknown.
        assert_eq!(t.update("InstallingMinecraft", 0), None);
        // Queue populated after init: latch.
        assert_eq!(t.update("InstallingMinecraft", 120), Some(120));
        // Drain: total sticks.
        assert_eq!(t.update("InstallingMinecraft", 90), Some(120));
        // Re-queue grows the total.
        assert_eq!(t.update("InstallingMinecraft", 130), Some(130));
        // New phase resets.
        assert_eq!(t.update("DownloadingFiles", 7), Some(7));
        assert_eq!(t.update("DownloadingFiles", 0), Some(7));
    }
}
