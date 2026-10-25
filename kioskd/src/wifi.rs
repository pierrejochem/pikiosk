//! WiFi for kiosk-ui: scan, status, connect and forget, through wpa_supplicant's
//! `wpa_cli` (the control socket is root-only, and this daemon is root). Networks the
//! user adds are kept in /data/store/wifi.json and merged into the wpa_supplicant
//! config at boot (`kioskd wifi-conf`), because the rootfs is read-only and the
//! generated config in /run is rebuilt from kiosk.conf on every boot.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// One writer at a time: connect and forget change both wpa_supplicant and the file.
static LOCK: Mutex<()> = Mutex::new(());

/// A network the user added from the touchscreen.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Saved {
    pub ssid: String,
    /// None for an open network
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psk: Option<String>,
    #[serde(default)]
    pub hidden: bool,
}

/// A network seen in a scan.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Network {
    pub ssid: String,
    /// 0..100
    pub signal: u8,
    pub secured: bool,
    pub saved: bool,
    pub connected: bool,
}

/// How to reach wpa_supplicant.
pub struct Wpa {
    program: PathBuf,
    iface: String,
    /// where wpa_cli puts its own socket. wpa_supplicant answers to that path from the host's
    /// mount namespace, so under kioskd's PrivateTmp the default (/tmp) never gets an answer.
    sockets: Option<PathBuf>,
    /// how long a connection may take to come up
    pub timeout: Duration,
    /// how long to let a scan run before reading the results
    pub scan_wait: Duration,
}

impl Wpa {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Wpa {
            program: var("KIOSKD_WPA_CLI").map(PathBuf::from).unwrap_or_else(|| "wpa_cli".into()),
            iface: var("KIOSKD_WIFI_IFACE").unwrap_or_else(|| "wlan0".into()),
            sockets: Some(var("KIOSKD_WPA_SOCKDIR").unwrap_or_else(|| "/run/kioskd".into())).filter(|d| !d.is_empty()).map(PathBuf::from),
            timeout: Duration::from_secs(25),
            scan_wait: Duration::from_secs(3),
        }
    }

    /// Runs one wpa_cli command. The arguments go to the program as they are, never through a shell.
    fn run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(&self.program);
        if let Some(dir) = &self.sockets {
            cmd.arg("-s").arg(dir);
        }
        let out = cmd.arg("-i").arg(&self.iface).args(args).output().with_context(|| format!("running {}", self.program.display()))?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || text.starts_with("FAIL") {
            // never echo the arguments: they can hold the password
            bail!("wpa_cli {} failed: {}", args.first().unwrap_or(&""), if text.is_empty() { "no answer from wpa_supplicant" } else { &text });
        }
        Ok(text)
    }

    fn ok(&self, args: &[&str]) -> Result<()> {
        self.run(args).map(|_| ())
    }
}

/// Turns wpa_supplicant's escaped SSID (`\xHH`, `\\`, `\"`) back into text.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            match b[i + 1] {
                b'x' if b.len() >= i + 4 => {
                    if let Some(v) = std::str::from_utf8(&b[i + 2..i + 4]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                }
                c @ (b'\\' | b'"') => {
                    out.push(c);
                    i += 2;
                    continue;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// dBm to 0..100 (-100 dBm or worse is 0, -50 dBm or better is 100)
fn quality(dbm: i32) -> u8 {
    (2 * (dbm + 100)).clamp(0, 100) as u8
}

/// Parses `scan_results`: bssid, frequency, signal, flags, ssid, tab separated.
fn parse_scan(text: &str) -> Vec<(String, u8, bool)> {
    let mut best: Vec<(String, u8, bool)> = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            continue;
        }
        let Ok(dbm) = f[2].trim().parse::<i32>() else { continue };
        let ssid = unescape(f[4]);
        // hidden networks show up with an empty or all-NUL name
        if ssid.trim_matches('\0').is_empty() {
            continue;
        }
        let secured = ["WPA", "WEP", "SAE", "RSN"].iter().any(|k| f[3].contains(k));
        let q = quality(dbm);
        match best.iter_mut().find(|(s, _, _)| *s == ssid) {
            Some(e) => {
                if q > e.1 {
                    e.1 = q;
                    e.2 = secured;
                }
            }
            None => best.push((ssid, q, secured)),
        }
    }
    best
}

/// The current connection: (ssid, state, ip address)
pub struct Status {
    pub state: String,
    pub ssid: Option<String>,
    pub ip: Option<String>,
}

fn parse_status(text: &str) -> Status {
    let mut s = Status { state: String::new(), ssid: None, ip: None };
    for line in text.lines() {
        match line.split_once('=') {
            Some(("wpa_state", v)) => s.state = v.to_string(),
            Some(("ssid", v)) => s.ssid = Some(unescape(v)),
            Some(("ip_address", v)) => s.ip = Some(v.to_string()),
            _ => {}
        }
    }
    s
}

impl Status {
    pub fn connected(&self) -> bool {
        self.state == "COMPLETED"
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "state": self.state, "connected": self.connected(), "ssid": self.ssid, "ip": self.ip })
    }
}

pub fn status(wpa: &Wpa) -> Result<Status> {
    Ok(parse_status(&wpa.run(&["status"])?))
}

fn saved_path(store: &Path) -> PathBuf {
    store.join("wifi.json")
}

pub fn saved(store: &Path) -> Vec<Saved> {
    std::fs::read(saved_path(store)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Writes the list atomically; it holds passwords, so only root may read it.
fn write_saved(store: &Path, list: &[Saved]) -> Result<()> {
    std::fs::create_dir_all(store)?;
    let (tmp, dest) = (store.join("wifi.json.new"), saved_path(store));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(list)?)?;
    file.sync_all()?;
    std::fs::rename(&tmp, &dest)?;
    File::open(store)?.sync_all()?;
    Ok(())
}

/// Scans and lists what is in range, the connected network first.
pub fn scan(wpa: &Wpa, store: &Path) -> Result<serde_json::Value> {
    // a scan that is still running answers FAIL-BUSY; the results of the last one are fine then
    let _ = wpa.run(&["scan"]);
    std::thread::sleep(wpa.scan_wait);
    let seen = parse_scan(&wpa.run(&["scan_results"])?);
    let connected = status(wpa).ok().filter(Status::connected).and_then(|s| s.ssid);
    let known: BTreeSet<String> = saved(store).into_iter().map(|s| s.ssid).collect();
    let mut list: Vec<Network> = seen
        .into_iter()
        .map(|(ssid, signal, secured)| Network { saved: known.contains(&ssid), connected: connected.as_deref() == Some(&ssid), ssid, signal, secured })
        .collect();
    list.sort_by(|a, b| b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)).then(a.ssid.cmp(&b.ssid)));
    Ok(serde_json::json!({ "networks": list, "connected": connected }))
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

/// Checks what the touchscreen sent before it goes anywhere near wpa_supplicant.
fn validate(ssid: &str, psk: &Option<String>) -> Result<()> {
    if ssid.is_empty() || ssid.len() > 32 || has_control(ssid) {
        bail!("the network name must be 1 to 32 characters");
    }
    if let Some(p) = psk
        && (!(8..=63).contains(&p.len()) || has_control(p))
    {
        bail!("the password must be 8 to 63 characters");
    }
    Ok(())
}

/// The ids of the networks wpa_supplicant has with this name.
fn network_ids(wpa: &Wpa, ssid: &str) -> Result<Vec<String>> {
    let text = wpa.run(&["list_networks"])?;
    Ok(text
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 2 && unescape(f[1]) == ssid).then(|| f[0].to_string())
        })
        .collect())
}

/// Adds the network, waits until it is connected and remembers it. A wrong password
/// leaves the networks that worked before as they were.
pub fn connect(wpa: &Wpa, store: &Path, ssid: &str, psk: Option<String>, hidden: bool) -> Result<serde_json::Value> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // no password sent: connect again with the one saved for this name (a saved network tapped on the screen)
    let known = saved(store).into_iter().find(|s| s.ssid == ssid);
    let hidden = hidden || known.as_ref().is_some_and(|s| s.hidden);
    let psk = psk.filter(|p| !p.is_empty()).or_else(|| known.and_then(|s| s.psk));
    validate(ssid, &psk)?;

    for id in network_ids(wpa, ssid)? {
        wpa.ok(&["remove_network", &id])?;
    }
    let id = wpa.run(&["add_network"])?.lines().last().unwrap_or("").trim().to_string();
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        bail!("wpa_supplicant did not add the network");
    }
    let configure = || -> Result<()> {
        wpa.ok(&["set_network", &id, "ssid", &hex(ssid)])?;
        match &psk {
            Some(p) => {
                wpa.ok(&["set_network", &id, "key_mgmt", "WPA-PSK WPA-PSK-SHA256 SAE"])?;
                wpa.ok(&["set_network", &id, "ieee80211w", "1"])?;
                wpa.ok(&["set_network", &id, "psk", &format!("\"{p}\"")])?;
            }
            None => wpa.ok(&["set_network", &id, "key_mgmt", "NONE"])?,
        }
        if hidden {
            wpa.ok(&["set_network", &id, "scan_ssid", "1"])?;
        }
        wpa.ok(&["set_network", &id, "priority", "10"])?;
        wpa.ok(&["select_network", &id])
    };
    let up = configure().and_then(|()| {
        let start = Instant::now();
        loop {
            let s = status(wpa)?;
            if s.connected() && s.ssid.as_deref() == Some(ssid) {
                return Ok(s);
            }
            if start.elapsed() > wpa.timeout {
                bail!("could not connect to {ssid}, check the password");
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
    let status = match up {
        Ok(s) => s,
        Err(e) => {
            // back to how it was: drop the new network and let the others try again
            let _ = wpa.ok(&["remove_network", &id]);
            let _ = wpa.ok(&["enable_network", "all"]);
            return Err(e);
        }
    };
    // the other networks stay as fallbacks, but this one has the priority
    let _ = wpa.ok(&["enable_network", "all"]);

    let mut list = saved(store);
    list.retain(|s| s.ssid != ssid);
    list.push(Saved { ssid: ssid.to_string(), psk, hidden });
    write_saved(store, &list).context("saving the network")?;
    Ok(status.to_json())
}

/// Removes a network from wpa_supplicant and from the saved list.
pub fn forget(wpa: &Wpa, store: &Path, ssid: &str) -> Result<()> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = saved(store);
    let before = list.len();
    list.retain(|s| s.ssid != ssid);
    if list.len() != before {
        write_saved(store, &list).context("saving the network list")?;
    }
    // wpa_supplicant may be down (no WiFi on this board); the saved list is what counts
    if let Ok(ids) = network_ids(wpa, ssid) {
        for id in ids {
            let _ = wpa.ok(&["remove_network", &id]);
        }
    }
    Ok(())
}

/// The networks from the saved list as wpa_supplicant.conf blocks, for the boot-time merge.
pub fn conf_blocks(store: &Path) -> String {
    let mut out = String::new();
    for s in saved(store) {
        // a name or password with a line break would end the block early
        if validate(&s.ssid, &s.psk).is_err() {
            continue;
        }
        out += &format!("network={{\n\tssid={}\n\tpriority=10\n", hex(&s.ssid));
        if s.hidden {
            out += "\tscan_ssid=1\n";
        }
        match &s.psk {
            Some(p) => out += &format!("\tkey_mgmt=WPA-PSK WPA-PSK-SHA256 SAE\n\tieee80211w=1\n\tpsk=\"{p}\"\n"),
            None => out += "\tkey_mgmt=NONE\n",
        }
        out += "}\n";
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCAN: &str = "bssid / frequency / signal level / flags / ssid\n\
        aa:bb:cc:00:00:01\t2412\t-45\t[WPA2-PSK-CCMP][ESS]\tHome\n\
        aa:bb:cc:00:00:02\t5180\t-70\t[WPA2-PSK-CCMP][ESS]\tHome\n\
        aa:bb:cc:00:00:03\t2437\t-82\t[ESS]\tCafe\\x20Guest\n\
        aa:bb:cc:00:00:04\t2437\t-60\t[WPA2-PSK-CCMP][ESS]\t\n";

    #[test]
    fn scan_results_are_merged_and_unescaped() {
        let got = parse_scan(SCAN);
        assert_eq!(got, vec![("Home".to_string(), 100, true), ("Cafe Guest".to_string(), 36, false)]);
        assert_eq!(unescape(r#"a\\b\"c\x41"#), r#"a\b"cA"#);
        assert_eq!(quality(-120), 0);
        assert_eq!(quality(-30), 100);
    }

    #[test]
    fn input_is_checked_before_it_reaches_wpa_supplicant() {
        assert!(validate("Home", &Some("12345678".into())).is_ok());
        assert!(validate("Open", &None).is_ok());
        assert!(validate("", &None).is_err());
        assert!(validate(&"x".repeat(33), &None).is_err());
        assert!(validate("a\nb", &None).is_err());
        assert!(validate("Home", &Some("short".into())).is_err());
        assert!(validate("Home", &Some("12345678\n}network={".into())).is_err());
    }

    #[test]
    fn saved_networks_round_trip_and_render_as_config() {
        let dir = std::env::temp_dir().join(format!("kioskd-wifi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(saved(&dir).is_empty());
        let list = vec![
            Saved { ssid: "Home".into(), psk: Some("hunter2hunter2".into()), hidden: false },
            Saved { ssid: "Open".into(), psk: None, hidden: true },
        ];
        write_saved(&dir, &list).unwrap();
        assert_eq!(saved(&dir), list);
        let mode = std::fs::metadata(dir.join("wifi.json")).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        let conf = conf_blocks(&dir);
        assert!(conf.contains("ssid=486f6d65\n") && conf.contains("psk=\"hunter2hunter2\""), "{conf}");
        assert!(conf.contains("key_mgmt=NONE") && conf.contains("scan_ssid=1"), "{conf}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A wpa_cli that answers like wpa_supplicant and writes the commands it got to a log.
    fn stub(dir: &Path, state: &str) -> Wpa {
        std::fs::create_dir_all(dir).unwrap();
        let script = dir.join("wpa_cli");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho \"$3 $4 $5 $6\" >> \"{log}\"\ncase \"$3\" in\n\
                 add_network) echo 7;;\n\
                 list_networks) printf 'network id / ssid / bssid / flags\\n3\\tHome\\tany\\t\\n';;\n\
                 status) printf 'wpa_state={state}\\nssid=Home\\nip_address=192.168.1.9\\n';;\n\
                 scan_results) cat \"{scan}\";;\n\
                 *) echo OK;;\nesac\n",
                log = dir.join("log").display(),
                scan = dir.join("scan.txt").display(),
            ),
        )
        .unwrap();
        std::fs::write(dir.join("scan.txt"), SCAN).unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        Wpa { program: script, iface: "wlan0".into(), sockets: None, timeout: Duration::from_secs(1), scan_wait: Duration::ZERO }
    }

    #[test]
    fn wpa_cli_gets_a_socket_dir_that_the_sandbox_shares() {
        let dir = std::env::temp_dir().join(format!("kioskd-wifi-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut wpa = stub(&dir, "COMPLETED");
        wpa.sockets = Some(PathBuf::from("/run/kioskd"));
        // the stub logs its arguments from the third on: after "-s DIR" that is "-i IFACE status"
        wpa.run(&["status"]).unwrap();
        let log = std::fs::read_to_string(dir.join("log")).unwrap();
        assert!(log.starts_with("-i wlan0 status"), "-s DIR comes before -i IFACE: {log}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn connecting_saves_the_network_and_a_failure_does_not() {
        let dir = std::env::temp_dir().join(format!("kioskd-wifi-stub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = dir.join("store");

        let wpa = stub(&dir.join("ok"), "COMPLETED");
        let data = connect(&wpa, &store, "Home", Some("hunter2hunter2".into()), false).unwrap();
        assert_eq!(data["connected"], true);
        assert_eq!(saved(&store), vec![Saved { ssid: "Home".into(), psk: Some("hunter2hunter2".into()), hidden: false }]);
        // tapping the saved network again sends no password: the saved one is used
        let log_before = std::fs::read_to_string(dir.join("ok/log")).unwrap();
        connect(&wpa, &store, "Home", None, false).unwrap();
        let log_after = std::fs::read_to_string(dir.join("ok/log")).unwrap();
        assert!(log_after[log_before.len()..].contains("psk"), "the saved password is set again");
        assert_eq!(saved(&store).len(), 1);
        let log = std::fs::read_to_string(dir.join("ok/log")).unwrap();
        assert!(log.contains("remove_network 3"), "the old entry for this name goes first: {log}");
        assert!(log.contains("select_network 7"), "{log}");

        let wpa = stub(&dir.join("bad"), "4WAY_HANDSHAKE");
        let e = connect(&wpa, &store, "Office", Some("wrongwrong".into()), false).unwrap_err();
        assert!(e.to_string().contains("check the password"), "{e}");
        assert_eq!(saved(&store).len(), 1, "a failed network is not kept");
        let log = std::fs::read_to_string(dir.join("bad/log")).unwrap();
        assert!(log.contains("enable_network all"), "the other networks must be tried again: {log}");

        let data = scan(&wpa, &store).unwrap();
        assert_eq!(data["networks"][0]["ssid"], "Home");
        assert_eq!(data["networks"][0]["saved"], true);

        forget(&wpa, &store, "Home").unwrap();
        assert!(saved(&store).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
