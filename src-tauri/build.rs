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
    "tokio-util",
    "futures-util",
    "async-trait",
    "tracing-subscriber",
    "toml",
    "semver",
    "uuid",
    "regex",
    "notify-debouncer-full",
    // Protocols
    "suppaftp",
    "russh",
    "russh-sftp",
    "reqwest",
    "quick-xml",
    "oauth2",
    "rustls",
    "ssh2",
    "tokio-rustls",
    "rustls-native-certs",
    "webpki-roots",
    "axum",
    "http",
    "url",
    "urlencoding",
    "percent-encoding",
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
    "aerovault",
    "keyring",
    "aes",
    "cbc",
    "ctr",
    "crypto_secretbox",
    "pbkdf2",
    "sha1",
    "ripemd",
    "md-5",
    "zeroize",
    "subtle",
    "data-encoding",
    "base64",
    "hex",
    "num-bigint-dig",
    "totp-rs",
    "sigstore",
    // Archives
    "sevenz-rust2",
    "zip",
    "tar",
    "flate2",
    "xz2",
    "bzip2",
    "unrar",
    "zstd",
    "reed-solomon-erasure",
    "xxhash-rust",
    // CLI & Tools
    "clap",
    "clap_complete",
    "indicatif",
    "rpassword",
    "ctrlc",
    "globset",
    "ratatui",
    "crossterm",
    "libunftp",
    "unftp-core",
    "rusqlite",
    "dirs",
    "filetime",
    "tempfile",
    "walkdir",
    "mime_guess",
    "open",
    "similar",
    "trash",
    "arboard",
    // System
    "libc",
    "windows",
    "winreg",
    "fuser",
    "gtk",
    "hound",
    "whisper-rs",
    // Plugins
    "tauri-plugin-fs",
    "tauri-plugin-dialog",
    "tauri-plugin-shell",
    "tauri-plugin-notification",
    "tauri-plugin-log",
    "tauri-plugin-single-instance",
    "tauri-plugin-localhost",
    "tauri-plugin-autostart",
    "tauri-plugin-window-state",
];

fn main() {
    // Parse Cargo.lock to extract resolved dependency versions
    let lock_contents = fs::read_to_string("Cargo.lock").expect("Failed to read Cargo.lock");

    let versions = parse_cargo_lock(&lock_contents);

    for dep_name in TRACKED_DEPS {
        let env_key = format!("DEP_VERSION_{}", dep_name.to_uppercase().replace('-', "_"));
        let version = versions
            .get(*dep_name)
            .map(|v| v.as_str())
            .unwrap_or("unknown");
        println!("cargo:rustc-env={env_key}={version}");
    }

    println!("cargo:rerun-if-changed=Cargo.lock");

    // Windows main thread default stack is 1 MB. The aeroftp-cli `Cli` enum
    // produced by clap derive has ~80 subcommand variants and is constructed
    // on the stack during `Cli::parse_from`, blowing the limit before main
    // can even print --help. Bump the reserve to 8 MB (matches POSIX default)
    // for this bin only; other bins are unaffected.
    #[cfg(target_os = "windows")]
    println!("cargo:rustc-link-arg-bin=aeroftp-cli=/STACK:8388608");

    // Detect Rust compiler version at build time: "rustc 1.84.0 (...)" → "1.84.0"
    if let Ok(output) = std::process::Command::new("rustc")
        .arg("--version")
        .output()
    {
        let ver_line = String::from_utf8_lossy(&output.stdout);
        let ver = ver_line.split_whitespace().nth(1).unwrap_or("unknown");
        println!("cargo:rustc-env=RUSTC_VERSION={ver}");
    } else {
        println!("cargo:rustc-env=RUSTC_VERSION=unknown");
    }

    // Optional Linux MTP: link libmtp when pkg-config finds it. CI and hosts
    // without libmtp-dev stay green via NullMtpBackend (cfg mtp_libmtp unset).
    detect_and_link_libmtp();

    // Generate the registered Tauri command list so `aeroftp-cli inventory` can
    // measure CLI/MCP parity against the GUI surface without a hand-maintained
    // list drifting out of sync.
    generate_tauri_command_registry();

    tauri_build::build()
}

/// Emit `TAURI_COMMANDS` from the single `tauri::generate_handler!` block in
/// lib.rs. Deriving the list at build time keeps it always in sync with the real
/// registered surface (no hand-maintained 800-entry const to drift), which is
/// what the inventory parity diff consumes. Fail loud: if the block markers move
/// the build breaks visibly rather than shipping a silently empty surface.
fn generate_tauri_command_registry() {
    let src = fs::read_to_string("src/lib.rs").expect("Failed to read src/lib.rs");
    let mut lines = src.lines();
    // Match the macro invocation itself (`generate_handler![`), not a prose
    // mention of the macro name in a doc comment, which would start the scan at
    // the wrong line and truncate the list.
    let opened = lines.by_ref().any(|l| l.contains("generate_handler!["));
    assert!(
        opened,
        "build.rs: no `tauri::generate_handler![` block found in lib.rs; the Tauri command registry generator needs updating"
    );
    let mut names: Vec<(Vec<String>, String)> = Vec::new();
    // A `#[cfg(...)]` in the handler list applies to the command on the NEXT
    // line. Dropping the attribute and keeping the command made the generated
    // list advertise commands the binary does not register: a release build
    // reported the debug-only ones, and a no-default-features build reported the
    // feature-gated ones, while the file calls itself the registered surface.
    // Carry the attributes onto the generated array elements and let rustc
    // evaluate exactly the conditions it evaluated for the real list, rather
    // than reimplementing Cargo's feature resolution here.
    let mut pending_cfg: Vec<String> = Vec::new();
    for line in lines.by_ref() {
        let s = line.trim();
        if s.starts_with("])") {
            break;
        }
        if s.starts_with("#[cfg(") {
            pending_cfg.push(s.to_string());
            continue;
        }
        // Skip blank lines, other attributes and comments.
        if s.is_empty() || s.starts_with("#[") || s.starts_with("//") {
            continue;
        }
        // The command name is the final path segment (module::fn -> fn).
        let tok = s.trim_end_matches(',').trim();
        let name = tok.rsplit("::").next().unwrap_or(tok);
        let valid = !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
        if valid {
            names.push((std::mem::take(&mut pending_cfg), name.to_string()));
        } else {
            // Not a command line, and not an attribute either. Silently clearing
            // here would put us straight back in the defect this fixes: a cfg
            // split across lines would drop its own continuation, then emit the
            // NEXT command unguarded, and the generated list would quietly claim
            // a command the binary may not contain. Every cfg in the block is on
            // one line today, so this is a trap rather than a bug, and it fails
            // loudly instead of waiting to be discovered.
            assert!(
                pending_cfg.is_empty(),
                "build.rs: `{s}` follows a #[cfg(...)] in the generate_handler! block but is not a command. \
                 A multi-line cfg attribute is not supported: put it on one line, or teach this parser to join them."
            );
        }
    }
    assert!(
        names.len() > 500,
        "build.rs: parsed only {} Tauri commands from lib.rs, expected the full handler list; the generate_handler! format may have changed",
        names.len()
    );
    let mut body = String::from(
        "// @generated by build.rs from the tauri::generate_handler! block in lib.rs.\n// Do not edit by hand. Registered Tauri (GUI) command names, consumed by\n// `aeroftp-cli inventory` to diff the GUI surface against CLI and MCP.\npub const TAURI_COMMANDS: &[&str] = &[\n",
    );
    for (attrs, n) in &names {
        for attr in attrs {
            body.push_str("    ");
            body.push_str(attr);
            body.push('\n');
        }
        body.push_str("    \"");
        body.push_str(n);
        body.push_str("\",\n");
    }
    body.push_str("];\n");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let dest = std::path::Path::new(&out_dir).join("tauri_commands.rs");
    fs::write(&dest, body).expect("Failed to write tauri_commands.rs");
    println!("cargo:rerun-if-changed=src/lib.rs");
}

/// Probe for system libmtp (Linux). Emits `cargo:rustc-cfg=mtp_libmtp` and link
/// flags when present. Never fails the build when absent.
fn detect_and_link_libmtp() {
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=AEROFTP_DISABLE_LIBMTP");

    if std::env::var_os("AEROFTP_DISABLE_LIBMTP").is_some() {
        println!("cargo:warning=AEROFTP_DISABLE_LIBMTP set; Linux MTP backend disabled");
        println!("cargo:rustc-env=AEROFTP_MTP_BACKEND=null");
        return;
    }

    let target = std::env::var("TARGET").unwrap_or_default();
    // Windows always uses the in-tree WPD backend (system COM, no extra DLL).
    if target.contains("windows") {
        println!("cargo:rustc-env=AEROFTP_MTP_BACKEND=wpd");
        return;
    }
    // Non-Linux, non-Windows (e.g. macOS): Null until ImageCapture lands.
    if !target.contains("linux") {
        println!("cargo:rustc-env=AEROFTP_MTP_BACKEND=null");
        return;
    }

    let status = std::process::Command::new("pkg-config")
        .args(["--exists", "libmtp"])
        .status();
    let found = matches!(status, Ok(s) if s.success());
    if !found {
        println!(
            "cargo:warning=libmtp not found (pkg-config libmtp); install libmtp-dev for portable-device support"
        );
        println!("cargo:rustc-env=AEROFTP_MTP_BACKEND=null");
        return;
    }

    // --libs-only-L / --libs-only-l keep us from injecting raw -Wl flags.
    if let Ok(output) = std::process::Command::new("pkg-config")
        .args(["--libs-only-L", "libmtp"])
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            for part in s.split_whitespace() {
                if let Some(path) = part.strip_prefix("-L") {
                    if !path.is_empty() {
                        println!("cargo:rustc-link-search=native={path}");
                    }
                }
            }
        }
    }
    if let Ok(output) = std::process::Command::new("pkg-config")
        .args(["--libs-only-l", "libmtp"])
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout);
            for part in s.split_whitespace() {
                if let Some(lib) = part.strip_prefix("-l") {
                    if !lib.is_empty() {
                        println!("cargo:rustc-link-lib={lib}");
                    }
                }
            }
        } else {
            println!("cargo:rustc-link-lib=mtp");
        }
    } else {
        println!("cargo:rustc-link-lib=mtp");
    }

    println!("cargo:rustc-cfg=mtp_libmtp");
    println!("cargo:rustc-env=AEROFTP_MTP_BACKEND=libmtp");
    if let Ok(output) = std::process::Command::new("pkg-config")
        .args(["--modversion", "libmtp"])
        .output()
    {
        if output.status.success() {
            let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("cargo:rustc-env=AEROFTP_LIBMTP_VERSION={ver}");
            println!("cargo:warning=linking libmtp {ver} for MTP portable-device backend");
        }
    }
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
                        Some(existing) => {
                            compare_semver(ver, existing) == std::cmp::Ordering::Greater
                        }
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
    let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    parse(a).cmp(&parse(b))
}
