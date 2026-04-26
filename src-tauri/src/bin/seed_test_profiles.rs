// Test helper: seeds docker harness profiles into the encrypted vault.
// Additive-only — does not touch runtime code paths.
// Run: cargo run --bin seed_test_profiles
//
// Optional: set `AEROFTP_SEED_AERORSYNC_E2E=1` to also seed two SFTP profiles
// pointed at a `linuxserver/openssh-server` container with rsync installed,
// for end-to-end verification of the AeroRsync delta path on top of the
// cross-profile transfer flow. Container setup required:
//
//   docker run -d --name bench-ssh-rsync -p 127.0.0.1:2242:2222 \
//     -e PUBLIC_KEY="$(cat ~/.ssh/id_ed25519.pub)" \
//     -e USER_NAME=testuser \
//     linuxserver/openssh-server:latest
//   docker exec bench-ssh-rsync apk add --no-cache rsync
//   docker exec bench-ssh-rsync sh -c '
//     rm -f /config/ssh_host_keys/ssh_host_rsa_key* \
//           /config/ssh_host_keys/ssh_host_ecdsa_key*'
//   docker restart bench-ssh-rsync
//   docker exec bench-ssh-rsync mkdir -p /config/source /config/dest
//
// The non-ed25519 host key removal is mandatory: libssh2 (used by classic
// SFTP) and russh (used by the AeroRsync probe) negotiate different host
// key algorithms when multiple are present, which trips the U-02 host-key
// fingerprint pinning gate and forces a soft fallback. Removing the rsa
// and ecdsa keys forces both libraries onto the same ed25519 key.
//
// Then enable the runtime toggle: `echo "enabled = true" >
// ~/.config/aeroftp/native_rsync.toml`. Verify with:
//   aeroftp-cli transfer 'AeroRsync E2E A' 'AeroRsync E2E B' \
//     /config/source/<file> /config/dest/<file>
// and look for `delta sync Upload ok: transport=aerorsync-proto-31` in -vv
// output.

use ftp_client_gui_lib::credential_store::CredentialStore;
use serde_json::{json, Value};

const PROFILES_KEY: &str = "config_server_profiles";

fn profile_id(slug: &str) -> String {
    format!("srv_test_{}", slug)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CredentialStore::init()?;
    let store = CredentialStore::from_cache().ok_or("vault not initialized")?;

    let existing = store.get(PROFILES_KEY).unwrap_or_else(|_| "[]".to_string());
    let mut profiles: Vec<Value> = serde_json::from_str(&existing).unwrap_or_default();

    let mut seeds = vec![
        json!({
            "id": profile_id("ftp_docker"),
            "name": "Docker FTP",
            "protocol": "ftp",
            "host": "127.0.0.1",
            "port": 2121,
            "username": "ftpuser",
            "initialPath": "/",
        }),
        json!({
            "id": profile_id("ftps_docker"),
            "name": "Docker FTPS",
            "protocol": "ftps",
            "host": "127.0.0.1",
            "port": 2121,
            "username": "ftpuser",
            "initialPath": "/",
            "options": {
                "tls_mode": "explicit",
                "verify_cert": "false"
            }
        }),
        json!({
            "id": profile_id("sftp_docker_ed25519"),
            "name": "Docker SFTP ed25519",
            "protocol": "sftp",
            "host": "127.0.0.1",
            "port": 2223,
            "username": "user_key",
            "initialPath": "/",
            "options": {
                "private_key_path": "/var/www/html/FTP_CLIENT_GUI/docs/dev/test-workspace/docker-harness/keys/id_ed25519_test",
                "trust_unknown_hosts": "true"
            }
        }),
        json!({
            "id": profile_id("sftp_docker_rsa"),
            "name": "Docker SFTP rsa",
            "protocol": "sftp",
            "host": "127.0.0.1",
            "port": 2223,
            "username": "user_key",
            "initialPath": "/",
            "options": {
                "private_key_path": "/var/www/html/FTP_CLIENT_GUI/docs/dev/test-workspace/docker-harness/keys/id_rsa_test",
                "trust_unknown_hosts": "true"
            }
        }),
        json!({
            "id": profile_id("webdav_docker"),
            "name": "Docker WebDAV",
            "protocol": "webdav",
            "host": "http://127.0.0.1:8080/",
            "port": 8080,
            "username": "webdavuser",
            "initialPath": "/",
        }),
        json!({
            "id": profile_id("s3_minio"),
            "name": "Docker MinIO",
            "protocol": "s3",
            "host": "",
            "port": 9000,
            "username": "admin",
            "initialPath": "/",
            "providerId": "minio",
            "options": {
                "bucket": "aeroftp-test",
                "region": "us-east-1",
                "endpoint": "http://127.0.0.1:9000",
                "path_style": "true"
            }
        }),
    ];

    // AeroRsync E2E pair — opt-in via env flag. See header comment for the
    // container setup required for the native delta path to actually trigger.
    if std::env::var("AEROFTP_SEED_AERORSYNC_E2E").is_ok() {
        let key_path = std::env::var("HOME")
            .map(|h| format!("{}/.ssh/id_ed25519", h))
            .map_err(|_| "HOME not set; cannot resolve SSH key path")?;
        seeds.push(json!({
            "id": profile_id("aerorsync_e2e_a"),
            "name": "AeroRsync E2E A",
            "protocol": "sftp",
            "host": "127.0.0.1",
            "port": 2242,
            "username": "testuser",
            "initialPath": "/config/source",
            "options": {
                "private_key_path": key_path,
                "trust_unknown_hosts": "true",
            }
        }));
        seeds.push(json!({
            "id": profile_id("aerorsync_e2e_b"),
            "name": "AeroRsync E2E B",
            "protocol": "sftp",
            "host": "127.0.0.1",
            "port": 2242,
            "username": "testuser",
            "initialPath": "/config/dest",
            "options": {
                "private_key_path": key_path,
                "trust_unknown_hosts": "true",
            }
        }));
    }

    let seed_count = seeds.len();
    for seed in seeds {
        let new_id = seed
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // replace any existing profile with the same id
        profiles.retain(|p| p.get("id").and_then(|v| v.as_str()) != Some(&new_id));

        // store the password under server_{id}
        let password = match seed.get("name").and_then(|v| v.as_str()) {
            Some(name) if name.contains("MinIO") => "password123",
            Some(name) if name.contains("WebDAV") => "password123",
            Some(name) if name.contains("FTP") => "password123",
            Some(name) if name.contains("SFTP") => "", // key auth
            _ => "",
        };
        store.store(&format!("server_{}", new_id), password)?;
        profiles.push(seed);
    }

    let serialized = serde_json::to_string(&profiles)?;
    store.store(PROFILES_KEY, &serialized)?;

    println!("Seeded {} docker test profiles", seed_count);
    println!("Use: aeroftp-cli -P 'Docker FTP' ls /");
    println!("     aeroftp-cli -P 'Docker FTPS' --insecure ls /");
    println!("     aeroftp-cli -P 'Docker SFTP ed25519' ls /");
    println!("     aeroftp-cli -P 'Docker WebDAV' ls /");
    println!("     aeroftp-cli -P 'Docker MinIO' ls /");
    if std::env::var("AEROFTP_SEED_AERORSYNC_E2E").is_ok() {
        println!("     aeroftp-cli -P 'AeroRsync E2E A' ls /");
        println!("     aeroftp-cli -P 'AeroRsync E2E B' ls /");
    }
    Ok(())
}
