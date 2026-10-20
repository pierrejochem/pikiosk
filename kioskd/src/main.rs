//! kioskd: root daemon that starts store apps and brings kiosk-ui back.
//!
//!   kioskd daemon          serve /run/kioskd.sock and watch for the Home gesture
//!   kioskd launch <id>     ask the daemon to start an installed app
//!   kioskd stop            ask the daemon to stop the running app
//!
//! Environment (all optional): KIOSKD_SOCK, KIOSKD_STORE, KIOSKD_UNIT_DIR,
//! KIOSKD_UI_CONF, KIOSKD_HOME_GESTURE (yes|no), KIOSKD_HOME_HOLD_MS,
//! KIOSKD_DISPLAY (WxH).

mod home;
mod launch;
mod server;

use anyhow::{Result, bail};

fn usage() -> ! {
    eprintln!("usage: kioskd daemon | launch <id> | stop");
    std::process::exit(2);
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["daemon"] => server::run(),
        ["launch", id] => client(&format!(r#"{{"cmd":"launch","id":{}}}"#, serde_json::to_string(id)?)),
        ["stop"] => client(r#"{"cmd":"stop"}"#),
        _ => usage(),
    }
}

/// Sends one request to the running daemon and reports its answer.
fn client(request: &str) -> Result<()> {
    let reply = server::request(&server::sock_path(), request)?;
    if reply.ok {
        return Ok(());
    }
    bail!("{}", reply.error.unwrap_or_else(|| "request failed".into()))
}
