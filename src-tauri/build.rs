use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Panel category of every crate a manifest lists as a direct dependency, in
/// the order the Dependencies panel shows the categories.
///
/// This table is the only hand-written part of the panel. The rows, their
/// versions and their requirements come from the manifests and `Cargo.lock`
/// (see `generate_dependency_index`), and the build fails in both directions:
/// when a manifest names a crate this table does not classify, and when the
/// table classifies a crate no manifest names any more.
const DEPENDENCY_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "Core",
        &[
            "anyhow",
            "async-trait",
            "bytes",
            "chrono",
            "futures-lite",
            "futures-util",
            "image",
            "image_hasher",
            "log",
            "notify",
            "notify-debouncer-full",
            "portable-pty",
            "regex",
            "semver",
            "serde",
            "serde_json",
            "tauri",
            "thiserror",
            "tlsh2",
            "tokio",
            "tokio-util",
            "toml",
            "tracing",
            "tracing-subscriber",
            "uuid",
        ],
    ),
    (
        "Protocols",
        &[
            "axum",
            "http",
            "oauth2",
            "percent-encoding",
            "quick-xml",
            "reqwest",
            "russh",
            "russh-sftp",
            "rustls",
            "rustls-native-certs",
            "rustls-pki-types",
            "ssh2",
            "suppaftp",
            "tokio-rustls",
            "url",
            "urlencoding",
            "webpki-roots",
        ],
    ),
    (
        "Security",
        &[
            "aerovault",
            "aes",
            "aes-gcm",
            "aes-gcm-siv",
            "aes-kw",
            "aes-siv",
            "argon2",
            "base64",
            "blake3",
            "cbc",
            "chacha20poly1305",
            "crypto_secretbox",
            "ctr",
            "data-encoding",
            "ed25519-dalek",
            "hex",
            "hkdf",
            "hmac",
            "jsonwebtoken",
            "keyring",
            "md-5",
            "md4",
            "num-bigint-dig",
            "pbkdf2",
            "rand",
            "ring",
            "ripemd",
            "scrypt",
            "secrecy",
            "sha1",
            "sha2",
            "sigstore",
            "subtle",
            "totp-rs",
            "x25519-dalek",
            "zeroize",
        ],
    ),
    (
        "Archives",
        &[
            "bzip2",
            "flate2",
            "reed-solomon-erasure",
            "sevenz-rust2",
            "tar",
            "unrar",
            "xxhash-rust",
            "xz2",
            "zip",
            "zstd",
        ],
    ),
    (
        "CLI & Tools",
        &[
            "arboard",
            "clap",
            "clap_complete",
            "crossterm",
            "ctrlc",
            "dirs",
            "filetime",
            "globset",
            "indicatif",
            "libunftp",
            "mime_guess",
            "open",
            "ratatui",
            "rpassword",
            "rusqlite",
            "similar",
            "tempfile",
            "trash",
            "unftp-core",
            "walkdir",
        ],
    ),
    (
        "P2P",
        &[
            "iroh",
            "iroh-blobs",
            "iroh-docs",
            "iroh-gossip",
            "iroh-mainline-address-lookup",
            "iroh-mdns-address-lookup",
        ],
    ),
    (
        "System",
        &[
            "acl-sys",
            "fuser",
            "gtk",
            "hound",
            "libc",
            "posix-acl",
            "whisper-rs",
            "windows",
            "winreg",
        ],
    ),
    (
        "Plugins",
        &[
            "tauri-plugin-autostart",
            "tauri-plugin-dialog",
            "tauri-plugin-fs",
            "tauri-plugin-localhost",
            "tauri-plugin-log",
            "tauri-plugin-notification",
            "tauri-plugin-shell",
            "tauri-plugin-single-instance",
            "tauri-plugin-window-state",
        ],
    ),
];

fn main() {
    // The Dependencies panel list and the DEP_VERSION_* env vars, both derived
    // from the manifests and Cargo.lock.
    generate_dependency_index();

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
    detect_and_link_libacl();

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

    // Commands a release build does not register, listed unconditionally.
    //
    // The array above is cfg-aware, which is the point, but it also means a
    // debug build and a release build report different surfaces. The committed
    // snapshot records the SHIPPED surface, so without this the drift check
    // would be a false alarm in the profile developers actually use, and a check
    // that cries wolf where you work is a check that gets ignored. Subtracting
    // this list makes the reported surface the same in both profiles.
    body.push_str(
        "\n// Registered only when `debug_assertions` is on, so absent from a shipped\n// binary. Subtract these to get the release surface from any build.\npub const TAURI_DEBUG_ONLY_COMMANDS: &[&str] = &[\n",
    );
    for (attrs, n) in &names {
        if attrs.iter().any(|a| a.contains("debug_assertions")) {
            body.push_str("    \"");
            body.push_str(n);
            body.push_str("\",\n");
        }
    }
    body.push_str("];\n");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let dest = std::path::Path::new(&out_dir).join("tauri_commands.rs");
    fs::write(&dest, body).expect("Failed to write tauri_commands.rs");
    println!("cargo:rerun-if-changed=src/lib.rs");
}

/// POSIX ACL B2: `acl-sys` links `-lacl`. Hosts with only `libacl1` (no
/// `-dev` package) ship `libacl.so.1` without the unversioned `libacl.so`
/// symlink. Point rustc at a local SONAME symlink so the notebook can
/// compile without installing headers.
fn detect_and_link_libacl() {
    #[cfg(target_os = "linux")]
    {
        use std::path::PathBuf;
        let target = std::env::var("TARGET").unwrap_or_default();
        let host = std::env::var("HOST").unwrap_or_default();
        if !target.contains("-linux-") || target != host {
            // Never offer a host library to a cross-target linker. A configured
            // cross toolchain must resolve libacl from its own sysroot.
            return;
        }
        let multiarch = match target.split('-').next().unwrap_or_default() {
            "x86_64" => "x86_64-linux-gnu",
            "aarch64" => "aarch64-linux-gnu",
            "arm" => "arm-linux-gnueabihf",
            "riscv64" => "riscv64-linux-gnu",
            _ => return,
        };
        let candidates = [
            PathBuf::from(format!("/usr/lib/{multiarch}/libacl.so")),
            PathBuf::from(format!("/usr/lib/{multiarch}/libacl.so.1")),
            PathBuf::from(format!("/lib/{multiarch}/libacl.so")),
            PathBuf::from(format!("/lib/{multiarch}/libacl.so.1")),
            PathBuf::from("/usr/lib/libacl.so"),
            PathBuf::from("/usr/lib/libacl.so.1"),
        ];
        let Some(found) = candidates.iter().find(|p| p.exists()) else {
            return;
        };
        if found.file_name().and_then(|n| n.to_str()) == Some("libacl.so") {
            if let Some(dir) = found.parent() {
                println!("cargo:rustc-link-search=native={}", dir.display());
            }
            return;
        }
        let out = std::env::var("OUT_DIR").expect("OUT_DIR");
        let dest = PathBuf::from(&out).join("libacl.so");
        let _ = fs::remove_file(&dest);
        let _ = std::os::unix::fs::symlink(found, &dest);
        println!("cargo:rustc-link-search=native={out}");
    }
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

/// A direct dependency as a manifest declares it.
struct ManifestDependency {
    /// The key in the manifest table: the name the code imports, which differs
    /// from `package` for a renamed dependency such as `rand_010`.
    key: String,
    package: String,
    requirement: String,
}

/// Direct dependencies of `manifest`, all targets included, followed by those
/// of every path crate it pulls in: a path crate ships in the same binary, so
/// its dependencies are ours too. The path crates are walked after the
/// manifest's own table, because `toml::Table` iterates keys in sorted order
/// and `aeroftp-peer-l0` would otherwise claim the shared crates first.
fn manifest_dependencies(manifest: &Path, out: &mut Vec<ManifestDependency>) {
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = fs::read_to_string(manifest)
        .unwrap_or_else(|e| panic!("build.rs: cannot read {}: {e}", manifest.display()));
    let doc: toml::Table = text
        .parse()
        .unwrap_or_else(|e| panic!("build.rs: cannot parse {}: {e}", manifest.display()));

    let mut tables: Vec<&toml::Table> = Vec::new();
    if let Some(table) = doc.get("dependencies").and_then(toml::Value::as_table) {
        tables.push(table);
    }
    if let Some(targets) = doc.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            if let Some(table) = target.get("dependencies").and_then(toml::Value::as_table) {
                tables.push(table);
            }
        }
    }

    let mut path_crates = Vec::new();
    for table in tables {
        for (key, value) in table {
            if let Some(path) = value.get("path").and_then(toml::Value::as_str) {
                let parent = manifest.parent().unwrap_or_else(|| Path::new("."));
                path_crates.push(parent.join(path).join("Cargo.toml"));
                continue;
            }
            let package = value
                .get("package")
                .and_then(toml::Value::as_str)
                .unwrap_or(key)
                .to_string();
            let requirement = value
                .as_str()
                .or_else(|| value.get("version").and_then(toml::Value::as_str))
                .unwrap_or("*")
                .to_string();
            out.push(ManifestDependency {
                key: key.clone(),
                package,
                requirement,
            });
        }
    }
    for child in path_crates {
        manifest_dependencies(&child, out);
    }
}

/// Every version `Cargo.lock` holds for each package name.
fn locked_versions(lock: &str) -> BTreeMap<String, Vec<semver::Version>> {
    let doc: toml::Table = lock.parse().expect("build.rs: cannot parse Cargo.lock");
    let mut versions: BTreeMap<String, Vec<semver::Version>> = BTreeMap::new();
    let packages = doc.get("package").and_then(toml::Value::as_array);
    for package in packages.into_iter().flatten() {
        let name = package.get("name").and_then(toml::Value::as_str);
        let version = package.get("version").and_then(toml::Value::as_str);
        if let (Some(name), Some(Ok(version))) = (name, version.map(semver::Version::parse)) {
            versions.entry(name.to_string()).or_default().push(version);
        }
    }
    versions
}

/// Write `dependencies.rs` for `src/dependency_index.rs`, and emit
/// `DEP_VERSION_<KEY>` for `get_system_info` and the sigstore verifier.
///
/// The version of a row is the newest locked version the manifest requirement
/// admits, not the newest version of that name in the lock: 13 crates are
/// locked more than once (`aes-gcm` 0.10.3 is ours, 0.11.0 arrives through
/// another crate), and the hand-kept list this replaces showed the wrong one
/// for every one of them.
fn generate_dependency_index() {
    let mut declared = Vec::new();
    manifest_dependencies(Path::new("Cargo.toml"), &mut declared);
    let lock = fs::read_to_string("Cargo.lock").expect("Failed to read Cargo.lock");
    let locked = locked_versions(&lock);

    let mut category_of: BTreeMap<&str, (usize, &str)> = BTreeMap::new();
    for (rank, (category, crates)) in DEPENDENCY_CATEGORIES.iter().enumerate() {
        for name in *crates {
            assert!(
                category_of.insert(name, (rank, category)).is_none(),
                "build.rs: {name} is listed twice in DEPENDENCY_CATEGORIES"
            );
        }
    }
    let declared_names: HashSet<&str> = declared.iter().map(|d| d.package.as_str()).collect();
    let mut unclassified: Vec<&str> = declared_names
        .iter()
        .copied()
        .filter(|name| !category_of.contains_key(name))
        .collect();
    unclassified.sort_unstable();
    let stale: Vec<&str> = category_of
        .keys()
        .copied()
        .filter(|name| !declared_names.contains(name))
        .collect();
    assert!(
        unclassified.is_empty() && stale.is_empty(),
        "build.rs: DEPENDENCY_CATEGORIES is out of sync with the manifests.\n  \
         give a category to: {unclassified:?}\n  \
         remove, no manifest declares them: {stale:?}"
    );

    let mut rows: Vec<(usize, String, String, String, &str)> = Vec::new();
    let mut listed: HashSet<(String, String)> = HashSet::new();
    let mut env_keys: HashSet<String> = HashSet::new();
    for dep in &declared {
        let version = semver::VersionReq::parse(&dep.requirement)
            .ok()
            .and_then(|req| {
                locked
                    .get(&dep.package)?
                    .iter()
                    .filter(|v| req.matches(v))
                    .max_by(|a, b| a.cmp_precedence(b))
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| "unknown".to_string());

        let env_key = format!("DEP_VERSION_{}", dep.key.to_uppercase().replace('-', "_"));
        if env_keys.insert(env_key.clone()) {
            println!("cargo:rustc-env={env_key}={version}");
        }
        if listed.insert((dep.package.clone(), version.clone())) {
            let (rank, category) = category_of[dep.package.as_str()];
            rows.push((
                rank,
                dep.package.clone(),
                version,
                dep.requirement.clone(),
                category,
            ));
        }
    }
    rows.sort_by(|a, b| (a.0, &a.1, &a.2).cmp(&(b.0, &b.1, &b.2)));

    let mut code = String::from(
        "// Generated by build.rs from the manifests and Cargo.lock. Do not edit.\n\
         pub(crate) const DEPENDENCIES: &[DependencyEntry] = &[\n",
    );
    for (_, name, version, requirement, category) in &rows {
        code.push_str(&format!(
            "    DependencyEntry {{ name: {name:?}, version: {version:?}, requirement: {requirement:?}, category: {category:?} }},\n"
        ));
    }
    code.push_str("];\n");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    fs::write(out_dir.join("dependencies.rs"), code)
        .expect("build.rs: cannot write dependencies.rs");
}
