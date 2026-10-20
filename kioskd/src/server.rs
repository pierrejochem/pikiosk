//! The control socket: one JSON request per line, one JSON reply per line.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{home, install, launch};

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    Launch { id: String },
    Stop,
    Install { path: String },
    Remove { id: String },
    List,
    Local,
    Overview,
}

#[derive(Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

pub fn sock_path() -> PathBuf {
    std::env::var_os("KIOSKD_SOCK").map(PathBuf::from).unwrap_or_else(|| "/run/kioskd.sock".into())
}

fn handle(line: &str) -> Reply {
    let paths = install::Paths::from_env();
    let json = |v: serde_json::Result<serde_json::Value>| v.map(Some).map_err(anyhow::Error::from);
    let result: anyhow::Result<Option<serde_json::Value>> = match serde_json::from_str::<Request>(line) {
        Ok(Request::Launch { id }) => launch::launch(&id).map(|()| None),
        Ok(Request::Stop) => launch::stop().map(|()| None),
        Ok(Request::Install { path }) => install::install(&paths, &path, &launch::running_apps()).and_then(|a| json(serde_json::to_value(a))),
        Ok(Request::Remove { id }) => install::remove(&paths, &id, &launch::running_apps()).map(|()| None),
        Ok(Request::List) => json(serde_json::to_value(install::installed(&paths))),
        Ok(Request::Local) => json(serde_json::to_value(install::local(&paths))),
        Ok(Request::Overview) => json(Ok(serde_json::json!({
            "allow_unsigned": paths.allow_unsigned,
            "board": paths.board,
            "apps": install::installed(&paths),
            "local": install::local(&paths),
        }))),
        Err(e) => Err(anyhow::anyhow!("bad request: {e}")),
    };
    match result {
        Ok(data) => Reply { ok: true, error: None, data },
        Err(e) => {
            log::warn!("request failed: {e:#}");
            Reply { ok: false, error: Some(format!("{e:#}")), data: None }
        }
    }
}

fn serve(stream: UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let reply = handle(&line);
    let mut out = &stream;
    writeln!(out, "{}", serde_json::to_string(&reply)?)?;
    Ok(())
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

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Err(e) = serve(s) {
                    log::warn!("connection: {e:#}");
                }
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
    fn requests_parse_and_bad_ones_are_refused() {
        let r = handle(r#"{"cmd":"launch","id":"../etc"}"#);
        assert!(!r.ok && r.error.unwrap().contains("invalid app id"));
        let r = handle(r#"{"cmd":"format"}"#);
        assert!(!r.ok && r.error.unwrap().contains("bad request"));
        assert!(!handle("not json").ok);
    }
}
