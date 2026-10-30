// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! CI probe of the built CLI's actual saved profiles, not its success messages.
use ftp_client_gui_lib::{connection_manifest as contract, profile_loader};
use serde_json::{json, Value};
use std::{path::Path, process::Command};

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SavedProfile {
    id: String,
    name: String,
    protocol: Option<String>,
    provider_id: Option<String>,
    host: String,
    port: u32,
    initial_path: Option<String>,
    options: Option<Value>,
}

fn invoke(cli: &Path, root: &Path, args: &[String]) -> std::process::Output {
    let mut child = Command::new(cli)
        .current_dir(root)
        .args(["--machine", "--no-banner"])
        .args(args)
        .env("AEROFTP_MASTER_PASSWORD", "synthetic profile parity master")
        .env(
            "AEROFTP_PROFILE_PASSWORD",
            "synthetic profile parity export",
        )
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent-profile-parity-dbus",
        )
        .env_remove("RUST_LOG")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run isolated CLI");
    let started = std::time::Instant::now();
    loop {
        if child.try_wait().expect("CLI status").is_some() {
            return child.wait_with_output().expect("CLI output");
        }
        if started.elapsed() > std::time::Duration::from_secs(30) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("isolated CLI exceeded 30 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn options(protocol: &str) -> Value {
    match protocol {
        "s3" | "backblaze" => json!({"bucket":"synthetic-bucket"}),
        "azure" => json!({"container":"synthetic-container"}),
        "kdrive" => json!({"drive_id":"12345"}),
        "cloudinary" => json!({"cloud_name":"synthetic-cloud"}),
        _ => json!({}),
    }
}

/// Run the connection-profile parity check against the CLI given on the
/// command line and report each mismatch.
#[tokio::main]
async fn main() {
    let source = std::env::args_os()
        .nth(1)
        .expect("pass the built aeroftp-cli path");
    let source = std::fs::canonicalize(source).expect("CLI executable");
    let executable = std::env::current_exe().unwrap();
    let root = tempfile::Builder::new()
        .prefix("profile-parity-")
        .tempdir_in(executable.parent().unwrap())
        .unwrap();
    let cli = root.path().join(source.file_name().unwrap());
    if std::fs::hard_link(&source, &cli).is_err() {
        std::fs::copy(&source, &cli).unwrap();
    }
    // A marker is private to this pathname, never beside the shared build.
    std::fs::write(root.path().join("portable.marker"), b"").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}");
    let requests =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Option<String>)>::new()));
    let received = std::sync::Arc::clone(&requests);
    let app = axum::Router::new().fallback(move |request: axum::http::Request<axum::body::Body>| {
        let received = std::sync::Arc::clone(&received);
        async move {
            received.lock().unwrap().push((request.uri().path().into(),
                request.headers().get(axum::http::header::AUTHORIZATION).and_then(|value| value.to_str().ok()).map(str::to_owned)));
            (axum::http::StatusCode::FORBIDDEN, [(axum::http::header::CONTENT_TYPE, "application/xml")],
                "<Error><Code>InvalidAccessKeyId</Code><Message>synthetic auth boundary</Message></Error>")
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut expected = Vec::new();
    let mut cases: Vec<(String, Option<String>)> = contract::manifest()
        .presets
        .iter()
        .map(|preset| (preset.protocol.clone(), Some(preset.id.clone())))
        .collect();
    cases.extend(
        contract::manifest()
            .protocols
            .iter()
            .filter(|spec| spec.creation == "profile")
            .map(|spec| (spec.token.clone(), None)),
    );
    cases.extend(
        contract::manifest()
            .aliases
            .keys()
            .map(|alias| (alias.clone(), None)),
    );
    for (index, (raw_protocol, preset_id)) in cases.into_iter().enumerate() {
        let spec = contract::connection_spec(&raw_protocol).unwrap();
        let name = format!("parity-{index}");
        let mut input = options(&spec.token);
        if let Some(preset) = preset_id.as_deref().and_then(contract::preset) {
            if preset
                .defaults
                .get("endpointTemplate")
                .and_then(Value::as_str)
                .is_some_and(|template| template.contains("{accountId}"))
            {
                input["accountId"] = json!("syntheticaccount");
                input["jurisdiction"] = json!("eu");
            }
        }
        let mut args = vec![
            "profile-add".into(),
            "--name".into(),
            name.clone(),
            "--protocol".into(),
            raw_protocol.clone(),
            "--username".into(),
            "synthetic".into(),
            if index % 2 == 0 {
                "--options-json-file".into()
            } else {
                "--options-json".into()
            },
            if index % 2 == 0 {
                let file = root.path().join("options.json");
                std::fs::write(&file, input.to_string()).unwrap();
                file.to_string_lossy().into_owned()
            } else {
                input.to_string()
            },
        ];
        if let Some(id) = &preset_id {
            args.extend(["--provider-id".into(), id.clone()]);
        }
        let output = invoke(&cli, root.path(), &args);
        if spec.dev_only && !cfg!(debug_assertions) {
            assert!(
                !output.status.success(),
                "DEV-only provider escaped its release gate"
            );
            continue;
        }
        assert!(
            output.status.success(),
            "{raw_protocol}/{preset_id:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let added: Value =
            serde_json::from_slice(&output.stdout).expect("machine profile-add JSON");
        expected.push((
            raw_protocol.clone(),
            name,
            spec,
            preset_id,
            input,
            added["id"].as_str().unwrap().to_owned(),
        ));
    }
    // Explicit modes, global flags, a custom endpoint, and cleartext consent survive saving.
    for (name, protocol, flags, input) in [
        (
            "global-s3",
            "s3",
            vec![
                "--provider-id",
                "wasabi",
                "--bucket",
                "global-bucket",
                "--region",
                "eu-west-1",
                "--insecure",
                "--credential-json",
                r#"{"username":"synthetic-access","password":"synthetic-secret"}"#,
            ],
            json!({"endpoint":endpoint,"allowCleartextEndpoint":true,"pathStyle":true}),
        ),
        (
            "mega-mode",
            "mega",
            vec!["--mega-mode", "megacmd"],
            json!({}),
        ),
        (
            "azure-container",
            "azure",
            vec!["--container", "global-container"],
            json!({}),
        ),
        (
            "sftp-key",
            "sftp",
            vec!["--key", "/synthetic/key"],
            json!({}),
        ),
    ] {
        let mut args: Vec<String> = vec!["profile-add", "--name", name, "--protocol", protocol]
            .into_iter()
            .map(str::to_owned)
            .collect();
        args.extend(flags.into_iter().map(str::to_owned));
        args.extend(["--options-json".into(), input.to_string()]);
        let output = invoke(&cli, root.path(), &args);
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let args = vec![
        "profile-add".into(),
        "--name".into(),
        "webdav-anonymous".into(),
        "--protocol".into(),
        "webdav".into(),
        "--host".into(),
        format!("{endpoint}/dav/"),
        "--port".into(),
        address.port().to_string(),
        "--options-json".into(),
        json!({"anonymous":true,"webdavScheme":"http"}).to_string(),
    ];
    assert!(
        invoke(&cli, root.path(), &args).status.success(),
        "anonymous WebDAV fixture creation failed"
    );
    for name in ["global-s3", "webdav-anonymous"] {
        let args = vec!["--profile".into(), name.into(), "ls".into(), "/".into()];
        let output = invoke(&cli, root.path(), &args);
        assert!(
            !output.status.success() && output.status.code() != Some(5),
            "{name} failed configuration before the synthetic auth boundary: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    {
        let requests = requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|(path, auth)| path.contains("global-bucket")
                    && auth
                        .as_deref()
                        .is_some_and(|auth| auth.contains("Credential=synthetic-access/")
                            && auth.contains("/eu-west-1/s3/aws4_request"))),
            "saved S3 options or credentials did not reach the signed loopback request"
        );
        assert!(
            requests
                .iter()
                .any(|(path, auth)| path.starts_with("/dav") && auth.is_none()),
            "anonymous/scheme options did not reach the WebDAV loopback request"
        );
    }
    server.abort();
    let foreign = root.path().join("foreign.conf");
    std::fs::write(&foreign, "[foreign-linode]\ntype = s3\nprovider = Linode\nendpoint = https://example.invalid\nregion = us-east-1\nbucket = synthetic-import\n\n[foreign-swift]\ntype = swift\nauth = http://127.0.0.1:8080/identity/v3/auth/tokens\nuser = synthetic\n").unwrap();
    let output = invoke(
        &cli,
        root.path(),
        &[
            "import".into(),
            "rclone".into(),
            foreign.to_string_lossy().into_owned(),
            "--apply".into(),
        ],
    );
    assert!(
        output.status.success(),
        "foreign import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved_count = expected.len() + 7;
    for (protocol, extra) in [
        ("mtp", json!({})),
        ("peer", json!({})),
        ("mega", json!({"mega_mode":"invalid"})),
        (
            "s3",
            json!({"bucket":"b","access_token":"must-not-be-saved"}),
        ),
    ] {
        let args = vec![
            "profile-add".into(),
            "--name".into(),
            "must-fail".into(),
            "--protocol".into(),
            protocol.into(),
            "--options-json".into(),
            extra.to_string(),
        ];
        assert!(
            !invoke(&cli, root.path(), &args).status.success(),
            "invalid/managed profile accepted: {protocol}"
        );
    }
    let export = root.path().join("profiles.aeroftp");
    let output = invoke(
        &cli,
        root.path(),
        &[
            "profile-export".into(),
            "--output".into(),
            export.to_string_lossy().into_owned(),
        ],
    );
    assert!(
        output.status.success(),
        "export failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let preview = ftp_client_gui_lib::import_server_profiles_with_mode(
        export.to_string_lossy().into_owned(),
        "synthetic profile parity export".into(),
        ftp_client_gui_lib::profile_import_plan::ExistingProfileMode::Skip,
        &[],
        None,
        true,
    )
    .await
    .expect("read actual encrypted export without restoring credentials");
    let saved: Vec<SavedProfile> =
        serde_json::from_value(serde_json::Value::Array(preview.servers))
            .expect("saved profile configuration");
    assert_eq!(
        saved.len(),
        saved_count,
        "failed creation left a partial profile"
    );
    let foreign_linode = saved
        .iter()
        .find(|p| p.name == "foreign-linode")
        .expect("foreign S3 profile preserved");
    assert_eq!(
        foreign_linode.provider_id.as_deref(),
        Some("linode-object-storage")
    );
    assert_eq!(
        foreign_linode.options.as_ref().unwrap()["endpoint"],
        "https://example.invalid"
    );
    let foreign_swift = saved
        .iter()
        .find(|p| p.name == "foreign-swift")
        .expect("foreign Swift profile preserved");
    assert_eq!(
        foreign_swift.host,
        "http://127.0.0.1:8080/identity/v3/auth/tokens"
    );
    assert_eq!(foreign_swift.port, 8080);
    for (raw_protocol, name, spec, preset_id, input, id) in expected {
        let profile = saved
            .iter()
            .find(|p| p.id == id)
            .expect("created id persisted");
        assert_eq!(profile.name, name);
        assert_eq!(profile.protocol.as_deref(), Some(spec.token.as_str()));
        let (_, effective_id) =
            contract::canonical_protocol_and_provider(&raw_protocol, preset_id.as_deref());
        let effective_id = effective_id.or(spec.native_provider_id.as_deref());
        let preset = effective_id
            .and_then(contract::preset)
            .filter(|preset| preset.protocol == spec.token);
        let expected_id = effective_id.filter(|_| !preset.is_some_and(|p| p.generic));
        assert_eq!(
            profile.provider_id.as_deref(),
            expected_id,
            "{name} preset identity"
        );
        let expected_host = preset
            .and_then(|p| p.defaults.get("server"))
            .and_then(Value::as_str)
            .or(spec.default_host.as_deref())
            .unwrap_or("")
            .replace("{username}", "synthetic");
        assert_eq!(profile.host, expected_host, "{name} host");
        let expected_path = preset
            .and_then(|p| p.defaults.get("basePath"))
            .and_then(Value::as_str)
            .filter(|path| !path.is_empty())
            .unwrap_or("/")
            .replace("{username}", "synthetic");
        assert_eq!(
            profile.initial_path.as_deref(),
            Some(expected_path.as_str()),
            "{name} initial path"
        );
        let expected_port = preset
            .and_then(|p| p.defaults.get("port"))
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(spec.default_port));
        assert_eq!(u64::from(profile.port), expected_port, "{name} port");
        let value = serde_json::to_value(profile).unwrap();
        let kind = contract::provider_type(&spec.token).unwrap();
        let (extra, _) = profile_loader::profile_connection_extra(&value, kind, &profile.host);
        if let Some(preset) = preset {
            for (key, expected) in &preset.defaults {
                if matches!(
                    key.as_str(),
                    "server" | "port" | "basePath" | "endpointTemplate"
                ) {
                    continue;
                }
                let key = profile_loader::canonicalize_profile_option_key(key);
                let expected = expected
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| expected.to_string());
                assert_eq!(extra.get(&key), Some(&expected), "{name}/{key}");
            }
        }
        for key in spec.required_options.iter() {
            let canonical = profile_loader::canonicalize_profile_option_key(key);
            assert!(
                extra.get(&canonical).is_some_and(|value| !value.is_empty()),
                "{name} missing {key}: {input}"
            );
        }
    }
    for (name, key, expected) in [
        ("global-s3", "bucket", "global-bucket"),
        ("global-s3", "region", "eu-west-1"),
        ("global-s3", "verify_cert", "false"),
        ("mega-mode", "mega_mode", "megacmd"),
        ("azure-container", "container", "global-container"),
        ("sftp-key", "private_key_path", "/synthetic/key"),
    ] {
        let profile = saved.iter().find(|p| p.name == name).unwrap();
        let value = serde_json::to_value(profile).unwrap();
        let kind = contract::provider_type(profile.protocol.as_deref().unwrap()).unwrap();
        let (extra, _) = profile_loader::profile_connection_extra(&value, kind, &profile.host);
        assert_eq!(
            extra.get(key).map(String::as_str),
            Some(expected),
            "{name}/{key}"
        );
    }
    println!("Saved and reloaded {saved_count} connection fixtures; defaults, aliases, modes, global options, and managed/secret rejection pass.");
}
