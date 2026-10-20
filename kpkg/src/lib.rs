//! The .kpkg format: a `tar.zst` archive with `manifest.toml` first, then the
//! program and its assets. Packing is deterministic (sorted, zeroed owner
//! and times), so the same directory always gives the same bytes.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const BOARDS: [&str; 3] = ["rpi3", "rpi4", "rpi5"];
pub const PERMISSIONS: [&str; 4] = ["network", "audio", "gpio", "serial"];
/// Unpacked size limit for one package.
pub const MAX_UNPACKED: u64 = 256 << 20;
const MAX_MANIFEST: u64 = 64 << 10;
const ICON_SIZE: u32 = 128;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub name: String,
    pub entry: String,
    #[serde(default)]
    pub icon: Option<String>,
    /// where the store files the app, e.g. "Weather"; default "Other"
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub min_os: Option<String>,
    /// empty = every board
    #[serde(default)]
    pub boards: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
}

/// An app id is a short reverse-DNS style name. The check keeps it safe to
/// use in a path and in a unit name.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with(['.', '-'])
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_'))
}

/// MAJOR.MINOR.PATCH as numbers, so versions compare right (1.10.0 > 1.9.0).
pub fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.split('.');
    let n = |s: Option<&str>| -> Option<u32> {
        let s = s?;
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'))).then(|| s.parse().ok())?
    };
    let r = (n(it.next())?, n(it.next())?, n(it.next())?);
    it.next().is_none().then_some(r)
}

/// A path inside a package: relative, no `..`, no empty parts.
fn safe_rel(p: &str) -> bool {
    !p.is_empty() && Path::new(p).components().all(|c| matches!(c, Component::Normal(_))) && !p.contains(['\0', '\\'])
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        let m: Manifest = toml::from_str(text).context("manifest.toml")?;
        m.validate()?;
        Ok(m)
    }

    pub fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            bail!("id {:?}: use lower-case letters, digits, '.', '-' and '_' (at most 64)", self.id);
        }
        if parse_version(&self.version).is_none() {
            bail!("version {:?} is not MAJOR.MINOR.PATCH", self.version);
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 40 || self.name.chars().any(|c| c.is_control()) {
            bail!("name must be 1-40 characters");
        }
        if !safe_rel(&self.entry) {
            bail!("entry {:?} must be a relative path inside the package", self.entry);
        }
        if let Some(i) = &self.icon
            && !safe_rel(i)
        {
            bail!("icon {i:?} must be a relative path inside the package");
        }
        if let Some(c) = &self.category
            && (c.trim().is_empty() || c.chars().count() > 24 || c.chars().any(|c| c.is_control()))
        {
            bail!("category must be 1-24 characters");
        }
        if let Some(v) = &self.min_os
            && parse_version(v).is_none()
        {
            bail!("min_os {v:?} is not MAJOR.MINOR.PATCH");
        }
        for b in &self.boards {
            if !BOARDS.contains(&b.as_str()) {
                bail!("unknown board {b:?} (known: {})", BOARDS.join(", "));
            }
        }
        for p in &self.permissions {
            if !PERMISSIONS.contains(&p.as_str()) {
                bail!("unknown permission {p:?} (known: {})", PERMISSIONS.join(", "));
            }
        }
        for e in &self.env {
            if e.starts_with('=') || !e.contains('=') || e.chars().any(|c| c.is_control()) {
                bail!("env entry {e:?} is not KEY=value");
            }
        }
        if self.args.iter().any(|a| a.chars().any(|c| c.is_control())) {
            bail!("args must not contain control characters");
        }
        Ok(())
    }

    pub fn runs_on(&self, board: &str) -> bool {
        self.boards.is_empty() || self.boards.iter().any(|b| b == board)
    }
}

/// One app in the store's catalog, as the publisher lists it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub category: String,
    /// the package's target name in the TUF repository
    pub file: String,
    pub size: u64,
    pub permissions: Vec<String>,
    pub boards: Vec<String>,
}

/// The catalog the publisher signs as the TUF target `catalog.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Catalog {
    pub v: u32,
    pub apps: Vec<CatalogEntry>,
}

impl Catalog {
    /// Lists every `.kpkg` in `dir`, by name then version.
    pub fn from_dir(dir: &Path) -> Result<Catalog> {
        let mut apps = Vec::new();
        for e in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = e?.path();
            if path.extension().is_none_or(|x| x != "kpkg") {
                continue;
            }
            let m = read_manifest(&path).with_context(|| path.display().to_string())?;
            apps.push(CatalogEntry {
                category: m.category.clone().unwrap_or_else(|| "Other".into()),
                file: path.file_name().and_then(|n| n.to_str()).context("file name")?.to_string(),
                size: fs::metadata(&path)?.len(),
                id: m.id,
                name: m.name,
                version: m.version,
                permissions: m.permissions,
                boards: m.boards,
            });
        }
        apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(parse_version(&a.version).cmp(&parse_version(&b.version))));
        Ok(Catalog { v: 1, apps })
    }

    /// The newest version of each app that runs on `board`.
    pub fn newest_for(&self, board: Option<&str>) -> Vec<&CatalogEntry> {
        let mut out: Vec<&CatalogEntry> = Vec::new();
        for e in self.apps.iter().filter(|e| board.is_none_or(|b| e.boards.is_empty() || e.boards.iter().any(|x| x == b))) {
            match out.iter_mut().find(|o| o.id == e.id) {
                Some(o) if parse_version(&e.version) > parse_version(&o.version) => *o = e,
                Some(_) => {}
                None => out.push(e),
            }
        }
        out
    }
}

/// Width and height of a PNG, read from its header.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || bytes[..8] != [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] || &bytes[12..16] != b"IHDR" {
        return None;
    }
    Some((u32::from_be_bytes(bytes[16..20].try_into().ok()?), u32::from_be_bytes(bytes[20..24].try_into().ok()?)))
}

fn walk(dir: &Path, rel: &Path, out: &mut Vec<(PathBuf, PathBuf, bool)>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let r = rel.join(e.file_name());
        let t = e.file_type()?;
        if t.is_dir() {
            out.push((path.clone(), r.clone(), true));
            walk(&path, &r, out)?;
        } else if t.is_file() {
            out.push((path, r, false));
        } else {
            bail!("{}: only files and directories can go in a package", r.display());
        }
    }
    Ok(())
}

/// Lints `dir` and writes it as a package. Returns the manifest.
pub fn pack(dir: &Path, out: &Path) -> Result<Manifest> {
    let text = fs::read_to_string(dir.join("manifest.toml")).context("reading manifest.toml")?;
    let m = Manifest::parse(&text)?;
    let entry = dir.join(&m.entry);
    let meta = fs::metadata(&entry).with_context(|| format!("entry {} is missing", m.entry))?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        bail!("entry {} is not an executable file", m.entry);
    }
    if let Some(icon) = &m.icon {
        let bytes = fs::read(dir.join(icon)).with_context(|| format!("icon {icon} is missing"))?;
        match png_size(&bytes) {
            Some((ICON_SIZE, ICON_SIZE)) => {}
            Some((w, h)) => bail!("icon {icon} is {w}x{h}, it must be {ICON_SIZE}x{ICON_SIZE}"),
            None => bail!("icon {icon} is not a PNG"),
        }
    }

    let mut files = Vec::new();
    walk(dir, Path::new(""), &mut files)?;
    let enc = zstd::Encoder::new(File::create(out)?, 19)?;
    let mut tar = tar::Builder::new(enc);
    tar.mode(tar::HeaderMode::Deterministic);
    let put = |tar: &mut tar::Builder<_>, path: &Path, rel: &Path, is_dir: bool| -> Result<()> {
        let mut h = tar::Header::new_gnu();
        h.set_mtime(0);
        h.set_uid(0);
        h.set_gid(0);
        if is_dir {
            h.set_entry_type(tar::EntryType::Directory);
            h.set_mode(0o755);
            h.set_size(0);
            tar.append_data(&mut h, rel, std::io::empty())?;
        } else {
            let exec = fs::metadata(path)?.permissions().mode() & 0o111 != 0;
            h.set_entry_type(tar::EntryType::Regular);
            h.set_mode(if exec { 0o755 } else { 0o644 });
            h.set_size(fs::metadata(path)?.len());
            tar.append_data(&mut h, rel, File::open(path)?)?;
        }
        Ok(())
    };
    put(&mut tar, &dir.join("manifest.toml"), Path::new("manifest.toml"), false)?;
    for (path, rel, is_dir) in &files {
        if rel == Path::new("manifest.toml") {
            continue;
        }
        put(&mut tar, path, rel, *is_dir)?;
    }
    tar.into_inner()?.finish()?.sync_all()?;
    Ok(m)
}

fn open(package: &Path) -> Result<tar::Archive<impl Read>> {
    let dec = zstd::Decoder::new(File::open(package).with_context(|| format!("opening {}", package.display()))?)?;
    // a small archive must not unpack to an unbounded stream
    Ok(tar::Archive::new(dec.take(MAX_UNPACKED + (8 << 20))))
}

/// Reads and validates the manifest without unpacking the rest.
pub fn read_manifest(package: &Path) -> Result<Manifest> {
    for entry in open(package)?.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() == Path::new("manifest.toml") {
            let mut text = String::new();
            (&mut entry).take(MAX_MANIFEST).read_to_string(&mut text)?;
            return Manifest::parse(&text);
        }
    }
    bail!("no manifest.toml in {}", package.display())
}

/// Unpacks into `dest` (which must not exist), flushing every file to disk.
/// Only regular files and directories are accepted, and none may leave `dest`.
pub fn unpack(package: &Path, dest: &Path) -> Result<Manifest> {
    let manifest = read_manifest(package)?;
    fs::create_dir(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut total = 0u64;
    let mut dirs = vec![dest.to_path_buf()];
    for entry in open(package)?.entries()? {
        let mut entry = entry?;
        let rel = entry.path()?.into_owned();
        if !safe_rel(&rel.to_string_lossy()) {
            bail!("unsafe path {:?} in package", rel);
        }
        let target = dest.join(&rel);
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                fs::create_dir_all(&target)?;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
                dirs.push(target);
            }
            tar::EntryType::Regular => {
                total += entry.size();
                if total > MAX_UNPACKED {
                    bail!("package unpacks to more than {} MiB", MAX_UNPACKED >> 20);
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                    dirs.push(parent.to_path_buf());
                }
                let exec = entry.header().mode()? & 0o111 != 0;
                let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(if exec { 0o755 } else { 0o644 }).open(&target)?;
                std::io::copy(&mut entry, &mut f)?;
                f.flush()?;
                f.sync_all()?;
            }
            other => bail!("{:?}: {other:?} entries are not allowed in a package", rel),
        }
    }
    dirs.sort();
    dirs.dedup();
    for d in dirs {
        File::open(&d)?.sync_all()?;
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kpkg-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        b.extend(b"IHDR");
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b
    }

    fn app_dir(root: &Path, extra: &str) -> PathBuf {
        let d = root.join("app");
        fs::create_dir_all(d.join("bin")).unwrap();
        fs::create_dir_all(d.join("assets")).unwrap();
        fs::write(d.join("bin/weather"), "#!/bin/sh\n").unwrap();
        fs::set_permissions(d.join("bin/weather"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(d.join("assets/icon.png"), png(128, 128)).unwrap();
        fs::write(
            d.join("manifest.toml"),
            format!("id='de.jochem.weather'\nversion='1.5.0'\nname='Weather'\nentry='bin/weather'\nicon='assets/icon.png'\nboards=['rpi3','rpi5']\npermissions=['network']\n{extra}"),
        )
        .unwrap();
        d
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(parse_version("1.10.0") > parse_version("1.9.0"));
        for bad in ["1.0", "1.0.0.0", "01.0.0", "1.x.0", "", "1.0.-1", "v1.0.0"] {
            assert_eq!(parse_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn pack_and_unpack_round_trip() {
        let root = tmp("round");
        let d = app_dir(&root, "");
        let out = root.join("w.kpkg");
        let m = pack(&d, &out).unwrap();
        assert_eq!(m.id, "de.jochem.weather");
        assert_eq!(read_manifest(&out).unwrap().version, "1.5.0");

        let dest = root.join("unpacked");
        unpack(&out, &dest).unwrap();
        let exe = fs::metadata(dest.join("bin/weather")).unwrap().permissions().mode();
        assert_eq!(exe & 0o777, 0o755);
        assert_eq!(fs::metadata(dest.join("assets/icon.png")).unwrap().permissions().mode() & 0o777, 0o644);
        assert!(unpack(&out, &dest).is_err(), "never unpacks over an existing directory");

        let again = root.join("w2.kpkg");
        pack(&d, &again).unwrap();
        assert_eq!(fs::read(&out).unwrap(), fs::read(&again).unwrap(), "packing is deterministic");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_lists_the_newest_version_for_a_board() {
        let root = tmp("catalog");
        let d = app_dir(&root, "");
        fs::create_dir_all(root.join("repo")).unwrap();
        pack(&d, &root.join("repo/weather-1.5.0.kpkg")).unwrap();
        let t = fs::read_to_string(d.join("manifest.toml")).unwrap();
        fs::write(d.join("manifest.toml"), t.replace("1.5.0", "1.10.0") + "category='Weather'\n").unwrap();
        pack(&d, &root.join("repo/weather-1.10.0.kpkg")).unwrap();
        fs::write(root.join("repo/notes.txt"), "ignored").unwrap();

        let c = Catalog::from_dir(&root.join("repo")).unwrap();
        assert_eq!(c.apps.len(), 2);
        assert_eq!(c.apps[0].category, "Other");
        assert_eq!(c.apps[1].category, "Weather");
        let newest = c.newest_for(Some("rpi3"));
        assert_eq!(newest.len(), 1);
        assert_eq!(newest[0].version, "1.10.0", "1.10.0 is newer than 1.5.0");
        assert!(c.newest_for(Some("rpi4")).is_empty(), "the app is for rpi3 and rpi5 only");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lint_catches_a_bad_package() {
        let root = tmp("lint");
        let d = app_dir(&root, "");
        let out = root.join("x.kpkg");

        fs::set_permissions(d.join("bin/weather"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(pack(&d, &out).unwrap_err().to_string().contains("not an executable"));
        fs::set_permissions(d.join("bin/weather"), fs::Permissions::from_mode(0o755)).unwrap();

        fs::write(d.join("assets/icon.png"), png(64, 64)).unwrap();
        assert!(pack(&d, &out).unwrap_err().to_string().contains("64x64"));
        fs::write(d.join("assets/icon.png"), "nope").unwrap();
        assert!(pack(&d, &out).unwrap_err().to_string().contains("not a PNG"));
        fs::write(d.join("assets/icon.png"), png(128, 128)).unwrap();

        let bad = |from: &str, to: &str, want: &str| {
            let t = fs::read_to_string(d.join("manifest.toml")).unwrap();
            fs::write(d.join("manifest.toml"), t.replace(from, to)).unwrap();
            let e = pack(&d, &out).unwrap_err().to_string();
            assert!(e.contains(want), "{e}");
            fs::write(d.join("manifest.toml"), t).unwrap();
        };
        bad("entry='bin/weather'", "entry='../weather'", "relative path");
        bad("entry='bin/weather'", "entry='/bin/sh'", "relative path");
        bad("'rpi3'", "'rpi9'", "unknown board");
        bad("'network'", "'root'", "unknown permission");
        bad("version='1.5.0'", "version='1.5'", "MAJOR.MINOR.PATCH");
        bad("de.jochem.weather", "De Jochem", "id");

        std::os::unix::fs::symlink("/etc/passwd", d.join("link")).unwrap();
        assert!(pack(&d, &out).unwrap_err().to_string().contains("only files and directories"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unpack_refuses_hostile_archives() {
        let root = tmp("evil");
        let manifest = "id='a.b'\nversion='1.0.0'\nname='A'\nentry='x'\n";
        let make = |name: &str, kind: tar::EntryType, link: Option<&str>| -> PathBuf {
            let out = root.join(format!("{}.kpkg", name.replace('/', "_")));
            let mut tar = tar::Builder::new(zstd::Encoder::new(File::create(&out).unwrap(), 1).unwrap());
            let mut h = tar::Header::new_gnu();
            h.set_size(manifest.len() as u64);
            h.set_mode(0o644);
            tar.append_data(&mut h, "manifest.toml", manifest.as_bytes()).unwrap();
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(kind);
            h.set_size(0);
            h.set_mode(0o755);
            if let Some(l) = link {
                h.set_link_name(l).unwrap();
            }
            // append_data refuses `..`; write the raw name instead
            let n = name.as_bytes();
            h.as_old_mut().name[..n.len()].copy_from_slice(n);
            h.set_cksum();
            tar.append(&h, std::io::empty()).unwrap();
            tar.into_inner().unwrap().finish().unwrap();
            out
        };
        for (name, kind, link) in [
            ("../escape", tar::EntryType::Regular, None),
            ("/abs", tar::EntryType::Regular, None),
            ("link", tar::EntryType::Symlink, Some("/etc/passwd")),
            ("hard", tar::EntryType::Link, Some("manifest.toml")),
            ("dev", tar::EntryType::Char, None),
        ] {
            let p = make(name, kind, link);
            let dest = root.join(format!("d-{}", name.replace('/', "_")));
            assert!(unpack(&p, &dest).is_err(), "{name} should be refused");
        }
        assert!(!root.join("escape").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
