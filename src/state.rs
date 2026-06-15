use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::{broadcast, oneshot};

use crate::events::AppEvent;

pub struct AppState {
    pub db: Mutex<Connection>,
    pub event_tx: broadcast::Sender<AppEvent>,
    pub running: Arc<Mutex<HashMap<String, RunningGame>>>,
}

pub struct RunningGame {
    pub pid: u32,
    pub started_at: std::time::Instant,
    pub kill_tx: Option<oneshot::Sender<()>>,
}
