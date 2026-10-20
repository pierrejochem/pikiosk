//! What the running install is doing, for kiosk-ui to poll.

use std::sync::Mutex;

use serde::Serialize;

#[derive(Serialize, Clone, Default, Debug)]
pub struct Progress {
    pub active: bool,
    pub id: String,
    /// "download" or "install"
    pub phase: String,
    pub done: u64,
    pub total: u64,
}

static CURRENT: Mutex<Progress> = Mutex::new(Progress { active: false, id: String::new(), phase: String::new(), done: 0, total: 0 });

pub fn get() -> Progress {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set(id: &str, phase: &str, done: u64, total: u64) {
    let mut p = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    *p = Progress { active: true, id: id.to_string(), phase: phase.to_string(), done, total };
}

pub fn update(phase: &str, done: u64, total: u64) {
    let mut p = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    p.phase = phase.to_string();
    p.done = done;
    p.total = total;
}

/// Marks the work finished when dropped, however it ended.
pub struct Job;

impl Job {
    pub fn start(id: &str) -> Job {
        set(id, "download", 0, 0);
        Job
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Progress::default();
    }
}
