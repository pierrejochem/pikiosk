//! /data/store/state.json: what has to survive a reboot besides the installed
//! apps themselves: updates still on their health check, versions that failed
//! it, and rollback notices the admin has not dismissed yet.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One writer at a time: updates read the file, change it and write it back.
static LOCK: Mutex<()> = Mutex::new(());

/// An update that has not yet passed its health check.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Pending {
    pub version: String,
    /// the version to go back to
    pub previous: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Notice {
    pub id: String,
    pub name: String,
    pub from: String,
    pub to: String,
    pub reason: String,
    /// unix seconds; the board has no RTC, so this can be wrong
    pub at: u64,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct State {
    #[serde(default)]
    pub pending: BTreeMap<String, Pending>,
    /// versions that failed their health check; the store does not offer them again
    #[serde(default)]
    pub blocked: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub notices: Vec<Notice>,
}

fn path(store: &Path) -> std::path::PathBuf {
    store.join("state.json")
}

pub fn read(store: &Path) -> State {
    std::fs::read(path(store)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Reads the state, lets `f` change it and writes it back atomically.
pub fn update<T>(store: &Path, f: impl FnOnce(&mut State) -> T) -> Result<T> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut state = read(store);
    let out = f(&mut state);
    std::fs::create_dir_all(store)?;
    let (tmp, dest) = (store.join("state.json.new"), path(store));
    let mut file = File::create(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(&state)?)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &dest)?;
    File::open(store)?.sync_all()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_survives_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("kioskd-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read(&dir), State::default());
        update(&dir, |s| {
            s.pending.insert("a".into(), Pending { version: "2.0.0".into(), previous: "1.0.0".into() });
            s.blocked.entry("a".into()).or_default().push("2.0.0".into());
        })
        .unwrap();
        let s = read(&dir);
        assert_eq!(s.pending["a"].previous, "1.0.0");
        assert_eq!(s.blocked["a"], ["2.0.0"]);
        // a damaged file is not fatal
        std::fs::write(dir.join("state.json"), "{ not json").unwrap();
        assert_eq!(read(&dir), State::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
