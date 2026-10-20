//! Installing, listing and removing apps in /data/store.
//!
//! An install never touches the running version: the package is unpacked to
//! `<version>.tmp/`, flushed to disk, renamed to `<version>/`, and only then
//! does the `current` symlink move, by an atomic rename. Pulling the power at
//! any step leaves the old version in place; leftovers are swept at start.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use kpkg::Manifest;
use serde::Serialize;

pub struct Paths {
    pub store: PathBuf,
    /// where packages may be installed from (only these directories)
    pub install_dirs: Vec<PathBuf>,
    /// this image's board (rpi3, rpi4, rpi5), when known
    pub board: Option<String>,
    pub allow_unsigned: bool,
}

impl Paths {
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Paths {
            store: var("KIOSKD_STORE").map(PathBuf::from).unwrap_or_else(|| "/data/store".into()),
            install_dirs: var("KIOSKD_INSTALL_DIRS")
                .map(|v| v.split(':').map(PathBuf::from).collect())
                .unwrap_or_else(|| vec!["/boot/apps".into()]),
            board: fs::read_to_string(var("KIOSKD_BOARD_FILE").unwrap_or_else(|| "/etc/pikiosk-board".into())).ok().map(|s| s.trim().to_string()),
            allow_unsigned: var("KIOSKD_ALLOW_UNSIGNED").as_deref() == Some("yes"),
        }
    }

    fn apps(&self) -> PathBuf {
        self.store.join("apps")
    }
}

#[derive(Serialize, Debug, PartialEq)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// absolute path of the icon, when the app has one
    pub icon: Option<String>,
    pub previous: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct LocalPkg {
    pub file: String,
    pub id: String,
    pub name: String,
    pub version: String,
    /// the installed version of this app, if any
    pub installed: Option<String>,
}

fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)?.sync_all().with_context(|| format!("syncing {}", dir.display()))
}

/// Replaces `link` with a symlink to `target` in one atomic step.
fn swap_link(link: &Path, target: &str) -> Result<()> {
    let tmp = link.with_extension("new");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(target, &tmp)?;
    fs::rename(&tmp, link)?;
    Ok(())
}

fn link_target(link: &Path) -> Option<String> {
    fs::read_link(link).ok()?.to_str().map(str::to_string)
}

/// Removes what an interrupted install or removal left behind.
pub fn recover(p: &Paths) {
    let Ok(entries) = fs::read_dir(p.apps()) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(".removing-") {
            let _ = fs::remove_dir_all(&path);
            continue;
        }
        let Ok(inner) = fs::read_dir(&path) else { continue };
        for f in inner.flatten() {
            let n = f.file_name().to_string_lossy().into_owned();
            if n.ends_with(".tmp") {
                log::info!("removing the leftovers of an interrupted install: {}", f.path().display());
                let _ = fs::remove_dir_all(f.path());
            } else if n.ends_with(".new") {
                let _ = fs::remove_file(f.path());
            }
        }
    }
}

fn within_install_dir(p: &Paths, file: &Path) -> bool {
    p.install_dirs.iter().filter_map(|d| d.canonicalize().ok()).any(|d| file.starts_with(d))
}

/// Installs a package file. `running` is the list of apps on screen now.
pub fn install(p: &Paths, file: &str, running: &[String]) -> Result<AppInfo> {
    if !p.allow_unsigned {
        bail!("installing unsigned packages is off (set STORE_ALLOW_UNSIGNED=yes in kiosk.conf)");
    }
    let file = Path::new(file).canonicalize().context("package file")?;
    if file.extension().is_none_or(|e| e != "kpkg") || !within_install_dir(p, &file) {
        bail!("{} is not a .kpkg file in an install directory", file.display());
    }
    install_package(p, &file, running, None)
}

/// Installs a package whose origin has been checked: an unsigned one from an
/// install directory, or one the TUF client downloaded and verified (then
/// `expected` is the catalog entry it must match).
pub fn install_package(p: &Paths, file: &Path, running: &[String], expected: Option<&kpkg::CatalogEntry>) -> Result<AppInfo> {
    let m = kpkg::read_manifest(file)?;
    if let Some(e) = expected {
        if m.id != e.id || m.version != e.version {
            bail!("the package is {} {}, the catalog says {} {}", m.id, m.version, e.id, e.version);
        }
        if let Some(extra) = m.permissions.iter().find(|x| !e.permissions.contains(x)) {
            bail!("the package asks for the permission {extra:?}, which the catalog does not list");
        }
    }
    if let (Some(board), false) = (&p.board, m.runs_on(p.board.as_deref().unwrap_or(""))) {
        bail!("{} does not support this board ({board})", m.name);
    }
    if running.contains(&m.id) {
        bail!("{} is running", m.name);
    }

    let app = p.apps().join(&m.id);
    fs::create_dir_all(&app)?;
    let old = link_target(&app.join("current"));
    if old.as_deref() == Some(m.version.as_str()) {
        bail!("{} {} is already installed", m.name, m.version);
    }
    let (tmp, dest) = (app.join(format!("{}.tmp", m.version)), app.join(&m.version));
    let _ = fs::remove_dir_all(&tmp);
    // an older copy of this version (kept as "previous") is replaced
    let _ = fs::remove_dir_all(&dest);

    crate::progress::update("install", 0, 1);
    kpkg::unpack_with_progress(file, &tmp, &mut |done, total| crate::progress::update("install", done, total))?;
    let entry = tmp.join(&m.entry);
    let ok = fs::metadata(&entry).is_ok_and(|md| {
        use std::os::unix::fs::PermissionsExt;
        md.is_file() && md.permissions().mode() & 0o111 != 0
    });
    if !ok {
        let _ = fs::remove_dir_all(&tmp);
        bail!("entry {} is not an executable file", m.entry);
    }

    fs::rename(&tmp, &dest)?;
    sync_dir(&app)?;
    swap_link(&app.join("current"), &m.version)?;
    if let Some(old) = &old {
        swap_link(&app.join("previous"), old)?;
    }
    sync_dir(&app)?;
    sync_dir(&p.apps())?;

    // the new version must pass its health check the first time it runs
    let previous = old.clone();
    crate::state::update(&p.store, |s| {
        if let Some(b) = s.blocked.get_mut(&m.id) {
            b.retain(|v| *v != m.version);
        }
        match &previous {
            Some(prev) => {
                s.pending.insert(m.id.clone(), crate::state::Pending { version: m.version.clone(), previous: prev.clone() });
            }
            None => {
                s.pending.remove(&m.id);
            }
        }
    })?;

    // keep the new version and the one before it
    let keep = [Some(m.version.clone()), old, Some("current".into()), Some("previous".into())];
    for e in fs::read_dir(&app)?.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if !keep.iter().flatten().any(|k| *k == n) {
            let _ = fs::remove_dir_all(e.path());
        }
    }
    log::info!("installed {} {}", m.id, m.version);
    app_info(p, &m.id).context("the new version is not readable")
}

pub fn remove(p: &Paths, id: &str, running: &[String]) -> Result<()> {
    if !kpkg::valid_id(id) {
        bail!("invalid app id");
    }
    if running.iter().any(|r| r == id) {
        bail!("{id} is running");
    }
    let app = p.apps().join(id);
    if !app.exists() {
        bail!("{id} is not installed");
    }
    let gone = p.apps().join(format!(".removing-{id}"));
    let _ = fs::remove_dir_all(&gone);
    fs::rename(&app, &gone)?;
    sync_dir(&p.apps())?;
    fs::remove_dir_all(&gone)?;
    // the app's own data stays, so a reinstall finds its settings again
    crate::state::update(&p.store, |s| {
        s.pending.remove(id);
        s.blocked.remove(id);
        s.notices.retain(|n| n.id != id);
    })?;
    log::info!("removed {id}");
    Ok(())
}

pub fn current_version(p: &Paths, id: &str) -> Option<String> {
    link_target(&p.apps().join(id).join("current"))
}

/// Switches `current` back to the previous version. With `discard` the version
/// being left is deleted (a failed update); otherwise it becomes the previous one,
/// so the admin can go forward again. Returns (from, to).
pub fn rollback(p: &Paths, id: &str, running: &[String], discard: bool) -> Result<(String, String)> {
    if !kpkg::valid_id(id) {
        bail!("invalid app id");
    }
    if running.iter().any(|r| r == id) {
        bail!("{id} is running");
    }
    let app = p.apps().join(id);
    let cur = link_target(&app.join("current")).with_context(|| format!("{id} is not installed"))?;
    let prev = link_target(&app.join("previous")).with_context(|| format!("{id} has no previous version"))?;
    if !app.join(&prev).join("manifest.toml").is_file() {
        bail!("the previous version {prev} is gone");
    }
    swap_link(&app.join("current"), &prev)?;
    if discard {
        let _ = fs::remove_file(app.join("previous"));
        let _ = fs::remove_dir_all(app.join(&cur));
    } else {
        swap_link(&app.join("previous"), &cur)?;
    }
    sync_dir(&app)?;
    crate::state::update(&p.store, |s| {
        s.pending.remove(id);
    })?;
    log::info!("rolled {id} back from {cur} to {prev}");
    Ok((cur, prev))
}

pub fn app_info(p: &Paths, id: &str) -> Option<AppInfo> {
    let dir = p.apps().join(id);
    let m = Manifest::parse(&fs::read_to_string(dir.join("current/manifest.toml")).ok()?).ok()?;
    if m.id != id {
        return None;
    }
    let icon = m.icon.as_ref().map(|i| dir.join("current").join(i)).filter(|i| i.is_file()).and_then(|i| i.to_str().map(str::to_string));
    Some(AppInfo { id: m.id, name: m.name, version: m.version, icon, previous: link_target(&dir.join("previous")) })
}

pub fn installed(p: &Paths) -> Vec<AppInfo> {
    let mut v: Vec<AppInfo> = fs::read_dir(p.apps())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| app_info(p, &e.file_name().to_string_lossy()))
        .collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

pub fn local(p: &Paths) -> Vec<LocalPkg> {
    let mut v = Vec::new();
    for dir in &p.install_dirs {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "kpkg") {
                continue;
            }
            let Ok(m) = kpkg::read_manifest(&path) else { continue };
            if !p.board.as_deref().is_none_or(|b| m.runs_on(b)) {
                continue;
            }
            let installed = link_target(&p.apps().join(&m.id).join("current"));
            v.push(LocalPkg { file: path.to_string_lossy().into_owned(), id: m.id, name: m.name, version: m.version, installed });
        }
    }
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.version.cmp(&b.version)));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Env {
        root: PathBuf,
        p: Paths,
    }

    impl Env {
        fn new(name: &str) -> Env {
            let root = std::env::temp_dir().join(format!("kioskd-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(root.join("media")).unwrap();
            let p = Paths { store: root.join("store"), install_dirs: vec![root.join("media")], board: Some("rpi3".into()), allow_unsigned: true };
            Env { root, p }
        }

        /// Builds media/<id>-<version>.kpkg whose program is a script that prints `tag`.
        fn package(&self, id: &str, version: &str, extra: &str) -> String {
            let d = self.root.join(format!("src-{id}-{version}"));
            fs::create_dir_all(d.join("bin")).unwrap();
            fs::write(d.join("bin/run"), "#!/bin/sh\n").unwrap();
            fs::set_permissions(d.join("bin/run"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(d.join("manifest.toml"), format!("id='{id}'\nversion='{version}'\nname='App {id}'\nentry='bin/run'\n{extra}")).unwrap();
            let out = self.root.join("media").join(format!("{id}-{version}.kpkg"));
            kpkg::pack(&d, &out).unwrap();
            out.to_string_lossy().into_owned()
        }

        fn current(&self, id: &str) -> Option<String> {
            link_target(&self.p.apps().join(id).join("current"))
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn install_update_keeps_the_previous_version() {
        let e = Env::new("update");
        let v1 = e.package("t.app", "1.0.0", "");
        let v2 = e.package("t.app", "1.1.0", "");
        let v3 = e.package("t.app", "1.2.0", "");
        let a = install(&e.p, &v1, &[]).unwrap();
        assert_eq!((a.version.as_str(), a.previous), ("1.0.0", None));
        assert!(install(&e.p, &v1, &[]).unwrap_err().to_string().contains("already installed"));

        let a = install(&e.p, &v2, &[]).unwrap();
        assert_eq!((a.version.as_str(), a.previous.as_deref()), ("1.1.0", Some("1.0.0")));
        assert!(e.p.apps().join("t.app/1.0.0/bin/run").exists(), "old version kept for rollback");

        install(&e.p, &v3, &[]).unwrap();
        assert_eq!(e.current("t.app").as_deref(), Some("1.2.0"));
        assert!(!e.p.apps().join("t.app/1.0.0").exists(), "only current and previous are kept");
        assert!(e.p.apps().join("t.app/1.1.0").exists());
    }

    #[test]
    fn power_cut_mid_install_leaves_the_old_version() {
        let e = Env::new("powercut");
        install(&e.p, &e.package("t.app", "1.0.0", ""), &[]).unwrap();
        // what a cut during the unpack of 1.1.0 leaves: a half-written .tmp and a stray link
        let app = e.p.apps().join("t.app");
        fs::create_dir_all(app.join("1.1.0.tmp/bin")).unwrap();
        fs::write(app.join("1.1.0.tmp/bin/run"), "half").unwrap();
        std::os::unix::fs::symlink("1.1.0", app.join("current.new")).unwrap();
        fs::create_dir_all(e.p.apps().join(".removing-gone")).unwrap();

        assert_eq!(e.current("t.app").as_deref(), Some("1.0.0"));
        assert_eq!(installed(&e.p)[0].version, "1.0.0", "still launchable");
        recover(&e.p);
        assert!(!app.join("1.1.0.tmp").exists() && !app.join("current.new").exists());
        assert!(!e.p.apps().join(".removing-gone").exists());
        // and the update goes through afterwards
        install(&e.p, &e.package("t.app", "1.1.0", ""), &[]).unwrap();
        assert_eq!(e.current("t.app").as_deref(), Some("1.1.0"));
    }

    #[test]
    fn a_failed_unpack_changes_nothing() {
        let e = Env::new("fail");
        install(&e.p, &e.package("t.app", "1.0.0", ""), &[]).unwrap();
        let bad = e.root.join("media/t.app-9.9.9.kpkg");
        fs::write(&bad, "not a package").unwrap();
        assert!(install(&e.p, bad.to_str().unwrap(), &[]).is_err());
        assert_eq!(e.current("t.app").as_deref(), Some("1.0.0"));
    }

    #[test]
    fn refusals() {
        let mut e = Env::new("refuse");
        let pkg = e.package("t.app", "1.0.0", "boards=['rpi5']");
        assert!(install(&e.p, &pkg, &[]).unwrap_err().to_string().contains("does not support this board"));
        let pkg = e.package("t.app", "1.0.1", "");
        assert!(install(&e.p, &pkg, &["t.app".into()]).unwrap_err().to_string().contains("is running"));

        let outside = e.root.join("outside.kpkg");
        fs::copy(&pkg, &outside).unwrap();
        assert!(install(&e.p, outside.to_str().unwrap(), &[]).unwrap_err().to_string().contains("install directory"));
        assert!(install(&e.p, "/etc/passwd", &[]).is_err());

        e.p.allow_unsigned = false;
        assert!(install(&e.p, &pkg, &[]).unwrap_err().to_string().contains("STORE_ALLOW_UNSIGNED"));
        assert!(installed(&e.p).is_empty());
    }

    #[test]
    fn remove_and_listing() {
        let e = Env::new("list");
        let a = e.package("b.app", "1.0.0", "");
        e.package("a.app", "2.0.0", "");
        install(&e.p, &a, &[]).unwrap();
        let l = local(&e.p);
        assert_eq!(l.iter().map(|p| (p.id.as_str(), p.installed.as_deref())).collect::<Vec<_>>(), [("a.app", None), ("b.app", Some("1.0.0"))]);

        assert!(remove(&e.p, "b.app", &["b.app".into()]).is_err());
        fs::create_dir_all(e.p.store.join("data/b.app")).unwrap();
        remove(&e.p, "b.app", &[]).unwrap();
        assert!(installed(&e.p).is_empty());
        assert!(e.p.store.join("data/b.app").exists(), "app data survives removal");
        assert!(remove(&e.p, "b.app", &[]).is_err());
        assert!(remove(&e.p, "../x", &[]).is_err());
    }

    #[test]
    fn an_update_stays_pending_until_it_passes_the_health_gate() {
        use crate::health::{Verdict, conclude};
        let e = Env::new("gate-good");
        install(&e.p, &e.package("t.app", "1.0.0", ""), &[]).unwrap();
        assert!(crate::state::read(&e.p.store).pending.is_empty(), "a first install has nothing to go back to");
        install(&e.p, &e.package("t.app", "2.0.0", ""), &[]).unwrap();
        let pending = crate::state::read(&e.p.store).pending;
        assert_eq!((pending["t.app"].version.as_str(), pending["t.app"].previous.as_str()), ("2.0.0", "1.0.0"));

        conclude(&e.p, "t.app", "2.0.0", Verdict::Inconclusive).unwrap();
        assert!(crate::state::read(&e.p.store).pending.contains_key("t.app"), "closed early: still pending");
        conclude(&e.p, "t.app", "2.0.0", Verdict::Good).unwrap();
        assert!(crate::state::read(&e.p.store).pending.is_empty());
        assert_eq!(e.current("t.app").as_deref(), Some("2.0.0"));
    }

    #[test]
    fn a_failed_health_gate_rolls_back_blocks_the_version_and_leaves_a_notice() {
        use crate::health::{Verdict, conclude};
        // conclude() stops the app and asks for kiosk-ui: not on the machine running the tests
        unsafe { std::env::set_var("KIOSKD_SYSTEMCTL", "true") };
        let e = Env::new("gate-bad");
        install(&e.p, &e.package("t.app", "1.0.0", ""), &[]).unwrap();
        install(&e.p, &e.package("t.app", "2.0.0", ""), &[]).unwrap();

        conclude(&e.p, "t.app", "2.0.0", Verdict::Failed).unwrap();
        assert_eq!(e.current("t.app").as_deref(), Some("1.0.0"), "back on the version that worked");
        assert!(!e.p.apps().join("t.app/2.0.0").exists() && !e.p.apps().join("t.app/previous").exists());
        assert_eq!(installed(&e.p)[0].version, "1.0.0", "and it is launchable");
        let s = crate::state::read(&e.p.store);
        assert!(s.pending.is_empty());
        assert_eq!(s.blocked["t.app"], ["2.0.0"]);
        assert_eq!((s.notices[0].from.as_str(), s.notices[0].to.as_str()), ("2.0.0", "1.0.0"));
        assert!(s.notices[0].reason.contains("health check"));

        // installing it again on purpose lifts the block
        install(&e.p, &e.package("t.app", "2.0.0", ""), &[]).unwrap();
        assert!(crate::state::read(&e.p.store).blocked["t.app"].is_empty());
    }

    #[test]
    fn manual_rollback_swaps_and_can_go_forward_again() {
        let e = Env::new("rollback");
        install(&e.p, &e.package("t.app", "1.0.0", ""), &[]).unwrap();
        assert!(rollback(&e.p, "t.app", &[], false).unwrap_err().to_string().contains("no previous"));
        install(&e.p, &e.package("t.app", "2.0.0", ""), &[]).unwrap();
        assert_eq!(rollback(&e.p, "t.app", &["t.app".into()], false).unwrap_err().to_string(), "t.app is running");
        assert_eq!(rollback(&e.p, "t.app", &[], false).unwrap(), ("2.0.0".into(), "1.0.0".into()));
        assert_eq!(e.current("t.app").as_deref(), Some("1.0.0"));
        assert_eq!(rollback(&e.p, "t.app", &[], false).unwrap(), ("1.0.0".into(), "2.0.0".into()), "and forward again");
        assert!(crate::state::read(&e.p.store).pending.is_empty());
    }
}
