use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tokio::sync::{broadcast, oneshot};

use crate::error::AppError;
use crate::events::AppEvent;

pub struct AppState {
    pub db: Mutex<Connection>,
    pub event_tx: broadcast::Sender<AppEvent>,
    pub running: Arc<Mutex<HashMap<String, RunningGame>>>,
    pub active_operations: Mutex<HashSet<String>>,
}

impl AppState {
    /// Reserve an instance while a launch, loader install, or delete is in flight.
    /// The guard also releases the reservation if an operation fails early.
    pub fn reserve(self: &Arc<Self>, id: &str) -> Result<InstanceOperation, AppError> {
        let mut active = self.active_operations.lock().unwrap();
        if !active.insert(id.to_string()) {
            return Err(AppError::BadRequest(format!("Instance {id} is busy")));
        }
        Ok(InstanceOperation {
            state: self.clone(),
            id: id.to_string(),
        })
    }
}

pub struct InstanceOperation {
    state: Arc<AppState>,
    id: String,
}

impl Drop for InstanceOperation {
    fn drop(&mut self) {
        self.state
            .active_operations
            .lock()
            .unwrap()
            .remove(&self.id);
    }
}

pub struct RunningGame {
    pub pid: u32,
    pub started_at: std::time::Instant,
    pub kill_tx: Option<oneshot::Sender<()>>,
}
