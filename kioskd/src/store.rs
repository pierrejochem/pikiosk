//! The signed store: a TUF repository served over HTTPS (or a file:// mirror).
//!
//! The trust anchor is `root.json`, baked into the image. The client checks
//! the signatures, the metadata expiry, rollback (the datastore remembers the
//! newest versions it has seen) and the length and SHA-256 of every file it
//! downloads, all through the `tough` crate. The publisher signs a
//! `catalog.json` target listing the apps, and each package is a target too.

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use kpkg::{Catalog, CatalogEntry};
use serde_json::json;
use tough::{IntoVec, Repository, RepositoryLoader, TargetName};
use url::Url;

use crate::install::{self, AppInfo, Paths};
use crate::{progress, state};

/// One TUF operation at a time: the datastore is not shared between them.
static BUSY: Mutex<()> = Mutex::new(());

const MAX_CATALOG: u64 = 4 << 20;

#[derive(Clone, Debug)]
pub struct Config {
    /// base URL of the store; None = the store is off
    pub url: Option<String>,
    pub channel: String,
    pub root: PathBuf,
    /// /data/store/cache: the TUF datastore, the last catalog and downloads
    pub cache: PathBuf,
    /// exists once the clock has been set by NTP
    pub clock_flag: PathBuf,
}

impl Config {
    pub fn from_env(store: &Path) -> Config {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Config {
            url: var("KIOSKD_STORE_URL"),
            channel: var("KIOSKD_STORE_CHANNEL").unwrap_or_else(|| "stable".into()),
            root: var("KIOSKD_ROOT_JSON").map(PathBuf::from).unwrap_or_else(|| "/usr/share/kioskd/root.json".into()),
            cache: store.join("cache"),
            clock_flag: var("KIOSKD_CLOCK_FLAG").map(PathBuf::from).unwrap_or_else(|| "/run/systemd/timesync/synchronized".into()),
        }
    }

    pub fn enabled(&self) -> bool {
        self.url.is_some()
    }

    fn urls(&self) -> Result<(Url, Url)> {
        let base = self.url.as_deref().context("the store is off (STORE_URL is empty)")?;
        let mut base = Url::parse(base).context("STORE_URL is not a URL")?;
        if !matches!(base.scheme(), "http" | "https" | "file") {
            bail!("STORE_URL must start with https://");
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        if self.channel.is_empty() || !self.channel.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') {
            bail!("invalid STORE_CHANNEL");
        }
        let root = base.join(&format!("{}/", self.channel))?;
        Ok((root.join("metadata/")?, root.join("targets/")?))
    }

    /// The board has no RTC: until NTP has set the clock, "now" is wrong and
    /// metadata would look expired or still valid by mistake.
    fn require_clock(&self) -> Result<()> {
        if !self.clock_flag.exists() {
            bail!("the clock is not set yet (waiting for NTP)");
        }
        Ok(())
    }

    async fn load(&self) -> Result<Repository> {
        let (metadata, targets) = self.urls()?;
        let root = tokio::fs::read(&self.root).await.with_context(|| format!("reading the trust anchor {}", self.root.display()))?;
        let datastore = self.cache.join("tuf").join(&self.channel);
        tokio::fs::create_dir_all(&datastore).await?;
        RepositoryLoader::new(&root, metadata, targets)
            .datastore(datastore)
            .load()
            .await
            .context("the store's metadata did not verify")
    }

    fn catalog_path(&self) -> PathBuf {
        self.cache.join("catalog.json")
    }

    /// The catalog from the last successful sync, and when that was (unix seconds).
    pub fn cached(&self) -> Option<(Catalog, u64)> {
        let cat = serde_json::from_slice(&std::fs::read(self.catalog_path()).ok()?).ok()?;
        let at = std::fs::read_to_string(self.cache.join("synced_at")).ok()?.trim().parse().ok()?;
        Some((cat, at))
    }

    fn save_catalog(&self, cat: &Catalog) -> Result<()> {
        std::fs::create_dir_all(&self.cache)?;
        let tmp = self.cache.join("catalog.json.new");
        std::fs::write(&tmp, serde_json::to_vec(cat)?)?;
        std::fs::rename(&tmp, self.catalog_path())?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        std::fs::write(self.cache.join("synced_at"), now.to_string())?;
        Ok(())
    }
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime").block_on(f)
}

fn target_length(repo: &Repository, name: &TargetName) -> Result<u64> {
    repo.all_targets().find(|(n, _)| n.raw() == name.raw()).map(|(_, t)| t.length).with_context(|| format!("{} is not in the repository", name.raw()))
}

async fn read_catalog(repo: &Repository) -> Result<Catalog> {
    let name = TargetName::new("catalog.json")?;
    if target_length(repo, &name)? > MAX_CATALOG {
        bail!("catalog.json is too large");
    }
    let bytes = repo.read_target(&name).await?.context("catalog.json is not in the repository")?.into_vec().await?;
    let cat: Catalog = serde_json::from_slice(&bytes).context("catalog.json")?;
    if cat.v != 1 {
        bail!("catalog.json has version {}, this image understands 1", cat.v);
    }
    Ok(cat)
}

/// The catalog without the versions that failed their health check on this kiosk.
fn without_blocked(cat: &Catalog, store: &Path) -> Catalog {
    let blocked = state::read(store).blocked;
    Catalog { v: cat.v, apps: cat.apps.iter().filter(|e| !blocked.get(&e.id).is_some_and(|b| b.contains(&e.version))).cloned().collect() }
}

/// A package file name from the catalog: plain, no directories.
fn valid_file_name(f: &str) -> bool {
    f.ends_with(".kpkg") && !f.starts_with('.') && f.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Downloads one package into the cache. The bytes are checked against the
/// signed length and hash while they stream in; on any error the file is deleted.
async fn download(cfg: &Config, repo: &Repository, entry: &CatalogEntry) -> Result<PathBuf> {
    if !valid_file_name(&entry.file) {
        bail!("bad package name {:?} in the catalog", entry.file);
    }
    let name = TargetName::new(&entry.file)?;
    let len = target_length(repo, &name)?;
    if len > kpkg::MAX_UNPACKED {
        bail!("{} is larger than {} MiB", entry.file, kpkg::MAX_UNPACKED >> 20);
    }
    let dir = cfg.cache.join("pkgs");
    tokio::fs::create_dir_all(&dir).await?;
    let (part, done) = (dir.join(format!("{}.part", entry.file)), dir.join(&entry.file));
    let mut stream = Box::pin(repo.read_target(&name).await?.context("the package is not in the repository")?);
    let result: Result<()> = async {
        let mut f = std::fs::File::create(&part)?;
        let mut done = 0u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("the download did not verify")?;
            f.write_all(&chunk)?;
            done += chunk.len() as u64;
            progress::update("download", done, len);
        }
        f.sync_all()?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &done)?;
    Ok(done)
}

/// Fetches and verifies the repository metadata and the catalog.
pub fn refresh(cfg: &Config) -> Result<Catalog> {
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    cfg.require_clock()?;
    block_on(async {
        let cat = read_catalog(&cfg.load().await?).await?;
        cfg.save_catalog(&cat)?;
        log::info!("store: synced, {} package(s) in the catalog", cat.apps.len());
        Ok(cat)
    })
}

/// Downloads the newest version of `id` for this board, verifies it and installs it.
pub fn install_app(cfg: &Config, paths: &Paths, id: &str, running: &[String]) -> Result<AppInfo> {
    let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    cfg.require_clock()?;
    let _job = progress::Job::start(id);
    let (entry, file) = block_on(async {
        let repo = cfg.load().await?;
        let cat = read_catalog(&repo).await?;
        let visible = without_blocked(&cat, &paths.store);
        let entry = visible.newest_for(paths.board.as_deref()).into_iter().find(|e| e.id == id).cloned().with_context(|| format!("{id} is not in the catalog for this board"))?;
        let file = download(cfg, &repo, &entry).await?;
        let _ = cfg.save_catalog(&cat);
        anyhow::Ok((entry, file))
    })?;
    let result = install::install_package(paths, &file, running, Some(&entry));
    let _ = std::fs::remove_file(&file);
    result
}

/// What kiosk-ui shows: the last synced catalog, with what is installed.
pub fn view(cfg: &Config, paths: &Paths) -> serde_json::Value {
    let installed = install::installed(paths);
    let (cat, synced_at) = cfg.cached().map(|(c, a)| (Some(c), Some(a))).unwrap_or((None, None));
    let cat = cat.map(|c| without_blocked(&c, &paths.store));
    let apps: Vec<_> = cat
        .iter()
        .flat_map(|c| c.newest_for(paths.board.as_deref()))
        .map(|e| {
            let have = installed.iter().find(|i| i.id == e.id).map(|i| i.version.clone());
            let update = have.as_ref().is_some_and(|h| kpkg::parse_version(h) < kpkg::parse_version(&e.version));
            json!({ "id": e.id, "name": e.name, "version": e.version, "category": e.category, "size": e.size, "permissions": e.permissions, "installed": have, "update": update, "boards": e.boards })
        })
        .collect();
    json!({ "enabled": cfg.enabled(), "channel": cfg.channel, "synced_at": synced_at, "apps": apps })
}

/// Keeps the catalog fresh: once the clock is set, and then every six hours.
pub fn sync_loop(cfg: Config) {
    if !cfg.enabled() {
        return;
    }
    loop {
        let wait = if !cfg.clock_flag.exists() {
            Duration::from_secs(10)
        } else {
            match refresh(&cfg) {
                Ok(_) => Duration::from_secs(6 * 3600),
                Err(e) => {
                    log::warn!("store: sync failed: {e:#}");
                    Duration::from_secs(600)
                }
            }
        };
        std::thread::sleep(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::num::NonZeroU64;
    use std::os::unix::fs::PermissionsExt;

    use aws_lc_rs::rand::SystemRandom;
    use jiff::{SignedDuration, Timestamp};
    use tough::editor::RepositoryEditor;
    use tough::editor::signed::{PathExists, SignedRole};
    use tough::key_source::{KeySource, LocalKeySource};
    use tough::schema::{KeyHolder, RoleKeys, RoleType, Root, Signed, Target};

    /// A throwaway publisher: one Ed25519 key for every role, and a repository
    /// laid out like the real one (`<base>/stable/{metadata,targets}`).
    struct Publisher {
        dir: PathBuf,
        key: PathBuf,
        root: PathBuf,
    }

    fn hours(h: i64) -> SignedDuration {
        SignedDuration::from_hours(h)
    }

    impl Publisher {
        async fn new(dir: &Path) -> Publisher {
            std::fs::create_dir_all(dir).unwrap();
            // tough reads an Ed25519 key as a raw PKCS#8 DER file (not PEM)
            let key = dir.join("key.der");
            let pkcs8 = aws_lc_rs::signature::Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
            std::fs::write(&key, pkcs8.as_ref()).unwrap();

            let ks: Box<dyn KeySource> = Box::new(LocalKeySource { path: key.clone() });
            let tuf_key = ks.as_sign().await.unwrap().tuf_key();
            let key_id = tuf_key.key_id().unwrap();
            let role = || RoleKeys { keyids: vec![key_id.clone()], threshold: NonZeroU64::new(1).unwrap(), _extra: HashMap::new() };
            let root = Root {
                spec_version: "1.0.0".into(),
                consistent_snapshot: true,
                version: NonZeroU64::new(1).unwrap(),
                expires: Timestamp::now() + hours(24 * 365),
                keys: HashMap::from([(key_id.clone(), tuf_key)]),
                roles: HashMap::from([(RoleType::Root, role()), (RoleType::Snapshot, role()), (RoleType::Targets, role()), (RoleType::Timestamp, role())]),
                _extra: HashMap::new(),
            };
            let signed = SignedRole::new(root.clone(), &KeyHolder::Root(root), &[ks], &SystemRandom::new()).await.unwrap();
            let root_path = dir.join("root.json");
            std::fs::write(&root_path, signed.buffer()).unwrap();
            let _: Signed<Root> = serde_json::from_slice(signed.buffer()).unwrap();
            Publisher { dir: dir.to_path_buf(), key, root: root_path }
        }

        /// Publishes `packages` (a directory with .kpkg files) as metadata version `version`.
        async fn publish(&self, version: u64, packages: &Path, expires: Timestamp) {
            let cat = Catalog::from_dir(packages).unwrap();
            std::fs::write(packages.join("catalog.json"), serde_json::to_vec(&cat).unwrap()).unwrap();
            let v = NonZeroU64::new(version).unwrap();
            let mut ed = RepositoryEditor::new(&self.root).await.unwrap();
            ed.targets_version(v).unwrap().targets_expires(expires).unwrap().snapshot_version(v).snapshot_expires(expires).timestamp_version(v).timestamp_expires(expires);
            for e in std::fs::read_dir(packages).unwrap().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                ed.add_target(TargetName::new(&name).unwrap(), Target::from_path(e.path()).await.unwrap()).unwrap();
            }
            let signed = ed.sign(&[Box::new(LocalKeySource { path: self.key.clone() })]).await.unwrap();
            let repo = self.dir.join("repo/stable");
            let _ = std::fs::remove_dir_all(&repo);
            signed.copy_targets(packages, repo.join("targets"), PathExists::Replace).await.unwrap();
            signed.write(repo.join("metadata")).await.unwrap();
        }

        fn config(&self, cache: &Path, flag: &Path) -> Config {
            Config {
                url: Some(Url::from_directory_path(self.dir.join("repo")).unwrap().to_string()),
                channel: "stable".into(),
                root: self.root.clone(),
                cache: cache.to_path_buf(),
                clock_flag: flag.to_path_buf(),
            }
        }
    }

    fn package(dir: &Path, id: &str, version: &str, extra: &str) {
        let src = dir.join(format!("src-{id}-{version}"));
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(src.join("bin/run"), format!("#!/bin/sh\n# {version}\n")).unwrap();
        std::fs::set_permissions(src.join("bin/run"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(src.join("manifest.toml"), format!("id='{id}'\nversion='{version}'\nname='App {id}'\nentry='bin/run'\n{extra}")).unwrap();
        std::fs::create_dir_all(dir.join("pkgs")).unwrap();
        kpkg::pack(&src, &dir.join("pkgs").join(format!("{id}-{version}.kpkg"))).unwrap();
    }

    struct Env {
        root: PathBuf,
        publisher: Publisher,
        cfg: Config,
        paths: Paths,
    }

    impl Env {
        async fn new(name: &str) -> Env {
            let root = std::env::temp_dir().join(format!("kioskd-store-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            let flag = root.join("synchronized");
            std::fs::write(&flag, "").unwrap();
            let publisher = Publisher::new(&root.join("publisher")).await;
            let cfg = publisher.config(&root.join("store/cache"), &flag);
            let paths = Paths { store: root.join("store"), install_dirs: vec![], board: Some("rpi3".into()), allow_unsigned: false };
            Env { root, publisher, cfg, paths }
        }

        fn pkgs(&self) -> PathBuf {
            self.root.join("publisher/pkgs")
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn run<F: Future>(f: F) -> F::Output {
        block_on(f)
    }

    #[test]
    fn a_signed_app_installs_and_updates() {
        let e = run(Env::new("ok"));
        let pk = e.root.join("publisher");
        package(&pk, "t.app", "1.0.0", "category='Tools'\n");
        run(e.publisher.publish(1, &e.pkgs(), Timestamp::now() + hours(24)));

        let cat = refresh(&e.cfg).unwrap();
        assert_eq!(cat.apps.len(), 1);
        let v = view(&e.cfg, &e.paths);
        assert_eq!(v["apps"][0]["category"], "Tools");
        assert_eq!(v["apps"][0]["installed"], serde_json::Value::Null);

        // signed installs need neither STORE_ALLOW_UNSIGNED nor an install directory
        let a = install_app(&e.cfg, &e.paths, "t.app", &[]).unwrap();
        assert_eq!(a.version, "1.0.0");
        assert!(!e.cfg.cache.join("pkgs/t.app-1.0.0.kpkg").exists(), "the download is removed after the install");

        package(&pk, "t.app", "1.1.0", "");
        run(e.publisher.publish(2, &e.pkgs(), Timestamp::now() + hours(24)));
        refresh(&e.cfg).unwrap();
        assert_eq!(view(&e.cfg, &e.paths)["apps"][0]["update"], true);
        let a = install_app(&e.cfg, &e.paths, "t.app", &[]).unwrap();
        assert_eq!((a.version.as_str(), a.previous.as_deref()), ("1.1.0", Some("1.0.0")));
        assert!(install_app(&e.cfg, &e.paths, "no.such.app", &[]).is_err());
    }

    #[test]
    fn a_tampered_package_is_rejected() {
        let e = run(Env::new("tamper"));
        let pk = e.root.join("publisher");
        package(&pk, "t.app", "1.0.0", "");
        run(e.publisher.publish(1, &e.pkgs(), Timestamp::now() + hours(24)));

        // an attacker on the mirror changes one byte of the package, same length
        let targets = e.root.join("publisher/repo/stable/targets");
        let file = std::fs::read_dir(&targets).unwrap().flatten().map(|f| f.path()).find(|p| p.to_string_lossy().ends_with("t.app-1.0.0.kpkg")).unwrap();
        let mut bytes = std::fs::read(&file).unwrap();
        let n = bytes.len() / 2;
        bytes[n] ^= 0xff;
        std::fs::write(&file, bytes).unwrap();

        let err = install_app(&e.cfg, &e.paths, "t.app", &[]).unwrap_err();
        assert!(format!("{err:#}").contains("did not verify") || format!("{err:#}").to_lowercase().contains("hash"), "{err:#}");
        assert!(install::installed(&e.paths).is_empty(), "nothing is installed");
        assert!(std::fs::read_dir(e.cfg.cache.join("pkgs")).map(|d| d.count()).unwrap_or(0) == 0, "no partial download is left");
    }

    #[test]
    fn an_old_snapshot_is_rejected() {
        let e = run(Env::new("rollback"));
        let pk = e.root.join("publisher");
        package(&pk, "t.app", "1.0.0", "");
        run(e.publisher.publish(1, &e.pkgs(), Timestamp::now() + hours(24)));
        let old = e.root.join("old-repo");
        copy_dir(&e.root.join("publisher/repo"), &old);

        package(&pk, "t.app", "1.1.0", "");
        run(e.publisher.publish(2, &e.pkgs(), Timestamp::now() + hours(24)));
        refresh(&e.cfg).unwrap();

        // a mirror (or a man in the middle) serves the validly signed but older repository
        std::fs::remove_dir_all(e.root.join("publisher/repo")).unwrap();
        copy_dir(&old, &e.root.join("publisher/repo"));
        let err = refresh(&e.cfg).unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("older") || format!("{err:#}").to_lowercase().contains("rollback") || format!("{err:#}").to_lowercase().contains("version"), "{err:#}");
        assert!(install_app(&e.cfg, &e.paths, "t.app", &[]).is_err());
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let t = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &t);
            } else {
                std::fs::copy(e.path(), t).unwrap();
            }
        }
    }

    #[test]
    fn expired_metadata_and_a_foreign_key_are_rejected() {
        let e = run(Env::new("expired"));
        let pk = e.root.join("publisher");
        package(&pk, "t.app", "1.0.0", "");
        run(e.publisher.publish(1, &e.pkgs(), Timestamp::now() - hours(1)));
        let err = refresh(&e.cfg).unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("expired"), "{err:#}");

        // a repository signed by somebody else's key does not match our root.json
        let other = run(Publisher::new(&e.root.join("other")));
        let other_pk = e.root.join("other");
        package(&other_pk, "t.app", "1.0.0", "");
        run(other.publish(1, &other_pk.join("pkgs"), Timestamp::now() + hours(24)));
        let mut cfg = other.config(&e.root.join("store2/cache"), &e.cfg.clock_flag);
        cfg.root = e.publisher.root.clone();
        assert!(refresh(&cfg).is_err(), "signed by a key the trust anchor does not list");
    }

    #[test]
    fn nothing_happens_before_the_clock_is_set_or_when_the_store_is_off() {
        let mut e = run(Env::new("clock"));
        let pk = e.root.join("publisher");
        package(&pk, "t.app", "1.0.0", "");
        run(e.publisher.publish(1, &e.pkgs(), Timestamp::now() + hours(24)));
        std::fs::remove_file(&e.cfg.clock_flag).unwrap();
        assert!(refresh(&e.cfg).unwrap_err().to_string().contains("clock"));
        assert!(install_app(&e.cfg, &e.paths, "t.app", &[]).unwrap_err().to_string().contains("clock"));

        e.cfg.url = None;
        std::fs::write(&e.cfg.clock_flag, "").unwrap();
        assert!(refresh(&e.cfg).unwrap_err().to_string().contains("store is off"));
        assert_eq!(view(&e.cfg, &e.paths)["enabled"], false);
    }

    #[test]
    fn a_package_that_disagrees_with_the_catalog_is_refused() {
        let e = run(Env::new("mismatch"));
        let pk = e.root.join("publisher");
        package(&pk, "t.app", "1.0.0", "permissions=['network']\n");
        let cat = Catalog::from_dir(&e.pkgs()).unwrap();
        let mut entry = cat.apps[0].clone();
        let file = e.pkgs().join(&entry.file);

        entry.permissions.clear();
        assert!(install::install_package(&e.paths, &file, &[], Some(&entry)).unwrap_err().to_string().contains("permission"));
        let mut other = cat.apps[0].clone();
        other.version = "9.9.9".into();
        assert!(install::install_package(&e.paths, &file, &[], Some(&other)).is_err());
        assert!(install::installed(&e.paths).is_empty());
        assert!(install::install_package(&e.paths, &file, &[], Some(&cat.apps[0])).is_ok());
    }
}
