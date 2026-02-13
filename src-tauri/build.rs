use std::collections::HashMap;
use std::fs;

/// Dependencies to expose as compile-time env vars for the About dialog
const TRACKED_DEPS: &[&str] = &[
    // Core
    "tauri",
    "tokio",
    "serde",
    "serde_json",
    "anyhow",
    "thiserror",
    "chrono",
    "log",
    "tracing",
    "portable-pty",
    "notify",
    "image",
    // Protocols
    "suppaftp",
    "russh",
    "russh-sftp",
    "reqwest",
    "quick-xml",
    "oauth2",
    "native-tls",
    // Security
    "argon2",
    "aes-gcm",
    "aes-gcm-siv",
    "chacha20poly1305",
    "hkdf",
    "aes-kw",
    "aes-siv",
    "scrypt",
    "ring",
    "secrecy",
    "sha2",
    "hmac",
    "blake3",
    "jsonwebtoken",
    // Archives
    "sevenz-rust",
    "zip",
    "tar",
    "flate2",
    "xz2",
    "bzip2",
    "unrar",
    // Plugins
    "tauri-plugin-fs",
    "tauri-plugin-dialog",
    "tauri-plugin-shell",
    "tauri-plugin-notification",
    "tauri-plugin-log",
    "tauri-plugin-single-instance",
    "tauri-plugin-localhost",
    "tauri-plugin-autostart",
];

fn main() {
    // Parse Cargo.lock to extract resolved dependency versions
    let lock_contents = fs::read_to_string("Cargo.lock")
        .expect("Failed to read Cargo.lock");

    let versions = parse_cargo_lock(&lock_contents);

    for dep_name in TRACKED_DEPS {
        let env_key = format!("DEP_VERSION_{}", dep_name.to_uppercase().replace('-', "_"));
        let version = versions.get(*dep_name).map(|v| v.as_str()).unwrap_or("unknown");
        println!("cargo:rustc-env={env_key}={version}");
    }

    println!("cargo:rerun-if-changed=Cargo.lock");

    // Detect Rust compiler version at build time: "rustc 1.84.0 (...)" → "1.84.0"
    if let Ok(output) = std::process::Command::new("rustc").arg("--version").output() {
        let ver_line = String::from_utf8_lossy(&output.stdout);
        let ver = ver_line.split_whitespace().nth(1).unwrap_or("unknown");
        println!("cargo:rustc-env=RUSTC_VERSION={ver}");
    } else {
        println!("cargo:rustc-env=RUSTC_VERSION=unknown");
    }

    tauri_build::build()
}

/// Parse Cargo.lock and return highest version for each package name.
/// When a crate appears multiple times (e.g. reqwest 0.11 as transitive + 0.13 as direct),
/// we keep the highest semver version which corresponds to our direct dependency.
fn parse_cargo_lock(contents: &str) -> HashMap<String, String> {
    let mut versions: HashMap<String, String> = HashMap::new();
    let mut current_name: Option<String> = None;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("name = ") {
            current_name = trimmed
                .strip_prefix("name = \"")
                .and_then(|s| s.strip_suffix('"'))
                .map(|s| s.to_string());
        } else if trimmed.starts_with("version = ") {
            if let Some(ref name) = current_name {
                if let Some(ver) = trimmed
                    .strip_prefix("version = \"")
                    .and_then(|s| s.strip_suffix('"'))
                {
                    let should_replace = match versions.get(name) {
                        None => true,
                        Some(existing) => compare_semver(ver, existing) == std::cmp::Ordering::Greater,
                    };
                    if should_replace {
                        versions.insert(name.clone(), ver.to_string());
                    }
                }
            }
            current_name = None;
        }
    }

    versions
}

/// Simple semver comparison: split on '.' and compare numerically
fn compare_semver(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.').filter_map(|p| p.parse().ok()).collect()
    };
    parse(a).cmp(&parse(b))
}
