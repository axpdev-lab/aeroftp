use super::*;
use std::sync::{Arc, Mutex};

// Fork before initializing either vault or user partitions. The child binary
// and portable marker have a private pathname, even in a full parallel suite.
#[test]
fn headless_flows_round_trip() {
    if std::env::var_os("AEROFTP_AUTH_FIXTURE_CHILD").is_none() {
        let executable = std::env::current_exe().unwrap();
        let root = tempfile::Builder::new()
            .prefix("auth-fixture-")
            .tempdir_in(executable.parent().unwrap())
            .unwrap();
        let child = root.path().join("auth-fixture-test");
        std::fs::hard_link(&executable, &child)
            .or_else(|_| std::fs::copy(&executable, &child).map(|_| ()))
            .unwrap();
        std::fs::write(root.path().join("portable.marker"), b"").unwrap();
        let empty_path = root.path().join("no-external-tools");
        std::fs::create_dir(&empty_path).unwrap();
        let output = std::process::Command::new(child)
            .args([
                "--exact",
                "profile_auth::tests::headless_flows_round_trip",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("AEROFTP_AUTH_FIXTURE_CHILD", "1")
            .env("PATH", &empty_path)
            .env("AEROFTP_MASTER_PASSWORD", "synthetic auth fixture master")
            .env_remove("AEROFTP_CRYPT_OVERLAY_PASSWORD")
            .env_remove("AEROFTP_CRYPT_OVERLAY_SALT")
            .env_remove("AEROFTP_CRYPT_OVERLAY_KEYFILE")
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-auth-fixture",
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _session_guard = crate::user_partitions::test_lock();
    let executable = std::env::current_exe().unwrap();
    assert!(executable
        .parent()
        .unwrap()
        .join("portable.marker")
        .is_file());
    assert_eq!(
        std::env::var("AEROFTP_MASTER_PASSWORD").unwrap(),
        "synthetic auth fixture master"
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(round_trip());
}

async fn round_trip() {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
        response::IntoResponse,
    };
    assert_eq!(CredentialStore::init().unwrap(), "OK");
    let store = CredentialStore::from_cache().unwrap();
    // Exercise the GUI/AeroAgent factory in this isolated, synthetic vault.
    // Missing authorization must reach profile-bound auth, without the old
    // blanket refusal or an interactive browser flow, for all seven routes.
    for protocol in [
        "googledrive",
        "dropbox",
        "onedrive",
        "box",
        "pcloud",
        "zohoworkdrive",
        "fourshared",
    ] {
        let id = format!("temp-factory-{protocol}");
        store
            .store(
                "config_server_profiles",
                &json!([{
                    "id": id, "name": "synthetic missing authorization", "protocol": protocol,
                    "host": "", "port": 0, "username": ""
                }])
                .to_string(),
            )
            .unwrap();
        if let Some(user) = users::cli_get_active_user(&store).unwrap() {
            let profiles: Vec<serde_json::Value> =
                serde_json::from_str(&store.get("config_server_profiles").unwrap()).unwrap();
            users::cli_replace_server_profiles_for_user(&store, user.id, &profiles).unwrap();
        }
        let server = crate::ai_tools::load_saved_servers().unwrap().remove(0);
        match crate::ai_tools::create_temp_provider(&server).await {
            Ok(_) => panic!("{protocol}: a profile without authorization must not connect"),
            Err(error) => {
                assert!(
                    !error.contains("requires browser authentication"),
                    "{protocol}: {error}"
                );
                assert!(
                    !error.contains("Unsupported protocol"),
                    "{protocol}: {error}"
                );
                assert!(
                    error.contains("Connection to") || error.contains("authorization missing"),
                    "{protocol}: {error}"
                );
            }
        }
    }
    store.delete("config_server_profiles").unwrap();
    let scope = AuthScope {
        store: &store,
        user_id: None,
    };
    let captured = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
    let received = Arc::clone(&captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app=axum::Router::new().fallback(move |request:Request<Body>| {
        let received=Arc::clone(&received);
        async move {
            let path=request.uri().path().to_owned();
            let auth=request.headers().get("authorization").and_then(|v|v.to_str().ok()).unwrap_or("").to_owned();
            let body=String::from_utf8(to_bytes(request.into_body(),65536).await.unwrap().to_vec()).unwrap();
            received.lock().unwrap().push((path.clone(),auth,body.clone()));
            if path=="/initiate" {return (StatusCode::OK,"oauth_token=request-token&oauth_token_secret=request-secret").into_response();}
            if path=="/oauth1-token" {return (StatusCode::OK,"oauth_token=access-token&oauth_token_secret=access-secret").into_response();}
            let payload=match path.as_str() {
                "/device"=>json!({"device_code":"synthetic-device-secret","user_code":"ABCD-EFGH","verification_uri":"https://example.invalid/verify","expires_in":600,"interval":5}),
                "/device-token"=>json!({"access_token":"synthetic-device-token","token_type":"bearer"}),
                "/auth/register"=>json!({"client_id":"twake-client","client_secret":"twake-client-secret","registration_access_token":"registration-secret"}),
                "/auth/access_token"=>json!({"access_token":"twake-access","refresh_token":"twake-refresh"}),
                "/oauth2-token"=>json!({"access_token":if body.contains("grant_type=refresh_token"){"oauth-refreshed"}else{"oauth-access"},"refresh_token":"oauth-refresh","expires_in":3600,"token_type":"Bearer"}),
                _ if path.ends_with("/access_tokens")=>json!({"token":"synthetic-installation-token","expires_at":(chrono::Utc::now()+chrono::Duration::hours(1)).to_rfc3339()}),
                _=>return (StatusCode::NOT_FOUND,"unexpected auth request").into_response(),
            };
            (StatusCode::OK,axum::Json(payload)).into_response()
        }
    });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoints = AuthEndpoints {
        device: format!("{endpoint}/device"),
        device_token: format!("{endpoint}/device-token"),
        installation: format!("{endpoint}/installations"),
        oauth1_begin: format!("{endpoint}/initiate"),
        oauth1_finish: format!("{endpoint}/oauth1-token"),
        oauth2_token: Some(format!("{endpoint}/oauth2-token")),
    };
    let mut gaps = Vec::new();
    let google =
        json!({"id":"fixture-google","name":"synthetic","protocol":"googledrive","host":""});
    execute_at(&scope,&google,&json!({"action":"configure","client_id":"fixture-client","client_secret":"fixture-client-secret"}),&endpoints).await.unwrap();
    let public = execute_at(&scope, &google, &json!({"action":"begin"}), &endpoints)
        .await
        .unwrap();
    assert!(!public.to_string().contains("fixture-client-secret"));
    let url = url::Url::parse(field(&public, "authorization_url")).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    execute_at(
        &scope,
        &google,
        &json!({"action":"finish","code":"fixture-code","state":query["state"]}),
        &endpoints,
    )
    .await
    .unwrap();
    let mut tokens: oauth2::StoredTokens =
        serde_json::from_str(&store.get("oauth_google_fixture-google").unwrap()).unwrap();
    assert_eq!(tokens.access_token, "oauth-access");
    assert!(store.get("oauth_google_other-profile").is_err());
    let (_, _, body) = captured
        .lock()
        .unwrap()
        .iter()
        .find(|(p, _, _)| p == "/oauth2-token")
        .unwrap()
        .clone();
    let fields: std::collections::HashMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect();
    use base64::Engine;
    use sha2::{Digest, Sha256};
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(fields["code_verifier"].as_bytes())),
        query["code_challenge"]
    );
    tokens.expires_at = Some(0);
    scope
        .write(
            "oauth_google_fixture-google",
            &serde_json::to_string(&tokens).unwrap(),
        )
        .unwrap();
    let mut refresh = oauth_config(
        ProviderType::GoogleDrive,
        "fixture-google",
        &("fixture-client".into(), "fixture-client-secret".into()),
        "",
        19899,
    )
    .unwrap();
    refresh.token_url = format!("{endpoint}/oauth2-token");
    assert_eq!(
        OAuth2Manager::new()
            .get_valid_token(&refresh)
            .await
            .unwrap()
            .expose_secret(),
        "oauth-refreshed"
    );
    assert_eq!(readiness(&scope, &google).unwrap(), "valid");

    let github =
        json!({"id":"fixture-github","name":"synthetic","protocol":"github","host":"owner/repo"});
    execute_at(
        &scope,
        &github,
        &json!({"action":"configure","mode":"authorize"}),
        &endpoints,
    )
    .await
    .unwrap();
    let public = execute_at(&scope, &github, &json!({"action":"begin"}), &endpoints)
        .await
        .unwrap();
    assert!(!public.to_string().contains("synthetic-device-secret"));
    execute_at(&scope, &github, &json!({"action":"finish"}), &endpoints)
        .await
        .unwrap();
    assert_eq!(
        store.get("github_oauth_fixture-github").unwrap(),
        "synthetic-device-token"
    );
    assert!(provider_at(&scope, &github, &endpoints)
        .await
        .unwrap()
        .is_some());
    let accounts = store.list_accounts().unwrap().into_iter().collect();
    if crate::profile_auth_state::derive_profile_auth_state(
        &store,
        &accounts,
        "fixture-github",
        "github",
        crate::profile_auth_state::profile_auth_source(&github),
    ) != "valid"
    {
        gaps.push("GitHub Device Flow readiness ignores its token slot");
    }

    // Public synthetic key, used only against the loopback mock App. Compiling
    // it into the test removes any runtime OpenSSL or fixture-file dependency.
    let pem = include_str!("../tests/fixtures/profile-auth/github-app-test-key.pem");
    execute_at(&scope,&github,&json!({"action":"configure","mode":"app","app_id":"123","installation_id":"456","pem":pem}),&endpoints).await.unwrap();
    assert!(provider_at(&scope, &github, &endpoints)
        .await
        .unwrap()
        .is_some());
    assert!(provider_at(&scope, &github, &endpoints)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        captured
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, auth, _)| p == "/installations/456/access_tokens"
                && auth.starts_with("Bearer eyJ"))
            .count(),
        2
    );
    if scope
        .read("profile_auth_deadline_fixture-github")
        .unwrap()
        .is_none()
    {
        gaps.push("GitHub installation expiry is unavailable to the MCP pool");
    }

    let deadline = connection_deadline(&scope, &github).unwrap().unwrap();
    assert!(crate::mcp::pool::authentication_current(
        Some(deadline),
        deadline - 1
    ));
    assert!(!crate::mcp::pool::authentication_current(
        Some(deadline),
        deadline
    ));
    let active = users::cli_get_active_user(&store).unwrap().unwrap();
    let owner = users::cli_create_user(&store, "Scoped OAuth owner", None, None, None).unwrap();
    let manager = OAuth2Manager::new().with_credential_user(Some(owner.id));
    manager
        .clone()
        .store_tokens(OAuthProvider::Google, "scoped-refresh", &tokens)
        .unwrap();
    let db = users::open_or_init_cli().unwrap();
    let count = |uid: i64| {
        db.query_row(
            "SELECT COUNT(*) FROM user_credentials WHERE user_id=?1 AND credential_id=?2",
            rusqlite::params![uid, "oauth_google_scoped-refresh"],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(count(owner.id), 1);
    assert_eq!(count(active.id), 0);
    let four =
        json!({"id":"fixture-fourshared","name":"synthetic","protocol":"fourshared","host":""});
    execute_at(
        &scope,
        &four,
        &json!({"action":"configure","client_id":"consumer","client_secret":"consumer-secret"}),
        &endpoints,
    )
    .await
    .unwrap();
    execute_at(&scope, &four, &json!({"action":"begin"}), &endpoints)
        .await
        .unwrap();
    execute_at(
        &scope,
        &four,
        &json!({"action":"finish","oauth_token":"request-token","verifier":"fixture-verifier"}),
        &endpoints,
    )
    .await
    .unwrap();
    assert_eq!(
        store.get("oauth_fourshared_fixture-fourshared").unwrap(),
        "access-token:access-secret"
    );
    assert!(provider_at(&scope, &four, &endpoints)
        .await
        .unwrap()
        .is_some());
    assert!(captured
        .lock()
        .unwrap()
        .iter()
        .any(|(p, auth, _)| p == "/oauth1-token"
            && auth.contains("oauth_signature=")
            && auth.contains("oauth_verifier=")));

    let twake = json!({"id":"fixture-twake","name":"synthetic","protocol":"twake","host":endpoint});
    let public = execute_at(&scope, &twake, &json!({"action":"begin"}), &endpoints)
        .await
        .unwrap();
    let url = url::Url::parse(field(&public, "authorization_url")).unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .to_string();
    execute_at(
        &scope,
        &twake,
        &json!({"action":"finish","code":"twake-code","state":state}),
        &endpoints,
    )
    .await
    .unwrap();
    let credential = store.get("server_fixture-twake").unwrap();
    let config = ProviderConfig {
        name: "synthetic".into(),
        provider_type: ProviderType::Twake,
        host: field(&twake, "host").into(),
        port: None,
        username: None,
        password: Some(credential),
        initial_path: None,
        extra: Default::default(),
    };
    for _ in 0..2 {
        let mut provider = ProviderFactory::create(&config).unwrap();
        provider.connect().await.unwrap();
        provider.disconnect().await.unwrap();
    }
    assert!(
        captured
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _, body)| p == "/auth/access_token"
                && body.contains("grant_type=refresh_token"))
            .count()
            >= 2
    );
    scope.write("profile_auth_config_corrupt", "null").unwrap();
    if scope.config("corrupt").is_ok() {
        gaps.push("Corrupt non-object auth configuration is accepted");
    }
    durable_profile_auth_lifecycle(&store, &github, active.id, owner.id).await;
    #[cfg(feature = "proton-native")]
    native_session_ownership(&store, active.id, owner.id).await;
    crate::crypt_overlay_provider::tests::captured_overlay_owner_survives_account_switch(
        &store, active.id, owner.id,
    )
    .await;
    server.abort();
    assert!(gaps.is_empty(), "Auth lifecycle gaps: {gaps:?}");
}

/// Import `file` with `list` as the saved profiles of `user` (the active one when
/// `None`), or preview it against `list` on a `dry_run`.
async fn import_over(
    store: &CredentialStore,
    file: String,
    password: String,
    mode: crate::profile_import_plan::ExistingProfileMode,
    list: &[serde_json::Value],
    user: Option<i64>,
    dry_run: bool,
) -> Result<crate::profile_import_plan::ProfileImportReport, String> {
    if dry_run {
        return crate::preview_server_profiles(&file, &password, mode, list);
    }
    let user = match user {
        Some(user) => user,
        None => users::cli_get_active_user(store)?.unwrap().id,
    };
    users::cli_update_server_profiles_for_user(store, user, |stored| {
        *stored = list.to_vec();
        true
    })?;
    crate::import_server_profiles_with_mode(file, password, mode, user).await
}

/// Durable profile-auth secrets through copy, export, purge and every import
/// mode, against the real vault of the forked fixture.
async fn durable_profile_auth_lifecycle(
    store: &CredentialStore,
    github: &Value,
    active_id: i64,
    other_id: i64,
) {
    let source = field(github, "id");
    let target = "fixture-github-copy";
    // A pending sign-in and an installation token's cache deadline are local
    // in-flight state. They must be deleted, never copied or exported.
    for prefix in TRANSIENT_KEY_PREFIXES {
        users::store_active_credential_typed_dual(
            store,
            &format!("{prefix}_{source}"),
            "oauth",
            "123456789",
        )
        .unwrap();
    }
    let copied = crate::copy_profile_secrets(source.into(), target.into(), Some("github".into()))
        .await
        .unwrap();
    for prefix in DURABLE_KEY_PREFIXES {
        assert_eq!(copied.get(prefix), Some(&true), "{prefix}");
        let source_value = users::resolve_active_credential(store, &format!("{prefix}_{source}"))
            .unwrap()
            .unwrap();
        let copied_value = users::resolve_active_credential(store, &format!("{prefix}_{target}"))
            .unwrap()
            .unwrap();
        assert_eq!(*copied_value, *source_value, "{prefix}");
    }
    for prefix in TRANSIENT_KEY_PREFIXES {
        assert!(!copied.contains_key(prefix));
        assert!(store.get(&format!("{prefix}_{target}")).is_err());
    }

    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("headless.aeroftp");
    let mut export_profile = github.clone();
    export_profile["port"] = json!(443);
    let metadata = crate::export_server_profiles_core(
        json!([export_profile.clone()]).to_string(),
        "fixture-export-password".into(),
        true,
        file.to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    assert!(metadata.has_credentials);
    let (_, secrets, _) =
        crate::profile_export::import_profiles(&file, "fixture-export-password").unwrap();
    let auth = secrets[source].profile_auth.as_ref().unwrap();
    assert!(auth.entries().iter().all(|(_, value)| value.is_some()));
    let encoded = serde_json::to_string(&secrets).unwrap();
    for prefix in TRANSIENT_KEY_PREFIXES {
        assert!(!encoded.contains(prefix));
    }
    crate::purge_profile_secrets(source.into(), Some("github".into()))
        .await
        .unwrap();
    for prefix in DURABLE_KEY_PREFIXES
        .into_iter()
        .chain(TRANSIENT_KEY_PREFIXES)
    {
        assert!(
            users::resolve_active_credential(store, &format!("{prefix}_{source}"))
                .unwrap()
                .is_none(),
            "{prefix}"
        );
    }
    // Nothing of this id is listed any more: the import restores every secret.
    import_over(
        store,
        file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        crate::profile_import_plan::ExistingProfileMode::Skip,
        &[],
        None,
        false,
    )
    .await
    .unwrap();
    for (prefix, expected) in auth.entries() {
        let actual = users::resolve_active_credential(store, &format!("{prefix}_{source}"))
            .unwrap()
            .unwrap();
        assert_eq!(actual.as_str(), expected.unwrap(), "{prefix}");
    }
    for prefix in TRANSIENT_KEY_PREFIXES {
        assert!(store.get(&format!("{prefix}_{source}")).is_err());
    }

    // Re-importing a profile the user already has: skip keeps the local
    // secrets, overwrite replaces them under the same id, duplicate writes them
    // to a new id only, and a dry run writes nothing.
    use crate::profile_import_plan::ExistingProfileMode;
    let current = vec![export_profile.clone()];
    let entries = auth.entries();
    let (stale_prefix, expected) = (entries[0].0, entries[0].1.unwrap());
    let stale_key = format!("{stale_prefix}_{source}");
    let read = |key: &str| users::resolve_active_credential(store, key).unwrap();
    // The import is pinned to the active user, as the GUI and the CLI pin it.
    let active = users::cli_get_active_user(store)
        .unwrap()
        .map(|user| user.id);
    assert!(active.is_some());
    let import_with = |mode, dry_run| {
        import_over(
            store,
            file.to_string_lossy().into_owned(),
            "fixture-export-password".into(),
            mode,
            &current,
            active,
            dry_run,
        )
    };
    users::store_active_credential_typed_dual(store, &stale_key, "oauth", "stale-local").unwrap();
    let skip = import_with(ExistingProfileMode::Skip, false).await.unwrap();
    assert!(skip.servers.is_empty());
    assert_eq!(skip.skipped, [field(&export_profile, "name")]);
    assert_eq!(read(&stale_key).unwrap().as_str(), "stale-local");
    let overwrite = import_with(ExistingProfileMode::Overwrite, false)
        .await
        .unwrap();
    assert_eq!(overwrite.servers[0]["importOutcome"], "overwritten");
    assert_eq!(overwrite.servers[0]["id"], source);
    assert_eq!(read(&stale_key).unwrap().as_str(), expected);
    users::store_active_credential_typed_dual(store, &stale_key, "oauth", "stale-local").unwrap();
    let preview = import_with(ExistingProfileMode::Duplicate, true)
        .await
        .unwrap();
    let preview_id = field(&preview.servers[0], "id");
    assert_ne!(preview_id, source);
    assert!(read(&format!("{stale_prefix}_{preview_id}")).is_none());
    let duplicate = import_with(ExistingProfileMode::Duplicate, false)
        .await
        .unwrap();
    assert_eq!(duplicate.servers[0]["importOutcome"], "duplicated");
    let copy_id = field(&duplicate.servers[0], "id");
    assert_ne!(copy_id, source);
    for (prefix, value) in &entries {
        let copied = read(&format!("{prefix}_{copy_id}")).unwrap();
        assert_eq!(copied.as_str(), value.unwrap(), "{prefix}");
    }
    assert_eq!(read(&stale_key).unwrap().as_str(), "stale-local");
    users::store_active_credential_typed_dual(store, &stale_key, "oauth", expected).unwrap();
    crate::purge_profile_secrets(copy_id.into(), Some("github".into()))
        .await
        .unwrap();
    assert!(overwrite.jotta_chains.is_empty());

    // A Jottacloud profile that brings its rotating refresh chain is named in
    // the report, for the GUI and CLI warning; a dry run names it too.
    let jotta = json!({"id":"fixture-jotta","name":"synthetic jotta","protocol":"jottacloud",
        "host":"","port":443});
    users::store_active_credential_typed_dual(
        store,
        "jottacloud_refresh_fixture-jotta",
        "jottacloud_refresh",
        "synthetic-refresh-chain",
    )
    .unwrap();
    users::store_active_credential_dual(
        store,
        "aerocrypt_overlay_pw_fixture-jotta",
        "synthetic-overlay-password",
    )
    .unwrap();
    let jotta_file = directory.path().join("jotta.aeroftp");
    crate::export_server_profiles_core(
        json!([jotta.clone()]).to_string(),
        "fixture-export-password".into(),
        true,
        jotta_file.to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    // Another profile's chain already sits in the legacy singleton: an import
    // or a duplicate must not replace it.
    store
        .store("jottacloud_refresh", "other-profile-chain")
        .unwrap();
    for dry_run in [true, false] {
        let jotta_report = import_over(
            store,
            jotta_file.to_string_lossy().into_owned(),
            "fixture-export-password".into(),
            ExistingProfileMode::Duplicate,
            std::slice::from_ref(&jotta),
            active,
            dry_run,
        )
        .await
        .unwrap();
        assert_eq!(jotta_report.servers[0]["importOutcome"], "duplicated");
        assert_eq!(jotta_report.jotta_chains.len(), 1);
        assert_eq!(jotta_report.jotta_chains[0].name, "synthetic jotta");
        let copy = field(&jotta_report.servers[0], "id");
        assert_eq!(jotta_report.jotta_chains[0].id, copy);
        let chain = read(&format!("jottacloud_refresh_{copy}"));
        if dry_run {
            assert!(chain.is_none());
        } else {
            // A real import wrote the chain to the copy: the warning is due.
            assert_eq!(chain.unwrap().as_str(), "synthetic-refresh-chain");
            // A restored secret shows in the profile to store, so a Crypt
            // profile added by an import reports its stored overlay password.
            assert_eq!(jotta_report.servers[0]["hasStoredAeroCryptPassword"], true);
            // And in the list as stored, not only in the report.
            let stored_copy = jotta_report
                .stored
                .iter()
                .find(|p| p["id"].as_str() == Some(copy))
                .unwrap();
            assert_eq!(stored_copy["hasStoredAeroCryptPassword"], true);
            assert_eq!(
                store.get("jottacloud_refresh").unwrap(),
                "other-profile-chain"
            );
            crate::purge_profile_secrets(copy.into(), Some("jottacloud".into()))
                .await
                .unwrap();
        }
    }
    // A backup that points the profile at another server is refused, and nothing
    // is written or removed: the stored password stays, and never reaches the
    // new server. The same backup over the same target goes through.
    users::store_active_credential_dual(store, &format!("server_{source}"), "stored-password")
        .unwrap();
    users::store_active_credential_typed_dual(store, &stale_key, "oauth", "stale-local").unwrap();
    let mut elsewhere = export_profile.clone();
    elsewhere["host"] = json!("other-owner/other-repo");
    let refused = import_over(
        store,
        file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Overwrite,
        std::slice::from_ref(&elsewhere),
        active,
        false,
    )
    .await
    .unwrap();
    assert!(refused.servers.is_empty());
    // The same with only an option of the backup weakening the connection: the
    // options reach the comparison.
    let mut weaker = export_profile.clone();
    weaker["options"] = json!({"verifyCert": false});
    let weaker_file = directory.path().join("weaker.aeroftp");
    crate::export_server_profiles_core(
        json!([weaker]).to_string(),
        "fixture-export-password".into(),
        false,
        weaker_file.to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    let weaker = import_over(
        store,
        weaker_file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Overwrite,
        &current,
        active,
        false,
    )
    .await
    .unwrap();
    assert!(weaker.servers.is_empty());
    assert_eq!(weaker.target_changed, [field(&export_profile, "name")]);
    assert_eq!(refused.target_changed, [field(&export_profile, "name")]);
    assert_eq!(refused.skipped, [field(&export_profile, "name")]);
    assert_eq!(read(&stale_key).unwrap().as_str(), "stale-local");
    assert_eq!(
        read(&format!("server_{source}")).unwrap().as_str(),
        "stored-password"
    );
    let kept = import_with(ExistingProfileMode::Overwrite, false)
        .await
        .unwrap();
    assert!(kept.target_changed.is_empty());
    assert_eq!(read(&stale_key).unwrap().as_str(), expected);
    assert_eq!(
        read(&format!("server_{source}")).unwrap().as_str(),
        "stored-password"
    );

    // An import for another account than the active one is refused before it
    // writes anything: restored secrets also reach the shared vault key.
    let list_of = |user: i64| {
        users::cli_update_server_profiles_for_user(store, user, |_| false)
            .unwrap()
            .0
    };
    let other_list = list_of(other_id);
    let shared_before = store.get(&stale_key).ok();
    let for_other = crate::import_server_profiles_with_mode(
        file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Duplicate,
        other_id,
    )
    .await
    .unwrap_err();
    assert!(for_other.starts_with("ACCOUNT_CHANGED"), "{for_other}");
    assert_eq!(list_of(other_id), other_list);
    assert_eq!(store.get(&stale_key).ok(), shared_before);

    // The real vault and partition: a rollback puts the old value back in both,
    // removes a key that did not exist, and leaves one changed since.
    {
        use crate::profile_import_plan::{
            apply_secret_writes, roll_back_secret_writes, SecretWrite,
        };
        let slots = crate::VaultSecretSlots {
            store,
            user: active_id,
        };
        store.store("server_rollback-held", "vault-before").unwrap();
        users::store_active_credential_dual(store, "server_rollback-held", "before").unwrap();
        let applied = apply_secret_writes(
            &slots,
            vec![
                SecretWrite::new("server_rollback-held".into(), None, "file-held"),
                SecretWrite::new("server_rollback-new".into(), None, "file-new"),
                SecretWrite::new("server_rollback-raced".into(), None, "file-raced"),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read("server_rollback-held").unwrap().as_str(), "file-held");
        users::store_active_credential_dual(store, "server_rollback-raced", "newer").unwrap();
        assert!(roll_back_secret_writes(&slots, applied).is_empty());
        assert_eq!(read("server_rollback-held").unwrap().as_str(), "before");
        assert_eq!(store.get("server_rollback-held").unwrap(), "before");
        assert!(read("server_rollback-new").is_none());
        assert!(store.get("server_rollback-new").is_err());
        assert_eq!(read("server_rollback-raced").unwrap().as_str(), "newer");

        // A token private to an account lives in its partition alone, and a key
        // a partition holds outside the prefix classes is updated there too: its
        // reader prefers that row.
        let other_slots = crate::VaultSecretSlots {
            store,
            user: other_id,
        };
        let partition = |key: &str| {
            users::get_user_credential_for(
                &users::open_or_init_cli().unwrap(),
                &store.derive_user_partition_wrapping_key(),
                other_id,
                key,
            )
            .unwrap()
            .map(|value| value.to_string())
        };
        users::store_credential_for_user(
            store,
            other_id,
            "filen_api_key_rollback",
            "filen_api_key",
            "partition-before",
        )
        .unwrap();
        let applied = apply_secret_writes(
            &other_slots,
            vec![
                SecretWrite::new(
                    "jottacloud_refresh_rollback".into(),
                    Some("jottacloud_refresh"),
                    "file-chain",
                ),
                SecretWrite::new("filen_api_key_rollback".into(), None, "file-key"),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            partition("jottacloud_refresh_rollback").unwrap(),
            "file-chain"
        );
        assert!(store.get("jottacloud_refresh_rollback").is_err());
        assert_eq!(partition("filen_api_key_rollback").unwrap(), "file-key");
        assert!(roll_back_secret_writes(&other_slots, applied).is_empty());
        assert!(partition("jottacloud_refresh_rollback").is_none());
        assert_eq!(
            partition("filen_api_key_rollback").unwrap(),
            "partition-before"
        );
        assert!(store.get("filen_api_key_rollback").is_err());

        // A write that reaches the vault and then fails in the partition goes back
        // in the vault too, and the error names the key.
        store
            .store("server_rollback-partial", "vault-before")
            .unwrap();
        let conn = users::open_or_init_cli().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_partial BEFORE INSERT ON user_credentials
             WHEN NEW.credential_id = 'server_rollback-partial'
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
        let refused = apply_secret_writes(
            &slots,
            vec![SecretWrite::new(
                "server_rollback-partial".into(),
                None,
                "file-partial",
            )],
        )
        .map(|_| ())
        .unwrap_err();
        conn.execute_batch("DROP TRIGGER refuse_partial;").unwrap();
        assert!(refused.contains("server_rollback-partial"), "{refused}");
        assert!(!refused.contains("file-partial"), "{refused}");
        assert_eq!(
            store.get("server_rollback-partial").unwrap(),
            "vault-before"
        );

        // Relocation takes the list lock too.
        let held = users::ProfileListLock::acquire(&users::open_or_init_cli().unwrap())
            .unwrap()
            .unwrap();
        let busy = users::cli_relocate_server_profile(
            store,
            active_id,
            other_id,
            source,
            "rollback-copy",
            None,
            false,
        );
        assert_eq!(busy.err().as_deref(), Some(users::PROFILE_LIST_BUSY));
        drop(held);
    }

    // Another writer saving between the secrets and the list: the real write
    // refuses, the secrets go back, and the import plans again on the new list,
    // keeping that save. Saving again on the second plan refuses the import, with
    // the secrets back.
    let raced_write = move || {
        let store = CredentialStore::from_cache().unwrap();
        let (_, revision) = users::cli_read_active_profiles_at_revision(&store, active_id).unwrap();
        users::cli_update_profiles_at_revision(&store, active_id, &revision, |list| {
            list.retain(|p| p["id"] != "raced");
            list.push(
                json!({"id": "raced", "name": "raced", "host": "h", "port": 1, "username": "u"}),
            );
        })
        .unwrap();
    };
    users::store_active_credential_typed_dual(store, &stale_key, "oauth", "stale-local").unwrap();
    let mut left = 1;
    crate::IMPORT_BEFORE_COMMIT.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            if left > 0 {
                left -= 1;
                raced_write();
            }
        }))
    });
    let replanned = import_with(ExistingProfileMode::Overwrite, false)
        .await
        .unwrap();
    assert_eq!(replanned.servers[0]["importOutcome"], "overwritten");
    assert!(replanned.stored.iter().any(|p| p["id"] == "raced"));
    assert_eq!(read(&stale_key).unwrap().as_str(), expected);
    users::store_active_credential_typed_dual(store, &stale_key, "oauth", "stale-local").unwrap();
    crate::IMPORT_BEFORE_COMMIT.with_borrow_mut(|hook| *hook = Some(Box::new(raced_write)));
    let refused_twice = import_with(ExistingProfileMode::Overwrite, false)
        .await
        .unwrap_err();
    crate::IMPORT_BEFORE_COMMIT.with_borrow_mut(|hook| *hook = None);
    assert_eq!(refused_twice, users::PROFILES_CHANGED);
    assert_eq!(read(&stale_key).unwrap().as_str(), "stale-local");

    // The operation of the rclone import, the same way: a save in between costs one
    // more plan and is kept, a second one refuses with no secret and no profile left.
    let bridge_plan = |id: &'static str| {
        move |_: &[serde_json::Value]| {
            Ok(crate::ImportedProfilesPlan {
                writes: vec![crate::profile_import_plan::SecretWrite::new(
                    format!("server_{id}"),
                    None,
                    "bridge-secret",
                )],
                additions: vec![
                    json!({"id": id, "name": id, "host": "b", "port": 2, "username": "u"}),
                ],
                result: (),
            })
        }
    };
    let mut left = 1;
    crate::IMPORT_BEFORE_COMMIT.with_borrow_mut(|hook| {
        *hook = Some(Box::new(move || {
            if left > 0 {
                left -= 1;
                raced_write();
            }
        }))
    });
    let (_, _, bridged) =
        crate::store_imported_profiles(store, active_id, bridge_plan("bridge-kept")).unwrap();
    assert!(bridged.iter().any(|p| p["id"] == "bridge-kept"));
    assert!(bridged.iter().any(|p| p["id"] == "raced"));
    assert_eq!(
        read("server_bridge-kept").unwrap().as_str(),
        "bridge-secret"
    );
    crate::IMPORT_BEFORE_COMMIT.with_borrow_mut(|hook| *hook = Some(Box::new(raced_write)));
    let bridge_refused =
        crate::store_imported_profiles(store, active_id, bridge_plan("bridge-gone"))
            .map(|_| ())
            .unwrap_err();
    crate::IMPORT_BEFORE_COMMIT.with_borrow_mut(|hook| *hook = None);
    assert_eq!(bridge_refused, users::PROFILES_CHANGED);
    assert!(read("server_bridge-gone").is_none());
    assert!(store.get("server_bridge-gone").is_err());
    let (kept, _) =
        users::cli_update_server_profiles_for_user(store, active_id, |_| false).unwrap();
    assert!(!kept.iter().any(|p| p["id"] == "bridge-gone"));

    // A file repeating an id holds one set of provider secrets: the first profile
    // brings the chain, the later one stored under a new id does not.
    let repeated_file = directory.path().join("repeated.aeroftp");
    crate::export_server_profiles_core(
        json!([jotta.clone(), jotta.clone()]).to_string(),
        "fixture-export-password".into(),
        true,
        repeated_file.to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    let repeated = import_over(
        store,
        repeated_file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Duplicate,
        &[],
        active,
        true,
    )
    .await
    .unwrap();
    assert_eq!(repeated.servers.len(), 2);
    assert_eq!(repeated.servers[1]["importOutcome"], "duplicated");
    assert_eq!(repeated.jotta_chains.len(), 1);
    assert_eq!(repeated.jotta_chains[0].id, repeated.servers[0]["id"]);

    // The profile whose own chain is the singleton refreshes it on an overwrite.
    users::store_active_credential_typed_dual(
        store,
        "jottacloud_refresh_fixture-jotta",
        "jottacloud_refresh",
        "other-profile-chain",
    )
    .unwrap();
    import_over(
        store,
        jotta_file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Overwrite,
        std::slice::from_ref(&jotta),
        active,
        false,
    )
    .await
    .unwrap();
    assert_eq!(
        store.get("jottacloud_refresh").unwrap(),
        "synthetic-refresh-chain"
    );

    let redacted = directory.path().join("headless-redacted.aeroftp");
    let metadata = crate::export_server_profiles_core(
        json!([export_profile]).to_string(),
        "fixture-export-password".into(),
        false,
        redacted.to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    assert!(!metadata.has_credentials);
    let (_, secrets, _) =
        crate::profile_export::import_profiles(&redacted, "fixture-export-password").unwrap();
    assert!(secrets.is_empty());
}

/// Native Proton sessions stay with the user who owns them through refresh,
/// copy, export, import and purge.
#[cfg(feature = "proton-native")]
async fn native_session_ownership(store: &CredentialStore, active_id: i64, selected_id: i64) {
    use crate::providers::proton_native::{
        api::SessionStore,
        session::{ProtonSession, SessionTokens},
        vault,
    };
    use secrecy::{ExposeSecret, SecretString};
    let session = |uid: &str, access: &str| ProtonSession {
        tokens: SessionTokens {
            uid: uid.into(),
            access_token: SecretString::from(access.to_string()),
            refresh_token: Some(SecretString::from(format!("refresh-{access}"))),
        },
        key_password: SecretString::from("synthetic-key-password".to_string()),
    };
    let active = vault::SessionOwner::for_user(Some(active_id));
    let selected = vault::SessionOwner::for_user(Some(selected_id));
    vault::store_session(
        vault::SessionOwner::for_user(None),
        None,
        &session("legacy-unbound", "legacy-access"),
    )
    .unwrap();
    assert_eq!(
        vault::load_session(active, None)
            .unwrap()
            .unwrap()
            .tokens
            .uid,
        "legacy-unbound"
    );
    assert!(vault::load_session(selected, None).unwrap().is_none());
    vault::delete_session(active, None).unwrap();
    let scoped = session("scoped-native-uid", "access-before");
    vault::store_session(selected, Some("native-selected"), &scoped).unwrap();
    let db = users::open_or_init_cli().unwrap();
    let count = |user: i64, key: &str| {
        db.query_row(
            "SELECT COUNT(*) FROM user_credentials WHERE user_id=?1 AND credential_id=?2",
            rusqlite::params![user, key],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    let selected_key = vault::session_key(Some("native-selected"));
    assert_eq!(count(selected_id, &selected_key), 1);
    assert_eq!(count(active_id, &selected_key), 0);

    // Refreshes continue to update their captured owner after a user switch,
    // and shared sessions reach the owners of the other profile keys.
    vault::store_session(active, Some("native-shared"), &scoped).unwrap();
    users::cli_set_active_user(store, selected_id).unwrap();
    let refresher = vault::VaultSession::new(selected, Some("native-selected"), &scoped);
    users::cli_set_active_user(store, active_id).unwrap();
    let refreshed = session("scoped-native-uid", "access-after");
    refresher.save(&refreshed.tokens);
    assert_eq!(
        vault::load_session(selected, Some("native-selected"))
            .unwrap()
            .unwrap()
            .tokens
            .access_token
            .expose_secret(),
        "access-after"
    );
    assert_eq!(
        vault::load_session(active, Some("native-shared"))
            .unwrap()
            .unwrap()
            .tokens
            .access_token
            .expose_secret(),
        "access-after"
    );
    assert_eq!(count(active_id, &selected_key), 0);
    assert_eq!(
        count(selected_id, &vault::session_key(Some("native-shared"))),
        0
    );
    // Copy/import of an old snapshot joins the UID lease and adopts the
    // current local pair, instead of creating a stale shared session.
    let old = serde_json::to_string(
        &crate::providers::proton_native::session::StoredSession::from(&scoped),
    )
    .unwrap();
    vault::restore_session(selected, "native-restored", &old)
        .await
        .unwrap();
    assert_eq!(
        vault::load_session(selected, Some("native-restored"))
            .unwrap()
            .unwrap()
            .tokens
            .access_token
            .expose_secret(),
        "access-after"
    );
    assert_eq!(
        count(selected_id, &vault::session_key(Some("native-restored"))),
        1
    );
    assert_eq!(
        count(active_id, &vault::session_key(Some("native-restored"))),
        0
    );

    // Exercise the actual GUI duplicate, encrypted export/import, and purge
    // routes too. Shared copies keep purge local: no Proton request is made.
    users::cli_set_active_user(store, selected_id).unwrap();
    let copied = crate::copy_profile_secrets(
        "native-selected".into(),
        "native-gui-copy".into(),
        Some("proton".into()),
    )
    .await
    .unwrap();
    assert_eq!(copied.get("proton_native_session"), Some(&true));
    assert_eq!(
        count(selected_id, &vault::session_key(Some("native-gui-copy"))),
        1
    );
    assert_eq!(
        count(active_id, &vault::session_key(Some("native-gui-copy"))),
        0
    );
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("native.aeroftp");
    let metadata = crate::export_server_profiles_core(
        json!([{"id":"native-selected","name":"Synthetic native profile","protocol":"proton",
            "port":443,"options":{"proton_mode":"native"}}])
        .to_string(),
        "native-fixture-export-password".into(),
        true,
        file.to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    assert!(metadata.has_credentials);
    crate::purge_profile_secrets("native-selected".into(), Some("proton".into()))
        .await
        .unwrap();
    assert_eq!(count(selected_id, &selected_key), 0);
    import_over(
        store,
        file.to_string_lossy().into_owned(),
        "native-fixture-export-password".into(),
        crate::profile_import_plan::ExistingProfileMode::Skip,
        &[],
        None,
        false,
    )
    .await
    .unwrap();
    assert_eq!(count(selected_id, &selected_key), 1);
    assert_eq!(count(active_id, &selected_key), 0);
    assert_eq!(
        vault::load_session(selected, Some("native-selected"))
            .unwrap()
            .unwrap()
            .tokens
            .access_token
            .expose_secret(),
        "access-after"
    );
    users::cli_set_active_user(store, active_id).unwrap();

    // Another account is active than the one the import is for: nothing is
    // written, for either of them.
    users::cli_set_active_user(store, selected_id).unwrap();
    crate::purge_profile_secrets("native-selected".into(), Some("proton".into()))
        .await
        .unwrap();
    users::cli_set_active_user(store, active_id).unwrap();
    assert_eq!(count(selected_id, &selected_key), 0);
    let refused = crate::import_server_profiles_with_mode(
        file.to_string_lossy().into_owned(),
        "native-fixture-export-password".into(),
        crate::profile_import_plan::ExistingProfileMode::Skip,
        selected_id,
    )
    .await
    .unwrap_err();
    assert!(refused.starts_with("ACCOUNT_CHANGED"), "{refused}");
    assert_eq!(count(selected_id, &selected_key), 0);
    assert_eq!(count(active_id, &selected_key), 0);

    // Reproduce the old takeover gap using the real vault and local lease:
    // connect read U1, a new browser sign-in stored U2, then connect binds U1.
    // U2 must survive and the destination must remain empty.
    let first = session("pending-first", "first-access");
    let replacement = session("pending-replacement", "replacement-access");
    vault::store_session(selected, None, &first).unwrap();
    let accounts = store.list_accounts().unwrap().into_iter().collect();
    let readiness = |user| {
        crate::profile_auth_state::derive_profile_auth_state_for_user(
            store,
            &accounts,
            "native-raced",
            "proton",
            crate::profile_auth_state::AuthSource::ProtonSession,
            Some(user),
        )
    };
    assert_eq!(readiness(selected_id), "valid");
    assert_eq!(readiness(active_id), "no_credentials");
    vault::store_session(active, None, &session("other-user", "other-access")).unwrap();
    vault::store_session(selected, None, &replacement).unwrap();
    assert!(
        vault::bind_unbound_session(selected, "native-raced", &first)
            .await
            .is_err()
    );
    assert!(vault::load_session(selected, Some("native-raced"))
        .unwrap()
        .is_none());
    assert_eq!(
        vault::load_session(selected, None)
            .unwrap()
            .unwrap()
            .tokens
            .uid,
        "pending-replacement"
    );
    assert_eq!(
        vault::load_session(active, None)
            .unwrap()
            .unwrap()
            .tokens
            .uid,
        "other-user"
    );

    // A refresh in the same gap is safe to adopt. Its rotated tokens, rather
    // than the stale pair captured before account verification, are bound.
    let rotated = session("pending-replacement", "rotated-access");
    vault::store_session(selected, None, &rotated).unwrap();
    let bound = vault::bind_unbound_session(selected, "native-bound", &replacement)
        .await
        .unwrap();
    assert_eq!(bound.tokens.access_token.expose_secret(), "rotated-access");
    assert!(vault::load_session(selected, None).unwrap().is_none());
    assert_eq!(
        vault::load_session(active, None)
            .unwrap()
            .unwrap()
            .tokens
            .uid,
        "other-user"
    );
    users::cli_set_active_user(store, active_id).unwrap();
    vault::delete_session(selected, Some("native-bound")).unwrap();
    assert_eq!(
        count(selected_id, &vault::session_key(Some("native-bound"))),
        0
    );
    assert!(vault::load_session(active, None).unwrap().is_some());
}
