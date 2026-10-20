//! The health gate: a freshly installed or updated version has to stay up for
//! 30 s the first time it runs. If it crashes instead, kioskd switches back to
//! the previous version, blocks the bad one and leaves a notice for the admin.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;

use crate::install::{self, Paths};
use crate::launch;
use crate::state::{self, Notice};

/// Samples per second times seconds: the gate polls twice a second for 30 s.
pub const GATE_TICKS: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Running,
    /// failed, or waiting to be restarted after a failure
    Crashed,
    /// ended without a failure: the app exited with status 0 or was stopped
    Exited,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// stayed up for the whole gate
    Good,
    Failed,
    /// closed cleanly before the gate was over: no verdict, it stays pending
    Inconclusive,
}

pub fn judge(mut poll: impl FnMut() -> Unit, mut wait: impl FnMut(), ticks: u32) -> Verdict {
    for _ in 0..ticks {
        match poll() {
            Unit::Crashed => return Verdict::Failed,
            Unit::Exited => return Verdict::Inconclusive,
            Unit::Running => {}
        }
        wait();
    }
    Verdict::Good
}

/// Maps `systemctl show` output (ActiveState, SubState, Result) to a state.
pub fn parse_unit(show: &str) -> Unit {
    let get = |k: &str| show.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).unwrap_or("");
    match (get("ActiveState"), get("SubState"), get("Result")) {
        ("failed", _, _) => Unit::Crashed,
        ("activating", "auto-restart", _) => Unit::Crashed,
        ("inactive", _, "success") => Unit::Exited,
        ("inactive", _, _) => Unit::Crashed,
        _ => Unit::Running,
    }
}

fn unit_state(id: &str) -> Unit {
    let out = std::process::Command::new(std::env::var("KIOSKD_SYSTEMCTL").unwrap_or_else(|_| "systemctl".into()))
        .args(["show", "-p", "ActiveState", "-p", "SubState", "-p", "Result", &format!("kiosk-app@{id}.service")])
        .output();
    match out {
        Ok(o) => parse_unit(&String::from_utf8_lossy(&o.stdout)),
        Err(_) => Unit::Running,
    }
}

static WATCHING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Called after an app has been started: watches it if its version is pending.
pub fn watch_if_pending(id: &str) {
    let paths = Paths::from_env();
    let current = install::current_version(&paths, id);
    let pending = state::read(&paths.store).pending.get(id).cloned();
    let (Some(current), Some(pending)) = (current, pending) else { return };
    if pending.version != current {
        return;
    }
    if !WATCHING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(id.to_string()) {
        return;
    }
    let id = id.to_string();
    std::thread::spawn(move || {
        log::info!("health gate: watching {id} {current} for {} s", GATE_TICKS / 2);
        let verdict = judge(|| unit_state(&id), || std::thread::sleep(Duration::from_millis(500)), GATE_TICKS);
        if let Err(e) = conclude(&Paths::from_env(), &id, &current, verdict) {
            log::warn!("health gate for {id}: {e:#}");
        }
        WATCHING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).remove(&id);
    });
}

/// Acts on the verdict: a good version is no longer pending, a failed one is rolled back.
pub fn conclude(p: &Paths, id: &str, version: &str, verdict: Verdict) -> Result<()> {
    match verdict {
        Verdict::Good => {
            state::update(&p.store, |s| {
                if s.pending.get(id).is_some_and(|x| x.version == version) {
                    s.pending.remove(id);
                }
            })?;
            log::info!("health gate: {id} {version} is good");
        }
        Verdict::Inconclusive => log::info!("health gate: {id} {version} closed early, it stays pending"),
        Verdict::Failed => {
            log::warn!("health gate: {id} {version} failed, rolling back");
            // stop the restart loop and bring kiosk-ui back before touching the files
            let _ = launch::stop();
            let name = install::app_info(p, id).map(|a| a.name).unwrap_or_else(|| id.to_string());
            let (from, to) = install::rollback(p, id, &[], true)?;
            let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            state::update(&p.store, |s| {
                s.pending.remove(id);
                s.blocked.entry(id.to_string()).or_default().push(from.clone());
                s.notices.retain(|n| n.id != id);
                s.notices.push(Notice { id: id.to_string(), name, from, to, reason: format!("closed during its {} s health check", GATE_TICKS / 2), at });
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_judges_what_the_unit_does() {
        let mut n = 0;
        let seq = |outcome: Unit, at: u32, n: &mut u32| {
            *n += 1;
            if *n > at { outcome } else { Unit::Running }
        };
        assert_eq!(judge(|| seq(Unit::Crashed, 3, &mut n), || {}, 60), Verdict::Failed);
        n = 0;
        assert_eq!(judge(|| seq(Unit::Exited, 10, &mut n), || {}, 60), Verdict::Inconclusive);
        assert_eq!(judge(|| Unit::Running, || {}, 60), Verdict::Good);
    }

    #[test]
    fn systemctl_output_maps_to_unit_states() {
        let show = |a: &str, s: &str, r: &str| parse_unit(&format!("ActiveState={a}\nSubState={s}\nResult={r}\n"));
        assert_eq!(show("active", "running", "success"), Unit::Running);
        assert_eq!(show("activating", "start", "success"), Unit::Running);
        assert_eq!(show("activating", "auto-restart", "exit-code"), Unit::Crashed, "waiting to restart after a crash");
        assert_eq!(show("failed", "failed", "exit-code"), Unit::Crashed);
        assert_eq!(show("inactive", "dead", "success"), Unit::Exited);
        assert_eq!(show("inactive", "dead", "signal"), Unit::Crashed);
        assert_eq!(show("deactivating", "stop-sigterm", "success"), Unit::Running);
    }
}
