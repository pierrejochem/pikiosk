//! kpkg: build and inspect pikiosk app packages.
//!
//!   kpkg build <dir> [-o out.kpkg]   lint <dir> and write the package
//!   kpkg inspect <file.kpkg>         print the manifest
//!   kpkg verify <file.kpkg>          check that the package unpacks cleanly

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

fn usage() -> ! {
    eprintln!("usage: kpkg build <dir> [-o out.kpkg] | inspect <file.kpkg> | verify <file.kpkg>");
    std::process::exit(2);
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["build", dir] => build(Path::new(dir), None),
        ["build", dir, "-o", out] => build(Path::new(dir), Some(PathBuf::from(out))),
        ["inspect", file] => {
            let m = kpkg::read_manifest(Path::new(file))?;
            println!("{} {} ({})", m.id, m.version, m.name);
            println!("entry: {}", m.entry);
            println!("boards: {}", if m.boards.is_empty() { "all".into() } else { m.boards.join(", ") });
            println!("permissions: {}", if m.permissions.is_empty() { "none".into() } else { m.permissions.join(", ") });
            Ok(())
        }
        ["verify", file] => {
            let tmp = std::env::temp_dir().join(format!("kpkg-verify-{}", std::process::id()));
            let r = kpkg::unpack(Path::new(file), &tmp).context("unpacking");
            let _ = std::fs::remove_dir_all(&tmp);
            let m = r?;
            println!("{file}: ok ({} {})", m.id, m.version);
            Ok(())
        }
        _ => usage(),
    }
}

fn build(dir: &Path, out: Option<PathBuf>) -> Result<()> {
    let text = std::fs::read_to_string(dir.join("manifest.toml")).context("reading manifest.toml")?;
    let m = kpkg::Manifest::parse(&text)?;
    let out = out.unwrap_or_else(|| PathBuf::from(format!("{}-{}.kpkg", m.id, m.version)));
    kpkg::pack(dir, &out)?;
    println!("{} ({} bytes)", out.display(), std::fs::metadata(&out)?.len());
    Ok(())
}
