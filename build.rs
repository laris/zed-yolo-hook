use std::path::{Path, PathBuf};
use std::process::Command;

/// Frida version to use (must match FRIDA_VERSION in frida-gum-sys checkout).
const FRIDA_VERSION: &str = "17.7.3";
const DEVKIT_NAME: &str = "frida-gum-devkit";
const DEVKIT_ARCH: &str = "macos-arm64";

fn main() {
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");

    // Ensure Frida devkit is available for frida-gum-sys build.
    // Fallback chain:
    //   1. Project cache: target/frida-cache/frida-gum.h
    //   2. User download dir: ~/Downloads/frida/<devkit>.tar.xz
    //   3. Fetch from GitHub releases
    if let Some(frida_sys_dir) = find_frida_gum_sys_dir() {
        let marker = frida_sys_dir.join("frida-gum.h");
        if marker.exists() {
            println!(
                "cargo:warning=Frida devkit already present at {}",
                frida_sys_dir.display()
            );
            return;
        }

        let tarball = format!("{DEVKIT_NAME}-{FRIDA_VERSION}-{DEVKIT_ARCH}.tar.xz");
        let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cache_dir = project_root.join("target/frida-cache");
        let cached_tarball = cache_dir.join(&tarball);

        // Fallback 1: project cache
        if cached_tarball.exists() {
            println!(
                "cargo:warning=Using cached Frida devkit from {}",
                cached_tarball.display()
            );
            extract_tarball(&cached_tarball, &frida_sys_dir);
            patch_frida_version(&frida_sys_dir);
            return;
        }

        // Fallback 2: ~/Downloads/frida/
        let home = std::env::var("HOME").unwrap_or_default();
        let downloads_tarball = PathBuf::from(&home)
            .join("Downloads/frida")
            .join(&tarball);
        if downloads_tarball.exists() {
            println!(
                "cargo:warning=Copying Frida devkit from {} to cache",
                downloads_tarball.display()
            );
            let _ = std::fs::create_dir_all(&cache_dir);
            let _ = std::fs::copy(&downloads_tarball, &cached_tarball);
            extract_tarball(&cached_tarball, &frida_sys_dir);
            patch_frida_version(&frida_sys_dir);
            return;
        }

        // Fallback 3: download from GitHub
        let url = format!(
            "https://github.com/frida/frida/releases/download/{FRIDA_VERSION}/{tarball}"
        );
        println!("cargo:warning=Downloading Frida devkit from {url}");
        let _ = std::fs::create_dir_all(&cache_dir);

        let status = Command::new("curl")
            .arg("-fSL")
            .arg("--retry")
            .arg("3")
            .arg("--retry-delay")
            .arg("5")
            .arg("-o")
            .arg(&cached_tarball)
            .arg(&url)
            .status();

        match status {
            Ok(s) if s.success() && cached_tarball.exists() => {
                println!("cargo:warning=Downloaded Frida devkit to cache");
                extract_tarball(&cached_tarball, &frida_sys_dir);
                patch_frida_version(&frida_sys_dir);
            }
            _ => {
                println!(
                    "cargo:warning=Failed to download Frida devkit. \
                     Please manually place {tarball} in ~/Downloads/frida/ or target/frida-cache/"
                );
            }
        }
    }
}

/// Find the frida-gum-sys source directory in Cargo's git checkout or registry.
fn find_frida_gum_sys_dir() -> Option<PathBuf> {
    let cargo_home = std::env::var("CARGO_HOME")
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            format!("{home}/.cargo")
        });

    // Git checkout (when using git dependency)
    let git_checkouts = PathBuf::from(&cargo_home).join("git/checkouts");
    if let Ok(entries) = std::fs::read_dir(&git_checkouts) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("frida-rust-") {
                if let Ok(revs) = std::fs::read_dir(entry.path()) {
                    for rev in revs.flatten() {
                        let sys_dir = rev.path().join("frida-gum-sys");
                        if sys_dir.exists() {
                            return Some(sys_dir);
                        }
                    }
                }
            }
        }
    }

    // Registry source (when using version dependency)
    let registry_src = PathBuf::from(&cargo_home).join("registry/src");
    if let Ok(mirrors) = std::fs::read_dir(&registry_src) {
        for mirror in mirrors.flatten() {
            if let Ok(crates) = std::fs::read_dir(mirror.path()) {
                for krate in crates.flatten() {
                    let name = krate.file_name();
                    if name.to_string_lossy().starts_with("frida-gum-sys-") {
                        return Some(krate.path());
                    }
                }
            }
        }
    }

    None
}

fn extract_tarball(tarball: &Path, dest: &Path) {
    let status = Command::new("tar")
        .arg("xf")
        .arg(tarball)
        .current_dir(dest)
        .status();

    match status {
        Ok(s) if s.success() => {
            println!(
                "cargo:warning=Extracted Frida devkit to {}",
                dest.display()
            );
        }
        _ => {
            println!(
                "cargo:warning=Failed to extract {} to {}",
                tarball.display(),
                dest.display()
            );
        }
    }
}

/// Patch FRIDA_VERSION file to match the devkit we placed.
fn patch_frida_version(frida_sys_dir: &Path) {
    let version_file = frida_sys_dir.join("FRIDA_VERSION");
    if version_file.exists() {
        if let Ok(current) = std::fs::read_to_string(&version_file) {
            if current.trim() != FRIDA_VERSION {
                let _ = std::fs::write(&version_file, FRIDA_VERSION);
                println!(
                    "cargo:warning=Patched FRIDA_VERSION: {} -> {FRIDA_VERSION}",
                    current.trim()
                );
            }
        }
    }
}
