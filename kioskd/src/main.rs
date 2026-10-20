//! kioskd: root daemon that starts store apps and brings kiosk-ui back.
//!
//!   kioskd daemon          serve /run/kioskd.sock and watch for the Home gesture
//!   kioskd launch <id>     ask the daemon to start an installed app
//!   kioskd stop            ask the daemon to stop the running app
//!   kioskd install <file>  install a .kpkg from an install directory
//!   kioskd remove <id>     remove an app (its data stays)
//!   kioskd list | local    installed apps | packages on local media
//!   kioskd refresh         sync the signed store catalog now
//!   kioskd get <id>        download, verify and install an app from the store
//!
//! Environment (all optional): KIOSKD_SOCK, KIOSKD_STORE, KIOSKD_UNIT_DIR,
//! KIOSKD_UI_CONF, KIOSKD_ALLOW_UNSIGNED, KIOSKD_STORE_URL, KIOSKD_STORE_CHANNEL, KIOSKD_ROOT_JSON, KIOSKD_CLOCK_FLAG, KIOSKD_INSTALL_DIRS, KIOSKD_BOARD_FILE, KIOSKD_HOME_GESTURE (yes|no), KIOSKD_HOME_HOLD_MS,
//! KIOSKD_DISPLAY (WxH).

mod home;
mod install;
mod launch;
mod server;
mod store;

use anyhow::{Result, bail};

fn usage() -> ! {
    eprintln!("usage: kioskd daemon | launch <id> | stop | install <file> | remove <id> | list | local | refresh | get <id>");
    std::process::exit(2);
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["daemon"] => server::run(),
        ["launch", id] => client(&format!(r#"{{"cmd":"launch","id":{}}}"#, serde_json::to_string(id)?)),
        ["stop"] => client(r#"{"cmd":"stop"}"#),
        ["install", file] => client(&format!(r#"{{"cmd":"install","path":{}}}"#, serde_json::to_string(file)?)),
        ["remove", id] => client(&format!(r#"{{"cmd":"remove","id":{}}}"#, serde_json::to_string(id)?)),
        ["list"] => client(r#"{"cmd":"list"}"#),
        ["local"] => client(r#"{"cmd":"local"}"#),
        ["refresh"] => client(r#"{"cmd":"store_refresh"}"#),
        ["get", id] => client(&format!(r#"{{"cmd":"store_install","id":{}}}"#, serde_json::to_string(id)?)),
        _ => usage(),
    }
}

/// Sends one request to the running daemon and reports its answer.
fn client(request: &str) -> Result<()> {
    let reply = server::request(&server::sock_path(), request)?;
    if reply.ok {
        if let Some(data) = reply.data {
            println!("{}", serde_json::to_string_pretty(&data)?);
        }
        return Ok(());
    }
    bail!("{}", reply.error.unwrap_or_else(|| "request failed".into()))
}
