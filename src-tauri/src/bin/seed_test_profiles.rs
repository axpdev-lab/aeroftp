// Test helper: seeds docker harness profiles into the encrypted vault.
// Additive-only — does not touch runtime code paths.
// Run: cargo run --bin seed_test_profiles

use ftp_client_gui_lib::credential_store::CredentialStore;
use serde_json::{json, Value};

const PROFILES_KEY: &str = "config_server_profiles";

fn profile_id(slug: &str) -> String {
    format!("srv_test_{}", slug)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    CredentialStore::init()?;
    let store = CredentialStore::from_cache().ok_or("vault not initialized")?;

    let existing = store
        .get(PROFILES_KEY)
        .unwrap_or_else(|_| "[]".to_string());
    let mut profiles: Vec<Value> = serde_json::from_str(&existing).unwrap_or_default();

    let seeds = vec![
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

    for seed in seeds {
        let new_id = seed.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
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

    println!("Seeded {} docker test profiles", 6);
    println!("Use: aeroftp-cli -P 'Docker FTP' ls /");
    println!("     aeroftp-cli -P 'Docker FTPS' --insecure ls /");
    println!("     aeroftp-cli -P 'Docker SFTP ed25519' ls /");
    println!("     aeroftp-cli -P 'Docker WebDAV' ls /");
    println!("     aeroftp-cli -P 'Docker MinIO' ls /");
    Ok(())
}
