use super::*;
use std::sync::{Arc, Mutex};

/// Slots whose write of one key fails, for a failure the real vault cannot be made
/// to produce.
struct FailingAt<'a, S> {
    slots: S,
    key: &'a str,
}

impl<S: crate::profile_import_plan::SecretSlots> crate::profile_import_plan::SecretSlots
    for FailingAt<'_, S>
{
    type Snapshot = S::Snapshot;

    fn snapshot(
        &self,
        write: &crate::profile_import_plan::SecretWrite,
    ) -> Result<S::Snapshot, String> {
        self.slots.snapshot(write)
    }

    fn write(&self, write: &crate::profile_import_plan::SecretWrite) -> Result<(), String> {
        if write.key == self.key {
            return Err("injected".to_string());
        }
        self.slots.write(write)
    }

    fn restore(
        &self,
        write: &crate::profile_import_plan::SecretWrite,
        before: S::Snapshot,
    ) -> Result<(), String> {
        self.slots.restore(write, before)
    }
}

/// The account a purge names when the test means the active one: the
/// explicit form `purge_profile_secrets` asks for once accounts exist.
fn active_account(store: &CredentialStore) -> Option<i64> {
    users::cli_get_active_user(store)
        .unwrap()
        .map(|user| user.id)
}

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

    // A profile id with path separators (an imported remote name) takes the
    // same expired-token path: the refresh runs under a lease whose file name
    // is made safe inside the token folder, and nothing is created outside it.
    let separator_id = "rclone-team/drive..\\x-1";
    let account = format!("oauth_google_{separator_id}");
    scope
        .write(&account, &serde_json::to_string(&tokens).unwrap())
        .unwrap();
    let mut separated = oauth_config(
        ProviderType::GoogleDrive,
        separator_id,
        &("fixture-client".into(), "fixture-client-secret".into()),
        "",
        19899,
    )
    .unwrap();
    separated.token_url = format!("{endpoint}/oauth2-token");
    let refreshes = |log: &Mutex<Vec<(String, String, String)>>| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|(p, _, b)| p == "/oauth2-token" && b.contains("grant_type=refresh_token"))
            .count()
    };
    let before = refreshes(&captured);
    assert_eq!(
        OAuth2Manager::new()
            .get_valid_token(&separated)
            .await
            .unwrap()
            .expose_secret(),
        "oauth-refreshed"
    );
    assert_eq!(
        refreshes(&captured),
        before + 1,
        "the expired token was refreshed"
    );
    // The whole `delete_tokens` path refused on ENOLCK, with the vault open
    // and the legacy owner active: its partition row, the vault entry and the
    // memory copy all stay; with the lock the same call removes all three.
    #[cfg(unix)]
    {
        use crate::providers::oauth2::OAuthProvider;
        let manager = OAuth2Manager::new();
        manager
            .store_tokens(OAuthProvider::Box, "enolck-profile", &tokens)
            .unwrap();
        let account = "oauth_box_enolck-profile";
        assert!(store.get(account).is_ok());
        let legacy_owner = users::cli_get_active_user(&store).unwrap().unwrap();
        let partition_rows = || {
            users::open_or_init_cli()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM user_credentials WHERE user_id=?1 AND credential_id=?2",
                    rusqlite::params![legacy_owner.id, account],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap()
        };
        assert_eq!(partition_rows(), 1, "the legacy owner's partition mirror");
        crate::providers::oauth2::seed_memory_token_for_test(account, &store.get(account).unwrap());
        assert!(
            crate::providers::oauth2::memory_token_cache_contains_for_test(account),
            "the memory cache starts populated"
        );
        crate::providers::oauth2::force_signout_enolck_for_test(true);
        let refused = manager.delete_tokens(OAuthProvider::Box, "enolck-profile");
        crate::providers::oauth2::force_signout_enolck_for_test(false);
        assert!(
            refused
                .as_ref()
                .is_err_and(|e| e.to_string().contains("nothing was signed out")),
            "{refused:?}"
        );
        assert!(store.get(account).is_ok(), "the vault entry stays");
        assert_eq!(partition_rows(), 1, "the partition row stays");
        assert!(
            crate::providers::oauth2::memory_token_cache_contains_for_test(account),
            "the refused sign-out leaves the memory copy"
        );
        assert!(manager.has_tokens(OAuthProvider::Box, "enolck-profile"));
        manager
            .delete_tokens(OAuthProvider::Box, "enolck-profile")
            .unwrap();
        assert!(store.get(account).is_err(), "the retry removes it");
        assert_eq!(partition_rows(), 0, "and the partition row");
        assert!(
            !crate::providers::oauth2::memory_token_cache_contains_for_test(account),
            "the retry removes the memory copy"
        );
        assert!(!manager.has_tokens(OAuthProvider::Box, "enolck-profile"));
    }
    let token_dir = crate::portable::aeroftp_data_root()
        .unwrap()
        .join("oauth_tokens");
    assert!(
        !token_dir.join("refresh-oauth_google_rclone-team").exists(),
        "the separator made no folder"
    );
    assert!(
        !token_dir
            .join("refresh-oauth_google_rclone-team_drive___x-1.lock")
            .exists(),
        "the lease is released after the refresh"
    );

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
    private_tokens_keep_to_their_account(&store, active.id, owner.id);
    github_readiness_reads_the_asked_accounts_profile(&store, active.id, owner.id);
    a_purge_reports_the_accounts_own_token(&store, active.id, owner.id).await;
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
        return crate::preview_server_profiles(
            &file,
            &password,
            mode,
            &crate::profile_security::RelaxationPolicy::KeepAll,
            list,
        );
    }
    let user = match user {
        Some(user) => user,
        None => users::cli_get_active_user(store)?.unwrap().id,
    };
    users::cli_update_server_profiles_for_user(store, user, |stored| {
        *stored = list.to_vec();
        true
    })?;
    crate::import_server_profiles_with_mode(
        file,
        password,
        mode,
        &crate::profile_security::RelaxationPolicy::KeepAll,
        user,
    )
    .await
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
    let copied =
        crate::copy_profile_secrets(source.into(), target.into(), Some("github".into()), None)
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
    crate::purge_profile_secrets(source.into(), Some("github".into()), active_account(store))
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
    crate::purge_profile_secrets(copy_id.into(), Some("github".into()), active_account(store))
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
            crate::purge_profile_secrets(
                copy.into(),
                Some("jottacloud".into()),
                active_account(store),
            )
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
    // Two spellings of one guarded option, the safe-looking one first: the
    // loader applies the second, so the overwrite is refused and the stored
    // password stays (CodeRabbit on #1524).
    let mut conflicting = export_profile.clone();
    conflicting["options"] = json!({"trustUnknownHosts": false, "trust_unknown_hosts": true});
    let conflicting_file = directory.path().join("conflicting.aeroftp");
    crate::export_server_profiles_core(
        json!([conflicting]).to_string(),
        "fixture-export-password".into(),
        false,
        conflicting_file.to_string_lossy().into_owned(),
    )
    .await
    .unwrap();
    let conflicting = import_over(
        store,
        conflicting_file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Overwrite,
        &current,
        active,
        false,
    )
    .await
    .unwrap();
    assert!(conflicting.servers.is_empty());
    assert_eq!(conflicting.target_changed, [field(&export_profile, "name")]);
    assert_eq!(
        read(&format!("server_{source}")).unwrap().as_str(),
        "stored-password"
    );
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
        &crate::profile_security::RelaxationPolicy::KeepAll,
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

        // An OAuth app is one unit: a failure at its last key puts the first back,
        // and clears the one that had nothing to go back to.
        store
            .store("oauth_rollback_client_id", "app-before")
            .unwrap();
        let refused = apply_secret_writes(
            &FailingAt {
                slots: crate::VaultOnlySlots { store },
                key: "oauth_rollback_region",
            },
            vec![
                SecretWrite::new("oauth_rollback_client_id".into(), None, "app-file"),
                SecretWrite::new("oauth_rollback_client_secret".into(), None, "secret-file"),
                SecretWrite::new("oauth_rollback_region".into(), None, "region-file"),
            ],
        )
        .map(|_| ())
        .unwrap_err();
        assert!(!refused.contains("app-file"), "{refused}");
        assert_eq!(store.get("oauth_rollback_client_id").unwrap(), "app-before");
        assert!(store.get("oauth_rollback_client_secret").is_err());
        assert!(store.get("oauth_rollback_region").is_err());
        store.delete("oauth_rollback_client_id").unwrap();

        // The rest of the app's writes are all or none through the same call, and a
        // name that could reach another key is refused.
        assert!(crate::write_oauth_app(
            &crate::VaultOnlySlots { store },
            "../server_x",
            [
                ("client_id", Some("x")),
                ("client_secret", None),
                ("region", None)
            ],
        )
        .is_err());
        crate::write_oauth_app(
            &crate::VaultOnlySlots { store },
            "rollback_app",
            [
                ("client_id", Some("id")),
                ("client_secret", None),
                ("region", None),
            ],
        )
        .unwrap();
        assert_eq!(store.get("oauth_rollback_app_client_id").unwrap(), "id");
        assert!(store.get("oauth_rollback_app_client_secret").is_err());
        crate::write_oauth_app(
            &crate::VaultOnlySlots { store },
            "rollback_app",
            [
                ("client_id", None),
                ("client_secret", Some("kept-secret")),
                ("region", None),
            ],
        )
        .unwrap();
        assert_eq!(
            store.get("oauth_rollback_app_client_secret").unwrap(),
            "kept-secret"
        );
        store.delete("oauth_rollback_app_client_secret").unwrap();
        // An emptied field is stored empty, and goes back whole if a later key fails.
        crate::write_oauth_app(
            &crate::VaultOnlySlots { store },
            "rollback_app",
            [
                ("client_id", Some("")),
                ("client_secret", None),
                ("region", None),
            ],
        )
        .unwrap();
        assert_eq!(store.get("oauth_rollback_app_client_id").unwrap(), "");
        assert!(crate::vault_value(store, "oauth_rollback_app_client_id")
            .unwrap()
            .is_none());
        store.store("oauth_rollback_app_client_id", "id").unwrap();
        crate::write_oauth_app(
            &FailingAt {
                slots: crate::VaultOnlySlots { store },
                key: "oauth_rollback_app_region",
            },
            "rollback_app",
            [
                ("client_id", Some("")),
                ("client_secret", None),
                ("region", Some("x")),
            ],
        )
        .unwrap_err();
        assert_eq!(store.get("oauth_rollback_app_client_id").unwrap(), "id");
        store.delete("oauth_rollback_app_client_id").unwrap();

        // An ordinary save that does not reach the row a reader prefers is an error
        // that names the key, and never its value. The vault is put back as it
        // was, so no reader finds the value of a save reported as failed.
        let conn = users::open_or_init_cli().unwrap();
        users::store_credential_for_user_dual(store, active_id, "server_dual-save", "old").unwrap();
        conn.execute_batch(
            "CREATE TRIGGER refuse_save BEFORE UPDATE ON user_credentials
             WHEN NEW.credential_id = 'server_dual-save'
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .unwrap();
        let refused = users::store_credential_for_user_dual(
            store,
            active_id,
            "server_dual-save",
            "new-value",
        )
        .unwrap_err();
        conn.execute_batch("DROP TRIGGER refuse_save;").unwrap();
        // What a reader gets is the row, not the vault: the reason for the error.
        assert_eq!(
            users::resolve_credential_for_user(store, active_id, "server_dual-save")
                .unwrap()
                .unwrap()
                .as_str(),
            "old"
        );
        assert!(refused.contains("server_dual-save"), "{refused}");
        assert!(!refused.contains("new-value"), "{refused}");
        assert_eq!(store.get("server_dual-save").unwrap(), "old");

        // An app kept in the legacy settings blob counts as stored: a config with
        // another app must not put it in front of the one existing profiles use.
        store
            .store(
                "config_oauth_clients",
                r#"{"legacyapp":{"clientId":"legacy-id","clientSecret":"legacy-secret"}}"#,
            )
            .unwrap();
        assert!(
            crate::fill_oauth_app(store, "legacyapp", Some("other-id"), Some("other-secret"))
                .unwrap()
        );
        assert!(store.get("oauth_legacyapp_client_id").is_err());
        assert!(store.get("oauth_legacyapp_client_secret").is_err());
        // A blob that cannot be parsed stops the write: it may hold the app in use.
        store.store("config_oauth_clients", "not json").unwrap();
        assert!(crate::fill_oauth_app(store, "legacyapp", Some("other-id"), None).is_err());
        assert!(store.get("oauth_legacyapp_client_id").is_err());
        store.delete("config_oauth_clients").unwrap();

        // A profile's token goes to the account holding it, not the active one.
        let root = store.derive_user_partition_wrapping_key();
        users::set_user_credential_for(&conn, &root, other_id, "oauth_box_owned", "oauth", "old")
            .unwrap();
        users::store_profile_credential(store, "owned", "oauth_box_owned", Some("oauth"), "new")
            .unwrap();
        let row = |user: i64| {
            users::get_user_credential_for(&conn, &root, user, "oauth_box_owned")
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(row(other_id).as_deref(), Some("new"));
        assert_eq!(row(active_id), None);
        // Another account's profile is not signed out from here; its own account can.
        let refused =
            users::delete_profile_credential(store, "owned", "oauth_box_owned").unwrap_err();
        assert!(refused.starts_with(users::ACCOUNT_CHANGED), "{refused}");
        assert_eq!(row(other_id).as_deref(), Some("new"));
        users::set_user_credential_for(&conn, &root, active_id, "oauth_box_owned", "oauth", "own")
            .unwrap();
        // Both accounts hold it now: a sign-in and a sign-out of the active one are
        // its own, and the other account keeps its token.
        users::store_profile_credential(
            store,
            "owned",
            "oauth_box_owned",
            Some("oauth"),
            "active-new",
        )
        .unwrap();
        assert_eq!(row(active_id).as_deref(), Some("active-new"));
        assert_eq!(row(other_id).as_deref(), Some("new"));
        users::delete_profile_credential(store, "owned", "oauth_box_owned").unwrap();
        assert_eq!(row(other_id).as_deref(), Some("new"));
        assert_eq!(row(active_id), None);
        assert!(store.get("oauth_box_owned").is_err());
        users::unmirror_credential_for_user_best_effort_any(other_id, "oauth_box_owned");

        // A key outside the typed set follows its row, as a relocation leaves one.
        let root = store.derive_user_partition_wrapping_key();
        users::set_user_credential_for(
            &conn,
            &root,
            active_id,
            "custom_relocated",
            "custom",
            "old",
        )
        .unwrap();
        users::store_credential_for_user_dual(store, active_id, "custom_relocated", "new").unwrap();
        assert_eq!(
            users::get_user_credential_for(&conn, &root, active_id, "custom_relocated")
                .unwrap()
                .unwrap()
                .as_str(),
            "new"
        );
        // One with no row stays in the vault alone.
        users::store_credential_for_user_dual(store, active_id, "custom_vault_only", "v").unwrap();
        assert!(
            users::get_user_credential_for(&conn, &root, active_id, "custom_vault_only")
                .unwrap()
                .is_none()
        );
        store.delete("custom_vault_only").unwrap();
        store.delete("custom_relocated").unwrap();
        users::delete_user_credential_for(&conn, active_id, "custom_relocated").unwrap();
        users::delete_user_credential_for(&conn, active_id, "server_dual-save").unwrap();
        store.delete("server_dual-save").unwrap();

        // Undoing a write ends with both places agreeing, whatever fails. Partition
        // step: the vault was never touched. Commit: the vault goes back to match.
        let key = "server_rollback-order";
        store.store(key, "vault-before").unwrap();
        let before = users::snapshot_credential_for_user(store, active_id, key).unwrap();
        users::store_imported_credential_for_user(store, active_id, key, None, "file-value")
            .unwrap();
        let in_partition = |conn: &rusqlite::Connection| {
            users::get_user_credential_for(
                conn,
                &store.derive_user_partition_wrapping_key(),
                active_id,
                key,
            )
            .unwrap()
            .map(|value| value.to_string())
        };
        let conn = users::open_or_init_cli().unwrap();
        conn.execute_batch(&format!(
            "CREATE TRIGGER refuse_undo BEFORE DELETE ON user_credentials
             WHEN OLD.credential_id = '{key}'
             BEGIN SELECT RAISE(ABORT, 'injected'); END;"
        ))
        .unwrap();
        users::restore_credential_for_user(store, active_id, key, None, "file-value", &before)
            .unwrap_err();
        assert_eq!(store.get(key).unwrap(), "file-value");
        assert_eq!(in_partition(&conn).unwrap(), "file-value");
        conn.execute_batch(&format!(
            "DROP TRIGGER refuse_undo;
             CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE commit_child (parent INTEGER
                 REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER refuse_commit AFTER DELETE ON user_credentials
             WHEN OLD.credential_id = '{key}'
             BEGIN INSERT INTO commit_child VALUES (1); END;"
        ))
        .unwrap();
        let refused =
            users::restore_credential_for_user(store, active_id, key, None, "file-value", &before)
                .unwrap_err();
        assert!(
            refused.starts_with("Commit credential restore"),
            "{refused}"
        );
        assert_eq!(store.get(key).unwrap(), "file-value");
        assert_eq!(in_partition(&conn).unwrap(), "file-value");
        conn.execute_batch(
            "DROP TRIGGER refuse_commit; DROP TABLE commit_child; DROP TABLE commit_parent;",
        )
        .unwrap();
        users::restore_credential_for_user(store, active_id, key, None, "file-value", &before)
            .unwrap();
        assert_eq!(store.get(key).unwrap(), "vault-before");
        assert!(in_partition(&conn).is_none());
        store.delete(key).unwrap();

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
        assert!(users::get_user_credential_for(
            &conn,
            &store.derive_user_partition_wrapping_key(),
            active_id,
            "server_rollback-partial",
        )
        .unwrap()
        .is_none());

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

    // A filesystem without file locks: an import stops before writing, says what to
    // do and changes nothing, and an ordinary save goes on.
    users::store_active_credential_typed_dual(store, &stale_key, "oauth", "stale-local").unwrap();
    let list_before = list_of(active_id);
    users::NO_FILE_LOCKS_IN_TEST.set(true);
    let without_locks = crate::import_server_profiles_with_mode(
        file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Overwrite,
        &crate::profile_security::RelaxationPolicy::KeepAll,
        active_id,
    )
    .await
    .unwrap_err();
    let saved_anyway = users::cli_update_server_profiles_for_user(store, active_id, |_| false);
    users::NO_FILE_LOCKS_IN_TEST.set(false);
    assert!(
        without_locks.contains("without file locks"),
        "{without_locks}"
    );
    assert_eq!(read(&stale_key).unwrap().as_str(), "stale-local");
    assert_eq!(list_of(active_id), list_before);
    assert!(saved_anyway.is_ok());

    // A copy for a list of another account than the active one writes nothing.
    let refused_copy = crate::copy_profile_secrets(
        source.into(),
        "copy-for-other".into(),
        Some("github".into()),
        Some(other_id),
    )
    .await
    .unwrap_err();
    assert_eq!(refused_copy, users::ACCOUNT_CHANGED);
    assert!(read(&format!("{stale_prefix}_copy-for-other")).is_none());

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
    // Stored for real: two ids, and the chain stays with the first of them. The
    // first is an added profile, which reports its overlay password and stores it.
    let repeated = import_over(
        store,
        repeated_file.to_string_lossy().into_owned(),
        "fixture-export-password".into(),
        ExistingProfileMode::Duplicate,
        &[],
        active,
        false,
    )
    .await
    .unwrap();
    assert_eq!(repeated.servers[0]["importOutcome"], "added");
    assert_eq!(repeated.servers[0]["hasStoredAeroCryptPassword"], true);
    let (first, second) = (
        field(&repeated.servers[0], "id"),
        field(&repeated.servers[1], "id"),
    );
    assert_ne!(first, second);
    let stored_first = repeated
        .stored
        .iter()
        .find(|p| p["id"].as_str() == Some(first))
        .unwrap();
    assert_eq!(stored_first["hasStoredAeroCryptPassword"], true);
    assert!(repeated
        .stored
        .iter()
        .any(|p| p["id"].as_str() == Some(second)));
    assert_eq!(
        read(&format!("jottacloud_refresh_{first}"))
            .unwrap()
            .as_str(),
        "synthetic-refresh-chain"
    );
    assert!(read(&format!("jottacloud_refresh_{second}")).is_none());

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
    // The legacy owner's session is its vault entry and its row: a delete
    // that leaves the row, which the account reads first, is an error.
    let kept = vault::session_key(Some("native-row-kept"));
    vault::store_session(
        active,
        Some("native-row-kept"),
        &session("kept-uid", "kept"),
    )
    .unwrap();
    let rows = users::open_or_init_cli().unwrap();
    rows.execute_batch(&format!(
        "CREATE TRIGGER keep_session_row BEFORE DELETE ON user_credentials
         WHEN OLD.credential_id = '{kept}'
         BEGIN SELECT RAISE(FAIL, 'disk full'); END;"
    ))
    .unwrap();
    let refused = vault::delete_session(active, Some("native-row-kept"));
    rows.execute_batch("DROP TRIGGER keep_session_row;")
        .unwrap();
    assert!(
        refused.is_err(),
        "a session delete that left the account's row was reported done"
    );
    assert!(vault::load_session(active, Some("native-row-kept"))
        .unwrap()
        .is_some());
    vault::delete_session(active, Some("native-row-kept")).unwrap();
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

    // Refreshes continue to update their captured owner after a user switch.
    // A session belongs to one account: a copy of it another account holds is
    // not reached, and that account signs in again when its pair runs out.
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
        "access-before"
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
        None,
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
    // An import brings no session another account may hold
    // (`an_import_brings_no_session_another_account_may_hold`): the legacy
    // owner's copy goes first, so the account's own export brings it back.
    vault::delete_session(active, Some("native-shared")).unwrap();
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
    crate::purge_profile_secrets(
        "native-selected".into(),
        Some("proton".into()),
        active_account(store),
    )
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
    crate::purge_profile_secrets(
        "native-selected".into(),
        Some("proton".into()),
        active_account(store),
    )
    .await
    .unwrap();
    users::cli_set_active_user(store, active_id).unwrap();
    assert_eq!(count(selected_id, &selected_key), 0);
    let refused = crate::import_server_profiles_with_mode(
        file.to_string_lossy().into_owned(),
        "native-fixture-export-password".into(),
        crate::profile_import_plan::ExistingProfileMode::Skip,
        &crate::profile_security::RelaxationPolicy::KeepAll,
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

/// The purge of a profile, run by an account other than the legacy owner,
/// reports a private token as removed where that account reads it: its own
/// row, never the owner's vault key of the same name.
async fn a_purge_reports_the_accounts_own_token(
    store: &CredentialStore,
    legacy_id: i64,
    private_id: i64,
) {
    let key = "oauth_dropbox_purge-target";
    let purge = || {
        crate::purge_profile_secrets(
            "purge-target".into(),
            Some("dropbox".into()),
            active_account(store),
        )
    };
    users::cli_set_active_user(store, private_id).unwrap();
    store.store(key, "owners-token").unwrap();
    // No owner recorded yet: the delete records the one it checks against.
    let conn = users::open_or_init_cli().unwrap();
    conn.execute(
        "DELETE FROM global_state WHERE key = 'legacy_credential_owner_id'",
        [],
    )
    .unwrap();
    // The account's own delete, with no read before it.
    users::delete_credential_dual_as(store, Some(private_id), key).unwrap();
    let recorded: Option<String> = conn
        .query_row(
            "SELECT value FROM global_state WHERE key = 'legacy_credential_owner_id'",
            [],
            |row| row.get(0),
        )
        .ok();
    let without_a_row = purge().await;
    let after_the_first = store.get(key).ok();
    // A change of admins after it does not hand the owner's key to another.
    let set_admin = |id: i64, admin: i64| {
        conn.execute(
            "UPDATE users SET is_admin = ?2 WHERE id = ?1",
            rusqlite::params![id, admin],
        )
        .unwrap();
    };
    set_admin(private_id, 1);
    set_admin(legacy_id, 0);
    users::store_credential_for_user_dual(store, private_id, key, "own-token").unwrap();
    let with_its_row = purge().await;
    set_admin(legacy_id, 1);
    set_admin(private_id, 0);
    users::cli_set_active_user(store, legacy_id).unwrap();
    assert_eq!(recorded, Some(legacy_id.to_string()));
    assert!(!without_a_row
        .unwrap()
        .removed
        .iter()
        .any(|removed| removed == key));
    assert!(with_its_row
        .unwrap()
        .removed
        .iter()
        .any(|removed| removed == key));
    // Neither purge took the owner's vault key of the same name.
    assert_eq!(after_the_first.as_deref(), Some("owners-token"));
    assert_eq!(store.get(key).unwrap(), "owners-token");
    let conn = users::open_or_init_cli().unwrap();
    let root = store.derive_user_partition_wrapping_key();
    assert_eq!(
        users::get_user_credential_for(&conn, &root, private_id, key).unwrap(),
        None
    );
    // What the purge reports for a locked account: its row, seen by its key
    // without the account's key, never the owner's vault copy of the same name.
    let locked = {
        let mut conn = users::open_or_init_cli().unwrap();
        users::create_user(
            &mut conn,
            &root,
            "Locked purge",
            None,
            None,
            Some("purge pass"),
        )
        .unwrap()
    };
    users::cli_unlock_user(store, locked.id, Some("purge pass")).unwrap();
    users::store_credential_for_user_dual(store, locked.id, key, "locked-token").unwrap();
    users::clear_user_session();
    let with_its_row = users::holds_credential(store, locked.id, key);
    users::delete_user_credential_for(&conn, locked.id, key).unwrap();
    let without_it = users::holds_credential(store, locked.id, key);
    users::delete_user(&mut users::open_or_init_cli().unwrap(), locked.id).unwrap();
    assert_eq!(
        with_its_row,
        Ok(true),
        "a locked account's row was not seen"
    );
    assert_eq!(
        without_it,
        Ok(false),
        "the owner's vault copy was counted as a locked account's"
    );
    assert_eq!(store.get(key).unwrap(), "owners-token");
    let _ = store.delete(key);
}

/// A private token written through the untyped dual writer (a profile copy,
/// `profile duplicate`) by an account other than the legacy owner lands in
/// that account's partition only: the vault key of the same name, the legacy
/// owner's, keeps its value. Read back, that account sees its own copy, or
/// nothing once it is gone, never the owner's; the owner still reads its own.
/// Any other key keeps the dual write.
/// The GitHub readiness of a profile of `asked`, which is not the active
/// account, reads that account's profile: the active account lists a
/// profile of the same id in another sign-in mode, whose credential `asked`
/// does not hold, and the answer would be that mode's.
fn github_readiness_reads_the_asked_accounts_profile(
    store: &CredentialStore,
    active_id: i64,
    asked: i64,
) {
    let id = "fixture-github-readiness";
    let was_active = users::cli_get_active_user(store)
        .unwrap()
        .map(|user| user.id);
    users::cli_set_active_user(store, active_id).unwrap();
    let profile = |mode: &str| {
        json!({"id": id, "name": id, "protocol": "github", "host": "",
               "options": {"githubAuthMode": mode}})
    };
    let mut lists = Vec::new();
    for (user, mode) in [(active_id, "app"), (asked, "authorize")] {
        let mut before = Vec::new();
        users::cli_update_server_profiles_for_user(store, user, |stored| {
            before = stored.clone();
            stored.push(profile(mode));
            true
        })
        .unwrap();
        lists.push((user, before));
    }
    let key = format!("github_oauth_{id}");
    users::store_credential_for_user_typed_dual(store, asked, &key, "oauth", "gho_readiness")
        .unwrap();
    let readiness = saved_github_readiness(store, id, None, Some(asked));
    users::delete_credential_for_user_dual(store, asked, &key).unwrap();
    for (user, before) in lists {
        users::cli_update_server_profiles_for_user(store, user, |stored| {
            *stored = before.clone();
            true
        })
        .unwrap();
    }
    if let Some(user) = was_active {
        users::cli_set_active_user(store, user).unwrap();
    }
    assert_eq!(
        readiness, "valid",
        "the readiness read the active account's profile of the same id"
    );
}

fn private_tokens_keep_to_their_account(store: &CredentialStore, legacy_id: i64, private_id: i64) {
    for key in [
        "oauth_dropbox_copy-target",
        "jottacloud_refresh_copy-target",
        "profile_auth_config_copy-target",
        "github_oauth_copy-target",
    ] {
        store.store(key, "legacy-owners-token").unwrap();
        users::store_credential_for_user_dual(store, private_id, key, "private-token").unwrap();
        assert_eq!(store.get(key).unwrap(), "legacy-owners-token", "{key}");
        let read = |user: i64| {
            users::resolve_credential_for_user(store, user, key)
                .unwrap()
                .map(|value| value.to_string())
        };
        assert_eq!(read(private_id).as_deref(), Some("private-token"), "{key}");
        users::unmirror_credential_for_user_best_effort_any(private_id, key);
        assert_eq!(read(private_id), None, "{key}");
        assert_eq!(
            read(legacy_id).as_deref(),
            Some("legacy-owners-token"),
            "{key}"
        );
        // A sign-out or purge through the dual deletes removes the account's
        // row, and the owner's vault key of the same name stays.
        users::store_credential_for_user_dual(store, private_id, key, "private-token").unwrap();
        users::delete_credential_for_user_dual(store, private_id, key).unwrap();
        assert_eq!(read(private_id), None, "{key}");
        assert_eq!(store.get(key).unwrap(), "legacy-owners-token", "{key}");
        users::store_credential_for_user_dual(store, private_id, key, "private-token").unwrap();
        users::delete_credential_dual_as(store, Some(private_id), key).unwrap();
        assert_eq!(read(private_id), None, "{key}");
        assert_eq!(store.get(key).unwrap(), "legacy-owners-token", "{key}");
        // The rollback of a failed copy removes the account's row, and the
        // owner's vault key of the same name stays.
        users::store_credential_for_user_dual(store, private_id, key, "private-token").unwrap();
        users::delete_credential_for_user_checked(store, private_id, key).unwrap();
        assert_eq!(read(private_id), None, "{key}");
        assert_eq!(store.get(key).unwrap(), "legacy-owners-token", "{key}");
        store.delete(key).unwrap();
    }
    // With the partition DB unopenable, the rollback still removes a plain
    // secret from the vault and returns the error; a private token's vault
    // key, whose owner only the DB names, is kept.
    let unopenable = || Err("Open partition DB: refused".to_string());
    store
        .store("oauth_dropbox_copy-target", "legacy-owners-token")
        .unwrap();
    let kept = users::delete_credential_checked_with(
        store,
        unopenable(),
        private_id,
        "oauth_dropbox_copy-target",
    )
    .unwrap_err();
    assert!(kept.contains("Open partition DB: refused"), "{kept}");
    assert_eq!(
        store.get("oauth_dropbox_copy-target").unwrap(),
        "legacy-owners-token"
    );
    store.delete("oauth_dropbox_copy-target").unwrap();
    store.store("server_copy-target", "server").unwrap();
    let removed = users::delete_credential_checked_with(
        store,
        unopenable(),
        private_id,
        "server_copy-target",
    )
    .unwrap_err();
    assert!(removed.contains("Open partition DB: refused"), "{removed}");
    assert!(store.get("server_copy-target").is_err());

    users::store_credential_for_user_dual(store, private_id, "server_copy-target", "server")
        .unwrap();
    assert_eq!(store.get("server_copy-target").unwrap(), "server");
    // The row itself, not a reader that falls back to the vault.
    assert_eq!(
        users::get_user_credential_for(
            &users::open_or_init_cli().unwrap(),
            &store.derive_user_partition_wrapping_key(),
            private_id,
            "server_copy-target",
        )
        .unwrap()
        .map(|value| value.to_string())
        .as_deref(),
        Some("server")
    );
    users::unmirror_credential_for_user_best_effort_any(private_id, "server_copy-target");
    store.delete("server_copy-target").unwrap();

    // Readiness reads the same place: the owner's live token in the vault
    // says nothing about the other account, whose own token, expired, is
    // the one that counts for it.
    let token = |expires_at: i64| {
        serde_json::json!({
            "access_token": "access",
            "refresh_token": "refresh",
            "expires_at": expires_at,
            "token_type": "Bearer",
            "scopes": [],
        })
        .to_string()
    };
    let now = chrono::Utc::now().timestamp();
    store
        .store("oauth_dropbox_copy-target", &token(now + 3600))
        .unwrap();
    let readiness = |user: i64| {
        let accounts = store.list_accounts().unwrap().into_iter().collect();
        crate::profile_auth_state::derive_profile_auth_state_for_user(
            store,
            &accounts,
            "copy-target",
            "dropbox",
            crate::profile_auth_state::AuthSource::Vault,
            Some(user),
        )
    };
    assert_eq!(readiness(legacy_id), "valid");
    assert_eq!(readiness(private_id), "no_credentials");
    users::store_credential_for_user_dual(
        store,
        private_id,
        "oauth_dropbox_copy-target",
        &token(now - 3600),
    )
    .unwrap();
    assert_eq!(readiness(private_id), "needs_refresh");
    assert_eq!(readiness(legacy_id), "valid");
    users::unmirror_credential_for_user_best_effort_any(private_id, "oauth_dropbox_copy-target");
    store.delete("oauth_dropbox_copy-target").unwrap();
    // The legacy owner's state is read where its connection reads the token:
    // its row first, the vault after it, so a valid row counts whatever the
    // vault holds.
    let conn = users::open_or_init_cli().unwrap();
    let root = store.derive_user_partition_wrapping_key();
    users::set_user_credential_for(
        &conn,
        &root,
        legacy_id,
        "oauth_dropbox_copy-target",
        "oauth",
        &token(now + 3600),
    )
    .unwrap();
    assert_eq!(
        readiness(legacy_id),
        "valid",
        "a valid row and no vault copy"
    );
    store
        .store("oauth_dropbox_copy-target", &token(now - 3600))
        .unwrap();
    assert_eq!(
        readiness(legacy_id),
        "valid",
        "a valid row over an expired vault copy"
    );
    users::delete_user_credential_for(&conn, legacy_id, "oauth_dropbox_copy-target").unwrap();
    store.delete("oauth_dropbox_copy-target").unwrap();

    // The same for a Jottacloud chain, under the profile's key and under the
    // singleton, for GitHub's own authorization, which is read for the account
    // asked about rather than the active one, and for an account whose locked
    // partition cannot say.
    let state = |user: i64, profile: &str, protocol: &str| {
        let accounts = store.list_accounts().unwrap().into_iter().collect();
        crate::profile_auth_state::derive_profile_auth_state_for_user(
            store,
            &accounts,
            profile,
            protocol,
            crate::profile_auth_state::AuthSource::Vault,
            Some(user),
        )
    };
    for key in ["jottacloud_refresh_copy-target", "jottacloud_refresh"] {
        store
            .store(key, r#"{"refresh_token":"owners-chain"}"#)
            .unwrap();
        assert_eq!(
            state(legacy_id, "copy-target", "jottacloud"),
            "valid",
            "{key}"
        );
        assert_eq!(
            state(private_id, "copy-target", "jottacloud"),
            "no_credentials",
            "{key}"
        );
        users::store_credential_for_user_dual(
            store,
            private_id,
            key,
            r#"{"refresh_token":"others-chain"}"#,
        )
        .unwrap();
        assert_eq!(
            state(private_id, "copy-target", "jottacloud"),
            "valid",
            "{key}"
        );
        users::unmirror_credential_for_user_best_effort_any(private_id, key);
        store.delete(key).unwrap();
    }
    store
        .store("profile_auth_config_gh-target", r#"{"mode":"authorize"}"#)
        .unwrap();
    store.store("github_oauth_gh-target", "gho_owners").unwrap();
    assert_eq!(state(legacy_id, "gh-target", "github"), "valid");
    assert_eq!(state(private_id, "gh-target", "github"), "no_credentials");
    store.delete("profile_auth_config_gh-target").unwrap();
    store.delete("github_oauth_gh-target").unwrap();
    let locked = {
        let mut conn = users::open_or_init_cli().unwrap();
        let root = store.derive_user_partition_wrapping_key();
        users::create_user(
            &mut conn,
            &root,
            "Locked",
            None,
            None,
            Some("locked passphrase"),
        )
        .unwrap()
    };
    assert_eq!(state(locked.id, "copy-target", "dropbox"), "unknown");
    users::delete_user(&mut users::open_or_init_cli().unwrap(), locked.id).unwrap();

    // An import run by the other account writes, undoes and deletes its own
    // row, also when the write names no type: the vault key of the same name
    // keeps the owner's value throughout, and so does a sign-out.
    let key = "oauth_box_import-target";
    store.store(key, "owners-token").unwrap();
    let row = |user: i64| {
        let conn = users::open_or_init_cli().unwrap();
        let root = store.derive_user_partition_wrapping_key();
        users::get_user_credential_for(&conn, &root, user, key)
            .unwrap()
            .map(|value| value.to_string())
    };
    let before = users::snapshot_credential_for_user(store, private_id, key).unwrap();
    users::store_imported_credential_for_user(store, private_id, key, None, "file-token").unwrap();
    assert_eq!(store.get(key).unwrap(), "owners-token");
    assert_eq!(row(private_id).as_deref(), Some("file-token"));
    // The owner signs in meanwhile with the same value: the undo of the other
    // account's write leaves it.
    store.store(key, "file-token").unwrap();
    users::restore_credential_for_user(store, private_id, key, None, "file-token", &before)
        .unwrap();
    assert_eq!(store.get(key).unwrap(), "file-token");
    assert_eq!(row(private_id), None);
    store.store(key, "owners-token").unwrap();
    users::store_imported_credential_for_user(store, private_id, key, None, "file-token").unwrap();
    users::delete_imported_credential_for_user(store, private_id, key).unwrap();
    assert_eq!(store.get(key).unwrap(), "owners-token");
    assert_eq!(row(private_id), None);
    users::store_imported_credential_for_user(store, private_id, key, None, "file-token").unwrap();
    users::cli_set_active_user(store, private_id).unwrap();
    let signed_out = users::delete_profile_credential(store, "import-target", key);
    users::cli_set_active_user(store, legacy_id).unwrap();
    signed_out.unwrap();
    assert_eq!(store.get(key).unwrap(), "owners-token");
    assert_eq!(row(private_id), None);
    store.delete(key).unwrap();
}

/// A fresh vault with no active user, for `test` to read and corrupt.
fn with_unreadable_profile_vault(test: impl FnOnce(&CredentialStore)) {
    let _env = crate::test_env::lock();
    let _partition = crate::user_partitions::test_lock();
    let dir = tempfile::TempDir::new().unwrap();
    let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
    let previous_passphrase = std::env::var_os("AEROFTP_USER_PASSPHRASE");
    std::env::set_var("XDG_CONFIG_HOME", dir.path());
    std::env::remove_var("AEROFTP_USER_PASSPHRASE");
    CredentialStore::clear_cache();
    CredentialStore::bootstrap_master_password("unreadable-profile-test").unwrap();
    CredentialStore::unlock_with_master("unreadable-profile-test", None).unwrap();
    let store = CredentialStore::from_cache().unwrap();
    crate::user_partitions::init_or_migrate_cli(&store).unwrap();
    test(&store);
    CredentialStore::clear_cache();
    match previous_xdg {
        Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    if let Some(value) = previous_passphrase {
        std::env::set_var("AEROFTP_USER_PASSPHRASE", value);
    }
}

/// CodeRabbit (CLI deep, #1574): a profile token that cannot be read is an
/// error, not a missing token. The legacy singleton beside it may be another
/// profile's account, so the connect and the rclone export that read the
/// token through `resolve_profile_oauth_token` refuse instead of using it.
#[test]
fn an_unreadable_profile_token_never_falls_back_to_the_singleton() {
    with_unreadable_profile_vault(|store| {
        store
            .store("oauth_fourshared_mine", "own-token:own-secret")
            .unwrap();
        store
            .store("oauth_fourshared", "other-token:other-secret")
            .unwrap();
        store
            .store(
                "fourshared_oauth_settings",
                r#"{"consumer_key":"ck","consumer_secret":"cs"}"#,
            )
            .unwrap();
        store.store("oauth_google_mine", "own-google").unwrap();
        store.store("oauth_google", "other-google").unwrap();
        store.corrupt_entry_for_test("oauth_fourshared_mine");
        store.corrupt_entry_for_test("oauth_google_mine");

        assert!(
            crate::bridge_commands::resolve_profile_oauth_token(store, "fourshared", "mine")
                .is_err()
        );
        // The GUI 4shared connect.
        let loaded = crate::provider_commands::load_fourshared_tokens("mine");
        assert!(
            loaded
                .as_ref()
                .is_err_and(|e| !e.contains("No 4shared tokens")),
            "{loaded:?}"
        );
        // The CLI and MCP 4shared connect (client configured, so only the token
        // decides it).
        let scope = AuthScope {
            store,
            user_id: None,
        };
        let four = json!({"id":"mine","name":"synthetic","protocol":"fourshared","host":""});
        let opened = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(provider_at(&scope, &four, &AuthEndpoints::default()));
        assert!(opened.is_err(), "connected with another account's token");
        // The rclone export (GUI bridge and CLI share this helper).
        let mut options = None;
        let injected = crate::bridge_commands::inject_rclone_oauth_export_options(
            &mut options,
            store,
            "googledrive",
            "mine",
            scope.user_id,
        );
        assert!(injected.is_err());
        assert!(!options
            .unwrap_or_default()
            .to_string()
            .contains("other-google"));
    });
}

/// CodeRabbit 4661188765: the rclone export resolves the OAuth token for the
/// account the exported list belongs to, captured before the export ran.
/// With that account holding no token and another account active with one
/// for the same profile id, the export must not pick the active account's.
#[test]
fn the_rclone_export_reads_the_token_of_the_captured_account() {
    with_unreadable_profile_vault(|store| {
        let captured = users::cli_create_user(store, "Export captured", None, None, None).unwrap();
        let active = users::cli_create_user(store, "Export active", None, None, None).unwrap();
        users::cli_set_active_user(store, active.id).unwrap();
        users::store_credential_for_user_dual(
            store,
            active.id,
            "oauth_google_mine",
            "active-google-token",
        )
        .unwrap();
        // Sanity: the unscoped read is the active account's token, the bug
        // this pins against.
        assert_eq!(
            crate::bridge_commands::resolve_profile_oauth_token(store, "google", "mine")
                .unwrap()
                .as_deref(),
            Some("active-google-token")
        );
        // The captured account has no token for the profile: nothing is
        // injected, and above all not the active account's.
        let mut options = None;
        let injected = crate::bridge_commands::inject_rclone_oauth_export_options(
            &mut options,
            store,
            "googledrive",
            "mine",
            Some(captured.id),
        );
        assert!(
            !options
                .unwrap_or_default()
                .to_string()
                .contains("active-google-token"),
            "the active account's token was exported for another account's profile: {injected:?}"
        );
    });
}

/// The `.aeroftp` export of an OAuth profile whose own token cannot be read
/// fails instead of bundling the legacy singleton (CodeRabbit, #1574).
#[test]
fn an_unreadable_oauth_token_fails_the_profile_export() {
    with_unreadable_profile_vault(|store| {
        store.store("oauth_google_mine", "own-google").unwrap();
        store.store("oauth_google", "other-google").unwrap();
        let server = crate::profile_export::ServerProfileExport {
            id: "mine".into(),
            name: "drive".into(),
            protocol: Some("googledrive".into()),
            ..Default::default()
        };
        let secrets = crate::collect_provider_secrets_for_server(store, &server).unwrap();
        assert_eq!(secrets.oauth.as_deref(), Some("own-google"));
        store.corrupt_entry_for_test("oauth_google_mine");
        let secrets = crate::collect_provider_secrets_for_server(store, &server);
        assert!(
            secrets.as_ref().is_err_and(|e| e.contains("drive")),
            "{:?}",
            secrets.map(|s| s.oauth)
        );
        // Absent, not unreadable: the singleton is still the fallback.
        store.delete("oauth_google_mine").unwrap();
        let secrets = crate::collect_provider_secrets_for_server(store, &server).unwrap();
        assert_eq!(secrets.oauth.as_deref(), Some("other-google"));
    });
}

/// The same for a Jottacloud refresh token (CodeRabbit, #1574).
#[test]
fn an_unreadable_jotta_token_fails_the_profile_export() {
    with_unreadable_profile_vault(|store| {
        store.store("jottacloud_refresh_mine", "own-jotta").unwrap();
        store.store("jottacloud_refresh", "other-jotta").unwrap();
        let server = crate::profile_export::ServerProfileExport {
            id: "mine".into(),
            name: "jotta".into(),
            protocol: Some("jottacloud".into()),
            ..Default::default()
        };
        store.corrupt_entry_for_test("jottacloud_refresh_mine");
        let secrets = crate::collect_provider_secrets_for_server(store, &server);
        assert!(
            secrets.as_ref().is_err_and(|e| e.contains("jotta")),
            "{:?}",
            secrets.map(|s| s.jotta_refresh)
        );
        store.delete("jottacloud_refresh_mine").unwrap();
        let secrets = crate::collect_provider_secrets_for_server(store, &server).unwrap();
        assert_eq!(secrets.jotta_refresh.as_deref(), Some("other-jotta"));
    });
}

/// The vault and partition database of an isolated child
/// ([`crate::provider_commands::isolated_account_fixture`]), opened and
/// migrated, the legacy owner active, and `others` more accounts without a
/// passphrase, each one aligned already. Returns the owner and their ids.
fn isolated_accounts(others: usize) -> (CredentialStore, i64, Vec<i64>) {
    CredentialStore::clear_cache();
    CredentialStore::bootstrap_master_password("synthetic profile auth fixture").unwrap();
    CredentialStore::unlock_with_master("synthetic profile auth fixture", None).unwrap();
    let store = CredentialStore::from_cache().unwrap();
    users::init_or_migrate_cli(&store).unwrap();
    let mut conn = users::open_or_init_cli().unwrap();
    let root = zeroize::Zeroizing::new(store.derive_user_partition_wrapping_key());
    let owner = users::active_user_id(&conn).unwrap().unwrap();
    let ids = (0..others)
        .map(|n| {
            users::create_passphrase_less_user(&mut conn, &root, &format!("Other {n}"), None, None)
                .unwrap()
                .id
        })
        .collect();
    users::set_active_user(&conn, owner).unwrap();
    // Their one-time passes run now, before the test writes a vault key.
    users::init_or_migrate_cli(&store).unwrap();
    (store, owner, ids)
}

/// `user`'s list, with a profile of each id in `ids`.
fn hold_profiles(store: &CredentialStore, user: i64, ids: &[&str]) {
    let mut conn = users::open_or_init_cli().unwrap();
    let root = zeroize::Zeroizing::new(store.derive_user_partition_wrapping_key());
    let profiles: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "name": id, "protocol": "dropbox", "host": "", "port": 0}))
        .collect();
    users::replace_server_profiles_for(&mut conn, &root, user, &profiles).unwrap();
}

/// A profile-bound read of a private token by an account other than the
/// legacy owner is its row alone: holding the profile alone does not make the
/// vault key of that name its own. The legacy owner keeps the vault fallback,
/// and a key that is not a private token keeps the sole holder's.
#[test]
fn a_sole_holder_reads_no_vault_token_of_another_account() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "profile_auth::tests::a_sole_holder_reads_no_vault_token_of_another_account",
    ) {
        return;
    }
    let _guard = users::test_lock();
    let (store, owner, others) = isolated_accounts(1);
    let other = others[0];
    hold_profiles(&store, other, &["p"]);
    hold_profiles(&store, owner, &["q"]);
    store.store("oauth_dropbox_p", "owners-token").unwrap();
    store.store("server_p", "p-password").unwrap();
    store.store("oauth_dropbox_q", "q-token").unwrap();
    let read = |user: i64, profile: &str, key: &str| {
        AuthScope {
            store: &store,
            user_id: Some(user),
        }
        .read_profile(profile, key)
        .unwrap()
    };
    assert_eq!(read(other, "p", "oauth_dropbox_p"), None);
    assert_eq!(read(other, "p", "server_p").as_deref(), Some("p-password"));
    assert_eq!(
        read(owner, "q", "oauth_dropbox_q").as_deref(),
        Some("q-token")
    );
}

/// GitHub readiness without a profile reads the account once: the profile is
/// the one that account lists and the credential the one it holds, never the
/// profile of one account paired with another's credentials after a switch.
#[test]
fn github_readiness_reads_one_account_for_its_profile_and_credential() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "profile_auth::tests::github_readiness_reads_one_account_for_its_profile_and_credential",
    ) {
        return;
    }
    let _guard = users::test_lock();
    let (store, owner, others) = isolated_accounts(1);
    let other = others[0];
    let mut conn = users::open_or_init_cli().unwrap();
    let root = zeroize::Zeroizing::new(store.derive_user_partition_wrapping_key());
    users::replace_server_profiles_for(
        &mut conn,
        &root,
        owner,
        &[json!({"id": "gh", "name": "gh", "protocol": "github", "host": ""})],
    )
    .unwrap();
    users::set_user_credential_for(
        &conn,
        &root,
        owner,
        "server_gh",
        users::relocate_secret_kind("server_gh"),
        "ghp_synthetic",
    )
    .unwrap();
    // The other account holds a profile of the same id signed in another
    // way, with no token: a lookup of its list would read no credential.
    users::replace_server_profiles_for(
        &mut conn,
        &root,
        other,
        &[
            json!({"id": "gh", "name": "gh", "protocol": "github", "host": "",
                 "options": {"githubAuthMode": "authorize"}}),
        ],
    )
    .unwrap();
    assert_eq!(
        saved_github_readiness(&store, "gh", None, Some(other)),
        "no_credentials"
    );
    assert_eq!(saved_github_readiness(&store, "gh", None, None), "valid");
    AFTER_READINESS_ACCOUNT.set(Some(Box::new(move || {
        users::set_active_user(&users::open_or_init_cli().unwrap(), other).unwrap();
    })));
    assert_eq!(saved_github_readiness(&store, "gh", None, None), "valid");
}

/// GitHub readiness with no active account reads the legacy list and the
/// vault: an account activated between its lookup and the list is not asked
/// for its profile, whose mode would pair with the vault's credentials.
#[test]
fn github_readiness_with_no_account_reads_the_legacy_list() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "profile_auth::tests::github_readiness_with_no_account_reads_the_legacy_list",
    ) {
        return;
    }
    let _guard = users::test_lock();
    let (store, _owner, others) = isolated_accounts(1);
    let other = others[0];
    let mut conn = users::open_or_init_cli().unwrap();
    let root = zeroize::Zeroizing::new(store.derive_user_partition_wrapping_key());
    users::replace_server_profiles_for(
        &mut conn,
        &root,
        other,
        &[
            json!({"id": "gh", "name": "gh", "protocol": "github", "host": "",
                 "options": {"githubAuthMode": "authorize"}}),
        ],
    )
    .unwrap();
    store
        .store(
            "config_server_profiles",
            &json!([{"id": "gh", "name": "gh", "protocol": "github", "host": ""}]).to_string(),
        )
        .unwrap();
    store.store("server_gh", "ghp_synthetic").unwrap();
    conn.execute("DELETE FROM global_state WHERE key = 'active_user_id'", [])
        .unwrap();
    assert!(users::cli_get_active_user(&store).unwrap().is_none());
    AFTER_READINESS_ACCOUNT.set(Some(Box::new(move || {
        users::set_active_user(&users::open_or_init_cli().unwrap(), other).unwrap();
    })));
    assert_eq!(saved_github_readiness(&store, "gh", None, None), "valid");
}

/// A sign-in of a private token whose profile two accounts hold, while a
/// third one is active or none is, is refused through
/// `store_profile_credential` and writes nothing: neither holder's row and
/// no vault key.
#[test]
fn a_sign_in_two_other_accounts_hold_is_refused_and_writes_nothing() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "profile_auth::tests::a_sign_in_two_other_accounts_hold_is_refused_and_writes_nothing",
    ) {
        return;
    }
    let _guard = users::test_lock();
    let (store, _owner, others) = isolated_accounts(3);
    let (first, second, active) = (others[0], others[1], others[2]);
    let conn = users::open_or_init_cli().unwrap();
    let root = zeroize::Zeroizing::new(store.derive_user_partition_wrapping_key());
    let key = "oauth_box_owned";
    for (user, token) in [(first, "first-token"), (second, "second-token")] {
        hold_profiles(&store, user, &["owned"]);
        users::set_user_credential_for(&conn, &root, user, key, "oauth", token).unwrap();
    }
    users::set_active_user(&conn, active).unwrap();
    let row = |user: i64| {
        users::get_user_credential_for(&conn, &root, user, key)
            .unwrap()
            .map(|value| value.to_string())
    };

    let refused =
        users::store_profile_credential(&store, "owned", key, Some("oauth"), "x").unwrap_err();
    assert!(refused.starts_with(users::ACCOUNT_CHANGED), "{refused}");
    assert_eq!(row(first).as_deref(), Some("first-token"));
    assert_eq!(row(second).as_deref(), Some("second-token"));
    assert_eq!(row(active), None);
    assert!(store.get(key).is_err(), "no vault key");

    // With no active account the same sign-in is refused the same way.
    conn.execute("DELETE FROM global_state WHERE key = 'active_user_id'", [])
        .unwrap();
    assert_eq!(users::active_user_id(&conn).unwrap(), None);
    let refused =
        users::store_profile_credential(&store, "owned", key, Some("oauth"), "x").unwrap_err();
    assert!(refused.starts_with(users::ACCOUNT_CHANGED), "{refused}");
    assert_eq!(row(first).as_deref(), Some("first-token"));
    assert_eq!(row(second).as_deref(), Some("second-token"));
    assert!(
        store.get(key).is_err(),
        "no vault key with no active account"
    );
}

/// Undoing an import write puts a private row back as it was held: one the
/// account's own sign-in stamped comes back stamped, so the readers and the
/// alignment keep taking it for the account's own, and one that was not
/// stamped comes back unstamped.
#[test]
fn a_rollback_puts_a_rows_sign_in_stamp_back_with_it() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "profile_auth::tests::a_rollback_puts_a_rows_sign_in_stamp_back_with_it",
    ) {
        return;
    }
    let _guard = users::test_lock();
    let (store, _, others) = isolated_accounts(1);
    let other = others[0];
    hold_profiles(&store, other, &["p", "q"]);
    let (stamped, unstamped) = ("oauth_dropbox_p", "oauth_dropbox_q");
    users::store_signed_in_credential_for_user(&store, other, stamped, "oauth", "mine").unwrap();
    users::store_imported_credential_for_user(&store, other, unstamped, Some("oauth"), "copied")
        .unwrap();
    let conn = users::open_or_init_cli().unwrap();
    let root = zeroize::Zeroizing::new(store.derive_user_partition_wrapping_key());
    for (key, held, signed_in) in [(stamped, "mine", true), (unstamped, "copied", false)] {
        assert_eq!(users::signed_in_row(&conn, other, key).unwrap(), signed_in);
        let before = users::snapshot_credential_for_user(&store, other, key).unwrap();
        users::store_imported_credential_for_user(&store, other, key, Some("oauth"), "file-value")
            .unwrap();
        users::restore_credential_for_user(
            &store,
            other,
            key,
            Some("oauth"),
            "file-value",
            &before,
        )
        .unwrap();
        assert_eq!(
            users::get_user_credential_for(&conn, &root, other, key)
                .unwrap()
                .as_deref()
                .map(|value| value.as_str()),
            Some(held)
        );
        assert_eq!(
            users::signed_in_row(&conn, other, key).unwrap(),
            signed_in,
            "{key} came back with another sign-in stamp than it had"
        );
    }
}
