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
    server.abort();
    assert!(gaps.is_empty(), "Auth lifecycle gaps: {gaps:?}");
}
