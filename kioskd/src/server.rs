//! The control socket: one JSON request per line, one JSON reply per line.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{admin, home, install, launch, progress, state, store};

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Launch { id: String },
    Stop,
    Install { path: String, token: Option<String> },
    Remove { id: String, token: Option<String> },
    Rollback { id: String, token: Option<String> },
    List,
    Local,
    Overview,
    StoreRefresh,
    StoreInstall { id: String, token: Option<String> },
    Unlock { pin: String },
    Lock,
    Keepalive { token: String },
    AckNotice { id: String },
    Progress,
}

#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl Reply {
    fn ok(data: Option<serde_json::Value>) -> Reply {
        Reply { ok: true, error: None, data }
    }

    fn err(msg: impl Into<String>, data: Option<serde_json::Value>) -> Reply {
        Reply { ok: false, error: Some(msg.into()), data }
    }
}

pub fn sock_path() -> PathBuf {
    std::env::var_os("KIOSKD_SOCK").map(PathBuf::from).unwrap_or_else(|| "/run/kioskd/kioskd.sock".into())
}

/// Installing, updating and removing need the admin: root on the device (an SSH
/// session) or a token from a correct PIN.
fn authorize(token: &Option<String>, peer_is_root: bool) -> Result<()> {
    if peer_is_root || token.as_deref().is_some_and(|t| admin::global().valid(t, Instant::now())) {
        return Ok(());
    }
    if !admin::global().pin_set() {
        anyhow::bail!("no admin PIN is set (STORE_PIN in kiosk.conf)");
    }
    anyhow::bail!("the admin PIN is needed")
}

fn admin_status() -> serde_json::Value {
    let a = admin::global();
    serde_json::json!({ "pin_set": a.pin_set(), "locked_for": a.locked_for(Instant::now()), "tries_left": a.tries_left(), "lockout": a.lockout_secs() })
}

fn handle(line: &str, peer_is_root: bool) -> Reply {
    let paths = install::Paths::from_env();
    let store_cfg = store::Config::from_env(&paths.store);
    let json = |v: serde_json::Result<serde_json::Value>| v.map(Some).map_err(anyhow::Error::from);
    let req = match serde_json::from_str::<Request>(line) {
        Ok(r) => r,
        Err(e) => return Reply::err(format!("bad request: {e}"), None),
    };
    let result: anyhow::Result<Option<serde_json::Value>> = match req {
        Request::Launch { id } => launch::launch(&id).map(|()| None),
        Request::Stop => launch::stop().map(|()| None),
        Request::Install { path, token } => authorize(&token, peer_is_root).and_then(|()| install::install(&paths, &path, &launch::running_apps())).and_then(|a| json(serde_json::to_value(a))),
        Request::Remove { id, token } => authorize(&token, peer_is_root).and_then(|()| install::remove(&paths, &id, &launch::running_apps())).map(|()| None),
        Request::Rollback { id, token } => authorize(&token, peer_is_root).and_then(|()| install::rollback(&paths, &id, &launch::running_apps(), false)).and_then(|(from, to)| json(Ok(serde_json::json!({ "from": from, "to": to })))),
        Request::List => json(serde_json::to_value(install::installed(&paths))),
        Request::Local => json(serde_json::to_value(install::local(&paths))),
        Request::Overview => json(Ok(serde_json::json!({
            "allow_unsigned": paths.allow_unsigned,
            "board": paths.board,
            "apps": install::installed(&paths),
            "local": install::local(&paths),
            "store": store::view(&store_cfg, &paths),
            "notices": state::read(&paths.store).notices,
            "admin": admin_status(),
        }))),
        Request::StoreRefresh => store::refresh(&store_cfg).and_then(|_| json(Ok(store::view(&store_cfg, &paths)))),
        Request::StoreInstall { id, token } => authorize(&token, peer_is_root).and_then(|()| {
            if !kpkg::valid_id(&id) {
                anyhow::bail!("invalid app id");
            }
            store::install_app(&store_cfg, &paths, &id, &launch::running_apps())
        })
        .and_then(|a| json(serde_json::to_value(a))),
        Request::Unlock { pin } => {
            return match admin::global().unlock(&pin, Instant::now()) {
                admin::Unlock::Ok(token) => Reply::ok(Some(serde_json::json!({ "token": token, "ttl": admin::TOKEN_TTL.as_secs() }))),
                admin::Unlock::Wrong { tries_left } => Reply::err("wrong PIN", Some(serde_json::json!({ "tries_left": tries_left }))),
                admin::Unlock::Locked { secs } => Reply::err("locked", Some(serde_json::json!({ "locked_for": secs }))),
                admin::Unlock::NoPin => Reply::err("no admin PIN is set (STORE_PIN in kiosk.conf)", None),
            };
        }
        Request::Lock => {
            admin::global().lock();
            Ok(None)
        }
        Request::Keepalive { token } => {
            if admin::global().valid(&token, Instant::now()) { Ok(None) } else { Err(anyhow::anyhow!("the admin session has ended")) }
        }
        Request::AckNotice { id } => state::update(&paths.store, |s| s.notices.retain(|n| n.id != id)).map(|()| None),
        Request::Progress => json(serde_json::to_value(progress::get())),
    };
    match result {
        Ok(data) => Reply::ok(data),
        Err(e) => {
            log::warn!("request failed: {e:#}");
            Reply::err(format!("{e:#}"), None)
        }
    }
}

/// The uid of the process on the other end of the socket.
fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred { pid: 0, uid: u32::MAX, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe { libc::getsockopt(stream.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERCRED, &mut cred as *mut _ as *mut _, &mut len) };
    (r == 0).then_some(cred.uid)
}

/// Reads one request line: at most 4 KiB and 10 s in all, so a client that dribbles
/// bytes cannot hold a thread (or memory) in a root process.
fn read_request(mut stream: &UnixStream) -> Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let start = Instant::now();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        if start.elapsed() > Duration::from_secs(10) {
            anyhow::bail!("request took too long");
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.iter().position(|b| *b == b'\n') {
                    buf.truncate(i);
                    break;
                }
                if buf.len() > 4096 {
                    anyhow::bail!("request is too long");
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(String::from_utf8(buf)?)
}

fn serve(stream: UnixStream) -> Result<()> {
    let line = read_request(&stream)?;
    let reply = handle(&line, peer_uid(&stream) == Some(0));
    let mut out = &stream;
    writeln!(out, "{}", serde_json::to_string(&reply)?)?;
    Ok(())
}

/// Connections being served; more than MAX_CONNECTIONS are dropped.
static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const MAX_CONNECTIONS: usize = 16;

struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Lets the `kiosk` group (kiosk-ui) use the socket.
fn open_to_kiosk_group(path: &Path) -> Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    let group = std::ffi::CString::new("kiosk")?;
    let gr = unsafe { libc::getgrnam(group.as_ptr()) };
    if gr.is_null() {
        log::warn!("no kiosk group, the socket stays root-only");
        return Ok(());
    }
    let c = std::ffi::CString::new(path.to_str().context("socket path is not UTF-8")?)?;
    if unsafe { libc::chown(c.as_ptr(), 0, (*gr).gr_gid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("chown socket");
    }
    Ok(())
}

pub fn run() -> Result<()> {
    let path = sock_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    open_to_kiosk_group(&path)?;
    install::recover(&install::Paths::from_env());
    log::info!("listening on {}", path.display());

    if std::env::var("KIOSKD_HOME_GESTURE").as_deref() != Ok("no") {
        std::thread::spawn(home::watch);
    } else {
        log::info!("home gesture is off");
    }

    std::thread::spawn({
        let cfg = store::Config::from_env(&install::Paths::from_env().store);
        move || store::sync_loop(cfg)
    });

    for stream in listener.incoming() {
        match stream {
            // a download must not block the next request (kiosk-ui asks for the overview meanwhile)
            Ok(s) => {
                if ACTIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= MAX_CONNECTIONS {
                    ACTIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    log::warn!("too many connections, dropping one");
                    continue;
                }
                std::thread::spawn(move || {
                    let _slot = Slot;
                    if let Err(e) = serve(s) {
                        log::warn!("connection: {e:#}");
                    }
                });
            }
            Err(e) => log::warn!("accept: {e}"),
        }
    }
    Ok(())
}

/// Sends one request line and reads the reply.
pub fn request(path: &Path, request: &str) -> Result<Reply> {
    let mut s = UnixStream::connect(path).with_context(|| format!("is kioskd running? connecting to {}", path.display()))?;
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    writeln!(s, "{request}")?;
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line)?;
    serde_json::from_str(&line).context("bad reply from kioskd")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_line_is_bounded() {
        let pair = || UnixStream::pair().unwrap();
        let (mut a, b) = pair();
        a.write_all(b"{\"cmd\":\"list\"}\nrest").unwrap();
        assert_eq!(read_request(&b).unwrap(), r#"{"cmd":"list"}"#);
        let (mut a, b) = pair();
        a.write_all(&vec![b'x'; 5000]).unwrap();
        assert!(read_request(&b).unwrap_err().to_string().contains("too long"), "no newline in 5000 bytes");
        let (a, b) = pair();
        drop(a);
        assert_eq!(read_request(&b).unwrap(), "", "a client that closes without sending");
    }

    #[test]
    fn requests_parse_and_bad_ones_are_refused() {
        let r = handle(r#"{"cmd":"launch","id":"../etc"}"#, true);
        assert!(!r.ok && r.error.unwrap().contains("invalid app id"));
        let r = handle(r#"{"cmd":"format"}"#, true);
        assert!(!r.ok && r.error.unwrap().contains("bad request"));
        assert!(!handle("not json", true).ok);
    }
}
