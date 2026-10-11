//! Host platform facts: artifact extension, archive format, platform slug.

use std::path::Path;

/// Extension of a built cdylib on this host.
pub const fn dylib_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

/// Executable extension, used when shipping the installer inside a bundle.
pub const fn exe_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "exe"
    } else {
        ""
    }
}

/// File name for an executable called `stem` on this host, e.g. `foo` / `foo.exe`.
pub fn exe_name(stem: &str) -> String {
    let extension = exe_extension();
    if extension.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{extension}")
    }
}

/// Stable slug baked into bundle/archive names.
///
/// Matches the names the release workflow already publishes:
/// `windows-x86_64`, `linux-x86_64`, `macos-x86_64`, `macos-aarch64`.
pub fn slug() -> String {
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "x86" => "i686",
        "arm" => "armv7",
        other => other,
    };
    format!("{os}-{arch}")
}

/// Archive extension `pack` writes on this host: `zip` or `tar.gz`.
pub const fn archive_extension() -> &'static str {
    if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    }
}

/// Archive file name for this platform, e.g. `rpi-packages-linux-x86_64.tar.gz`.
pub fn archive_name() -> String {
    format!("rpi-packages-{}.{}", slug(), archive_extension())
}

/// True when the path looks like a zip archive.
pub fn is_zip(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_os_arch_pair() {
        let s = slug();
        assert!(s.contains('-'), "{s}");
        let (os, arch) = s.split_once('-').unwrap();
        assert!(["windows", "linux", "macos"].contains(&os), "{os}");
        assert!(!arch.is_empty(), "{arch}");
    }

    #[test]
    fn archive_name_tracks_host() {
        let name = archive_name();
        assert!(name.starts_with("rpi-packages-"), "{name}");
        if cfg!(target_os = "windows") {
            assert!(name.ends_with(".zip"), "{name}");
        } else {
            assert!(name.ends_with(".tar.gz"), "{name}");
        }
    }

    #[test]
    fn exe_name_follows_the_host() {
        let name = exe_name("rpi-acp-bridge");
        if cfg!(target_os = "windows") {
            assert_eq!(name, "rpi-acp-bridge.exe");
        } else {
            assert_eq!(name, "rpi-acp-bridge");
        }
    }

    #[test]
    fn zip_detection_is_case_insensitive() {
        assert!(is_zip(Path::new("a.zip")));
        assert!(is_zip(Path::new("a.ZIP")));
        assert!(!is_zip(Path::new("a.tar.gz")));
    }
}
