//! Starting and stopping apps as systemd units.
//!
//! Each app runs as `kiosk-app@<id>.service`, a template unit from the rootfs
//! overlay. Before a start, a drop-in under /run/systemd/system sets what the
//! template cannot know: the program, its arguments and its permissions.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

pub use kpkg::{Manifest, valid_id};

pub fn store_dir() -> PathBuf {
    std::env::var_os("KIOSKD_STORE").map(PathBuf::from).unwrap_or_else(|| "/data/store".into())
}

fn unit_dir() -> PathBuf {
    std::env::var_os("KIOSKD_UNIT_DIR").map(PathBuf::from).unwrap_or_else(|| "/run/systemd/system".into())
}

/// Quotes one word for a systemd `ExecStart=` or `Environment=` line.
fn quote(word: &str) -> Result<String> {
    if word.chars().any(|c| c.is_control()) {
        bail!("control character in {word:?}");
    }
    Ok(format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%").replace('$', "$$")))
}

/// The drop-in text for one app.
pub fn drop_in(manifest: &Manifest, entry: &Path) -> Result<String> {
    let mut text = String::from("[Service]\nExecStart=\n");
    let mut line = quote(entry.to_str().ok_or_else(|| anyhow!("entry path is not UTF-8"))?)?;
    for a in &manifest.args {
        line.push(' ');
        line.push_str(&quote(a)?);
    }
    text += &format!("ExecStart={line}\n");
    for kv in &manifest.env {
        if !kv.contains('=') || kv.starts_with('=') {
            bail!("env entry {kv:?} is not KEY=value");
        }
        text += &format!("Environment={}\n", quote(kv)?);
    }
    for p in &manifest.permissions {
        match p.as_str() {
            "network" => text += "PrivateNetwork=no\n",
            "audio" => text += "SupplementaryGroups=audio\n",
            other => bail!("permission {other:?} is not supported yet"),
        }
    }
    Ok(text)
}

/// Reads the manifest of the active version and finds its program.
fn resolve(id: &str) -> Result<(Manifest, PathBuf)> {
    let app_dir = store_dir().join("apps").join(id);
    let current = app_dir.join("current");
    let text = std::fs::read_to_string(current.join("manifest.toml")).with_context(|| format!("{id} is not installed"))?;
    let manifest = Manifest::parse(&text)?;
    if manifest.id != id {
        bail!("manifest is for {}, not {id}", manifest.id);
    }
    let root = app_dir.canonicalize()?;
    let entry = current.join(&manifest.entry).canonicalize().with_context(|| format!("entry {} is missing", manifest.entry))?;
    if !entry.starts_with(&root) {
        bail!("entry {} leaves the app directory", manifest.entry);
    }
    let meta = std::fs::metadata(&entry)?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        bail!("entry {} is not an executable file", manifest.entry);
    }
    Ok((manifest, entry))
}

fn systemctl(args: &[&str]) -> Result<std::process::Output> {
    let bin = std::env::var("KIOSKD_SYSTEMCTL").unwrap_or_else(|_| "systemctl".into());
    Command::new(bin).args(args).output().context("running systemctl")
}

fn systemctl_ok(args: &[&str]) -> Result<()> {
    let out = systemctl(args)?;
    if out.status.success() {
        return Ok(());
    }
    bail!("systemctl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim())
}

/// Instances of kiosk-app@.service that are running now.
pub fn running_apps() -> Vec<String> {
    let Ok(out) = systemctl(&["list-units", "kiosk-app@*.service", "--state=active,activating,deactivating", "--plain", "--no-legend"]) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|u| u.strip_prefix("kiosk-app@")?.strip_suffix(".service"))
        .map(str::to_string)
        .collect()
}

/// The app's writable state directory, owned by the kiosk user.
fn prepare_data_dir(id: &str) -> Result<()> {
    let dir = store_dir().join("data").join(id);
    std::fs::create_dir_all(&dir)?;
    let user = std::ffi::CString::new("kiosk")?;
    let pw = unsafe { libc::getpwnam(user.as_ptr()) };
    if pw.is_null() {
        // not on a real kiosk (tests, desktop): leave the ownership alone
        return Ok(());
    }
    let (uid, gid) = unsafe { ((*pw).pw_uid, (*pw).pw_gid) };
    let c = std::ffi::CString::new(dir.to_str().ok_or_else(|| anyhow!("data path is not UTF-8"))?)?;
    if unsafe { libc::chown(c.as_ptr(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("chown data dir");
    }
    Ok(())
}

pub fn launch(id: &str) -> Result<()> {
    if !valid_id(id) {
        bail!("invalid app id");
    }
    let running = running_apps();
    if !running.is_empty() {
        bail!("{} is already running", running.join(", "));
    }
    let (manifest, entry) = resolve(id)?;
    let text = drop_in(&manifest, &entry)?;
    prepare_data_dir(id)?;

    let dir = unit_dir().join(format!("kiosk-app@{id}.service.d"));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("kioskd.conf"), text)?;
    systemctl_ok(&["daemon-reload"])?;
    let unit = format!("kiosk-app@{id}.service");
    // a start limit left over from an earlier crash loop would refuse this start
    let _ = systemctl(&["reset-failed", &unit]);
    systemctl_ok(&["start", &unit])?;
    log::info!("launched {id}");
    Ok(())
}

/// Stops the running app. kiosk-ui comes back through the unit's OnSuccess=,
/// and is started here as well in case the stop was not counted as a success.
pub fn stop() -> Result<()> {
    let running = running_apps();
    for id in &running {
        systemctl_ok(&["stop", &format!("kiosk-app@{id}.service")])?;
    }
    systemctl_ok(&["start", "--no-block", "kiosk.service"])?;
    log::info!("stopped {}", if running.is_empty() { "nothing".into() } else { running.join(", ") });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(toml: &str) -> Manifest {
        let mut t = toml.to_string();
        // the launcher tests only care about the fields they set
        if !t.contains("version") {
            t += "\nversion='1.0.0'\nname='T'";
        }
        let m: Manifest = toml::from_str(&t).unwrap();
        m
    }

    #[test]
    fn drop_in_sets_program_and_permissions() {
        let m = manifest("id='a.b'\nentry='bin/x'\npermissions=['network','audio']\nargs=['--n','5%']\nenv=['K=v w']");
        let t = drop_in(&m, Path::new("/data/store/apps/a.b/1/bin/x")).unwrap();
        assert!(t.contains("ExecStart=\nExecStart=\"/data/store/apps/a.b/1/bin/x\" \"--n\" \"5%%\"\n"), "{t}");
        assert!(t.contains("Environment=\"K=v w\"\n"));
        assert!(t.contains("PrivateNetwork=no\n") && t.contains("SupplementaryGroups=audio\n"));
    }

    #[test]
    fn no_permissions_keeps_the_template_sandbox() {
        let t = drop_in(&manifest("id='a'\nentry='x'"), Path::new("/x")).unwrap();
        assert!(!t.contains("PrivateNetwork"));
    }

    #[test]
    fn bad_manifest_values_are_refused() {
        let p = Path::new("/x");
        assert!(drop_in(&manifest("id='a'\nentry='x'\npermissions=['root']"), p).is_err());
        assert!(drop_in(&manifest("id='a'\nentry='x'\nargs=[\"a\\nb\"]"), p).is_err());
        assert!(drop_in(&manifest("id='a'\nentry='x'\nenv=['noequals']"), p).is_err());
    }

    #[test]
    fn entry_must_stay_inside_the_app() {
        let tmp = std::env::temp_dir().join(format!("kioskd-test-{}", std::process::id()));
        let app = tmp.join("apps/t.app/1.0");
        std::fs::create_dir_all(app.join("bin")).unwrap();
        std::fs::write(tmp.join("outside"), "").unwrap();
        std::fs::write(app.join("bin/ok"), "").unwrap();
        std::fs::set_permissions(app.join("bin/ok"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("1.0", tmp.join("apps/t.app/current")).unwrap();
        // SAFETY: tests that touch KIOSKD_* run in this one test
        unsafe { std::env::set_var("KIOSKD_STORE", &tmp) };

        let write = |entry: &str| std::fs::write(app.join("manifest.toml"), format!("id='t.app'\nversion='1.0.0'\nname='T'\nentry='{entry}'\n")).unwrap();
        write("bin/ok");
        assert!(resolve("t.app").is_ok());
        write("../../../outside");
        assert!(resolve("t.app").is_err(), "escape via ..");
        std::os::unix::fs::symlink(tmp.join("outside"), app.join("bin/evil")).unwrap();
        write("bin/evil");
        assert!(resolve("t.app").unwrap_err().to_string().contains("leaves"), "escape via a symlink");
        write("bin/missing");
        assert!(resolve("t.app").is_err());
        assert!(resolve("other.app").is_err());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
