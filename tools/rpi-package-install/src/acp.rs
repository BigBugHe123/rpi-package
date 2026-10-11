//! Package the standalone `rpi-acp-bridge` binary for the ACP Registry.
//!
//! `pack` produces the extension bundle an rpi host installs; this produces what
//! the ACP Registry's `distribution.binary` expects: a per-platform archive
//! with one agent binary at its root, plus a ready-to-paste `agent.json` stanza.
//!
//! The bridge speaks ACP on its own stdin/stdout and drives child
//! `rpi --mode rpc` processes, so an ACP client (Zed) can launch it directly —
//! no rpi host extension loading, no host `ownsStdio` change.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::platform;

/// Cargo bin target in `packages/rpi-acp` (`[[bin]] name`).
pub const BINARY_STEM: &str = "rpi-acp-bridge";

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub manifest: Option<PathBuf>,
    /// Where `dist/` lives (default: `<workspace>/dist`).
    pub out_dir: Option<PathBuf>,
    /// Stop after staging, do not create the archive.
    pub skip_archive: bool,
}

pub struct Outcome {
    pub stage_dir: PathBuf,
    pub binary: PathBuf,
    pub archive: Option<PathBuf>,
    pub sha256: Option<String>,
}

pub fn run(opts: &Options) -> Result<Outcome> {
    let manifest = opts
        .manifest
        .clone()
        .unwrap_or_else(|| PathBuf::from("Cargo.toml"));
    let workspace = crate::metadata::load(&manifest)?;
    let root = workspace.root.clone();
    let out_dir = opts.out_dir.clone().unwrap_or_else(|| root.join("dist"));

    let binary_name = platform::exe_name(BINARY_STEM);
    let built = workspace.release_dir().join(&binary_name);
    if !built.is_file() {
        bail!(
            "{} not found; build it first:\n    cargo build --release -p rpi-acp --bin {BINARY_STEM}",
            built.display()
        );
    }

    let slug = platform::slug();
    let stage_dir = out_dir.join(format!("rpi-acp-{slug}"));
    if stage_dir.exists() {
        std::fs::remove_dir_all(&stage_dir)
            .with_context(|| format!("cannot clear {}", stage_dir.display()))?;
    }
    std::fs::create_dir_all(&stage_dir)
        .with_context(|| format!("cannot create {}", stage_dir.display()))?;

    let binary = stage_dir.join(&binary_name);
    std::fs::copy(&built, &binary).with_context(|| format!("cannot stage {binary_name}"))?;

    // Ship the adapter's README next to the binary; the registry archive is a
    // plain bundle, so a note about `RPI_BIN` belongs with it.
    let readme = root.join("packages/rpi-acp/README.md");
    if readme.is_file() {
        std::fs::copy(&readme, stage_dir.join("README.md"))
            .context("cannot stage packages/rpi-acp/README.md")?;
    }

    if opts.skip_archive {
        println!("staged {binary_name} -> {}", stage_dir.display());
        return Ok(Outcome {
            stage_dir,
            binary,
            archive: None,
            sha256: None,
        });
    }

    let archive = out_dir.join(format!("rpi-acp-{slug}.{}", platform::archive_extension()));
    crate::archive::create_flat(&stage_dir, &archive)?;
    let sha256 = sha256_file(&archive).ok();

    println!("staged  {binary_name} -> {}", stage_dir.display());
    println!("archive -> {}", archive.display());
    if let Some(sum) = &sha256 {
        let file = archive
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        println!("sha256  -> {sum}");
        println!();
        println!("registry distribution.binary entry for \"{slug}\":");
        println!("  \"{slug}\": {{");
        println!("    \"archive\": \"<release-url>/{file}\",");
        println!("    \"cmd\": \"./{binary_name}\",");
        println!("    \"args\": [],\n    \"sha256\": \"{sum}\"\n  }}");
    }

    Ok(Outcome {
        stage_dir,
        binary,
        archive: Some(archive),
        sha256,
    })
}

/// SHA-256 of a file, as the lowercase hex the registry records.
fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // sha256("abc") — the canonical NIST test vector.
        let path = std::env::temp_dir().join("rpi-acp-sha256-abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sha256_reports_missing_files() {
        let missing = std::env::temp_dir().join("rpi-acp-sha256-nope.bin");
        let _ = std::fs::remove_file(&missing);
        assert!(sha256_file(&missing).is_err());
    }
}
