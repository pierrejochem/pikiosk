//! The admin PIN: a lockout that survives a power cut, and a short-lived
//! token that kiosk-ui holds after a correct PIN.
//!
//! The PIN lives in a root-only file under /run/kiosk (written by
//! apply-config from STORE_PIN); kiosk-ui never sees it. After 3 wrong PINs
//! every PIN is refused for `lockout`. The board has no RTC, so the remaining
//! lockout is counted on the boot clock and saved every 10 s; the wrong-PIN
//! count is saved at once. Pulling the power does not reset either.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const MAX_TRIES: u32 = 3;
/// An admin session ends this long after its last request.
pub const TOKEN_TTL: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq)]
pub enum Unlock {
    Ok(String),
    Wrong { tries_left: u32 },
    Locked { secs: u64 },
    /// STORE_PIN is not set, so nobody can be admin
    NoPin,
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    fails: u32,
    lock_remaining: u64,
}

struct Inner {
    fails: u32,
    locked_until: Option<Instant>,
    token: Option<(String, Instant)>,
}

pub struct Admin {
    file: PathBuf,
    pin: Option<String>,
    lockout: Duration,
    inner: Mutex<Inner>,
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn random_token() -> String {
    let mut b = [0u8; 16];
    File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).expect("/dev/urandom");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Admin {
    pub fn new(store: &Path, pin: Option<String>, lockout: Duration, now: Instant) -> Admin {
        let file = store.join("pin.json");
        let saved: Saved = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let locked_until = (saved.lock_remaining > 0).then(|| now + Duration::from_secs(saved.lock_remaining));
        Admin { file, pin, lockout, inner: Mutex::new(Inner { fails: saved.fails, locked_until, token: None }) }
    }

    fn save(&self, i: &Inner, now: Instant) {
        let remaining = i.locked_until.map_or(0, |t| t.saturating_duration_since(now).as_secs() + 1);
        let tmp = self.file.with_extension("new");
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = File::create(&tmp)?;
            f.write_all(&serde_json::to_vec(&Saved { fails: i.fails, lock_remaining: remaining }).unwrap())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.file)
        })();
        if let Err(e) = result {
            log::warn!("could not save the PIN state: {e}");
        }
    }

    fn locked_secs(i: &Inner, now: Instant) -> u64 {
        i.locked_until.map_or(0, |t| (t.saturating_duration_since(now).as_secs_f64().ceil()) as u64)
    }

    pub fn unlock(&self, pin: &str, now: Instant) -> Unlock {
        let Some(expected) = &self.pin else { return Unlock::NoPin };
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.locked_until.is_some_and(|t| now >= t) {
            i.locked_until = None;
            i.fails = 0;
        }
        let secs = Self::locked_secs(&i, now);
        if secs > 0 {
            return Unlock::Locked { secs };
        }
        if same(pin, expected) {
            i.fails = 0;
            let token = random_token();
            i.token = Some((token.clone(), now + TOKEN_TTL));
            self.save(&i, now);
            return Unlock::Ok(token);
        }
        i.fails += 1;
        i.token = None;
        if i.fails >= MAX_TRIES {
            i.locked_until = Some(now + self.lockout);
            self.save(&i, now);
            return Unlock::Locked { secs: self.lockout.as_secs() };
        }
        self.save(&i, now);
        Unlock::Wrong { tries_left: MAX_TRIES - i.fails }
    }

    /// True for the current admin token; every use extends the session.
    pub fn valid(&self, token: &str, now: Instant) -> bool {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match &mut i.token {
            Some((t, expires)) if same(t, token) && now < *expires => {
                *expires = now + TOKEN_TTL;
                true
            }
            _ => false,
        }
    }

    pub fn lock(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).token = None;
    }

    pub fn lockout_secs(&self) -> u64 {
        self.lockout.as_secs()
    }

    pub fn pin_set(&self) -> bool {
        self.pin.is_some()
    }

    pub fn locked_for(&self, now: Instant) -> u64 {
        Self::locked_secs(&self.inner.lock().unwrap_or_else(|e| e.into_inner()), now)
    }

    pub fn tries_left(&self) -> u32 {
        MAX_TRIES.saturating_sub(self.inner.lock().unwrap_or_else(|e| e.into_inner()).fails)
    }

    /// Saves the remaining lockout; called every 10 s while locked.
    pub fn tick(&self, now: Instant) {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.locked_until.is_some_and(|t| now < t) {
            self.save(&i, now);
        }
    }
}

static GLOBAL: OnceLock<Admin> = OnceLock::new();

/// The daemon's admin: PIN from KIOSKD_PIN_FILE, lockout from KIOSKD_PIN_LOCKOUT.
pub fn global() -> &'static Admin {
    GLOBAL.get_or_init(|| {
        let file = std::env::var("KIOSKD_PIN_FILE").unwrap_or_else(|_| "/run/kiosk/store.pin".into());
        let pin = std::fs::read_to_string(file).ok().map(|s| s.trim().to_string()).filter(|p| (4..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit()));
        let lockout = std::env::var("KIOSKD_PIN_LOCKOUT").ok().and_then(|v| v.parse::<u64>().ok()).filter(|s| *s >= 1).unwrap_or(300);
        let store = crate::install::Paths::from_env().store;
        let admin = Admin::new(&store, pin, Duration::from_secs(lockout), Instant::now());
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(10));
            global().tick(Instant::now());
        });
        admin
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("kioskd-admin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn secs(base: Instant, s: u64) -> Instant {
        base + Duration::from_secs(s)
    }

    #[test]
    fn three_wrong_pins_lock_and_the_lock_expires() {
        let d = dir("lock");
        let t0 = Instant::now();
        let a = Admin::new(&d, Some("1234".into()), Duration::from_secs(300), t0);
        assert_eq!(a.unlock("0000", t0), Unlock::Wrong { tries_left: 2 });
        assert_eq!(a.unlock("1111", secs(t0, 1)), Unlock::Wrong { tries_left: 1 });
        assert_eq!(a.unlock("2222", secs(t0, 2)), Unlock::Locked { secs: 300 });
        // while locked even the right PIN is refused
        assert_eq!(a.unlock("1234", secs(t0, 100)), Unlock::Locked { secs: 202 });
        assert!(matches!(a.unlock("1234", secs(t0, 303)), Unlock::Ok(_)), "unlocks when the timer has run out");
        assert_eq!(a.tries_left(), 3);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_lockout_and_the_wrong_count_survive_a_power_cut() {
        let d = dir("cut");
        let t0 = Instant::now();
        let a = Admin::new(&d, Some("1234".into()), Duration::from_secs(300), t0);
        a.unlock("0000", t0);
        // power cut after one wrong PIN: the count is still 1
        let b = Admin::new(&d, Some("1234".into()), Duration::from_secs(300), t0);
        assert_eq!(b.tries_left(), 2);
        b.unlock("0000", t0);
        assert_eq!(b.unlock("0000", t0), Unlock::Locked { secs: 300 });
        // 100 s later the remaining time is saved; then the power goes
        b.tick(secs(t0, 100));
        // the board boots: the boot clock starts over, the remaining ~200 s are restored
        let t1 = Instant::now();
        let c = Admin::new(&d, Some("1234".into()), Duration::from_secs(300), t1);
        let left = c.locked_for(t1);
        assert!((199..=201).contains(&left), "restored {left}");
        assert!(matches!(c.unlock("1234", secs(t1, 10)), Unlock::Locked { .. }));
        assert!(matches!(c.unlock("1234", secs(t1, 202)), Unlock::Ok(_)));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_right_pin_gives_a_token_that_expires_and_is_extended_by_use() {
        let d = dir("token");
        let t0 = Instant::now();
        let a = Admin::new(&d, Some("12345678".into()), Duration::from_secs(300), t0);
        let Unlock::Ok(token) = a.unlock("12345678", t0) else { panic!("should unlock") };
        assert!(a.valid(&token, secs(t0, 59)));
        assert!(a.valid(&token, secs(t0, 118)), "each use extends the session");
        assert!(!a.valid(&token, secs(t0, 200)), "expired after 60 s without use");
        assert!(!a.valid("00000000000000000000000000000000", t0));
        let Unlock::Ok(t2) = a.unlock("12345678", secs(t0, 300)) else { panic!() };
        assert_ne!(t2, token);
        a.lock();
        assert!(!a.valid(&t2, secs(t0, 301)), "locked");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn no_pin_means_no_admin() {
        let d = dir("nopin");
        let a = Admin::new(&d, None, Duration::from_secs(300), Instant::now());
        assert_eq!(a.unlock("1234", Instant::now()), Unlock::NoPin);
        assert!(!a.pin_set());
        std::fs::remove_dir_all(&d).ok();
    }
}
