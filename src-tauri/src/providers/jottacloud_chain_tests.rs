//! How a Jottacloud refresh chain's deletion is recorded across processes,
//! and what a rotation writes, keeps and hands out around one.

use super::*;
use crate::credential_store::CredentialStore;
use crate::user_partitions as partitions;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

/// A partition with its first account, a vault open in master mode and a
/// token endpoint that answers every refresh with `rotated-<n>`, the n-th
/// call. Dropped in reverse order of the locks it took.
pub(super) struct Chains {
    pub(super) store: CredentialStore,
    pub(super) first: i64,
    pub(super) owner: Option<i64>,
    pub(super) endpoint: String,
    pub(super) calls: Arc<Mutex<usize>>,
    pub(super) runtime: Option<tokio::runtime::Runtime>,
    server: Option<tokio::task::JoinHandle<()>>,
    _temporary: tempfile::TempDir,
    _partition: partitions::TestLock,
    _env: std::sync::MutexGuard<'static, ()>,
}

pub(super) const PASSWORD: &str = "jotta-chain-test-pass";

impl Chains {
    pub(super) fn new() -> Self {
        let env = crate::test_env::lock();
        let partition = partitions::test_lock();
        let temporary = tempfile::TempDir::new().expect("temporary config");
        std::env::set_var("XDG_CONFIG_HOME", temporary.path());
        std::env::remove_var("AEROFTP_USER_PASSPHRASE");
        CredentialStore::clear_cache();
        CredentialStore::bootstrap_master_password(PASSWORD).expect("bootstrap");
        CredentialStore::unlock_with_master(PASSWORD, None).expect("unlock");
        let store = CredentialStore::from_cache().expect("cached vault");
        partitions::init_or_migrate_cli(&store).expect("init partitions");
        let conn = partitions::open_or_init_cli().expect("partition DB");
        let first = partitions::get_active_user(&conn)
            .expect("active user")
            .expect("first account")
            .id;
        let owner = chain_owner(Some(first)).expect("owner");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let calls = Arc::new(Mutex::new(0usize));
        let (endpoint, server) = runtime.block_on(async {
            let calls = calls.clone();
            let app = axum::Router::new().route(
                "/token",
                axum::routing::post(move |_body: String| {
                    let calls = calls.clone();
                    async move {
                        let call = {
                            let mut calls = calls.lock().unwrap();
                            *calls += 1;
                            *calls
                        };
                        axum::Json(serde_json::json!({
                            "access_token": format!("access-{call}"),
                            "refresh_token": format!("rotated-{call}"),
                            "expires_in": 3600,
                        }))
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (endpoint, server)
        });
        Self {
            store,
            first,
            owner,
            endpoint,
            calls,
            runtime: Some(runtime),
            server: Some(server),
            _temporary: temporary,
            _partition: partition,
            _env: env,
        }
    }

    pub(super) fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime.as_ref().unwrap()
    }

    /// A stored chain's JSON for this endpoint.
    pub(super) fn chain(&self, token: &str) -> String {
        serde_json::json!({
            "refresh_token": token, "token_endpoint": self.endpoint, "username": "jotta-user",
        })
        .to_string()
    }

    /// The refresh token the first account reads under `key` (empty for none).
    pub(super) fn stored(&self, key: &str) -> String {
        stored_refresh_token(
            partitions::resolve_credential_for_user(&self.store, self.first, key)
                .unwrap()
                .as_deref()
                .map(|value| value.as_str()),
        )
    }

    pub(super) fn pending(&self, key: &str) -> Option<String> {
        PENDING_ROTATIONS
            .lock()
            .unwrap()
            .get(&(self.owner, key.to_string()))
            .map(|json| json.to_string())
    }

    /// File `token` as the pending rotation of `key`, filed by a connection
    /// that began at deletion count `generation`.
    pub(super) fn file(&self, key: &str, token: &str, generation: u64) {
        let mut chain: serde_json::Value = serde_json::from_str(&self.chain(token)).unwrap();
        chain[PENDING_GEN] = generation.into();
        PENDING_ROTATIONS.lock().unwrap().insert(
            (self.owner, key.to_string()),
            zeroize::Zeroizing::new(chain.to_string()),
        );
    }

    /// A provider of the first account bound to `profile`.
    pub(super) fn bound(&self, profile: &str) -> JottacloudProvider {
        let mut provider = JottacloudProvider::new(JottacloudConfig {
            login_token: SecretString::from(String::new()),
            device: "Jotta".into(),
            mountpoint: "Archive".into(),
            initial_path: None,
        })
        .with_profile_id(profile);
        provider.set_credential_user(Some(self.first));
        provider
    }

    /// The deletion count a connection of `profile` notes for its own key now.
    pub(super) fn noted(&self, profile: &str) -> Option<u64> {
        let mut provider = self.bound(profile);
        provider.bind_credential_owner().expect("bind");
        provider.note_chain_generations();
        let key = provider.refresh_token_account();
        let noted = provider
            .chain_generations
            .lock()
            .unwrap()
            .get(&key)
            .copied();
        noted
    }

    /// A login of `profile` that persists `token`: what its key holds then.
    pub(super) fn login(&self, profile: &str, token: &str) -> String {
        let mut login = self.bound(profile);
        login.bind_credential_owner().expect("bind");
        login.begin_login_chain();
        login.refresh_token = SecretString::from(token.to_string());
        login.token_endpoint = self.endpoint.clone();
        login.username = "jotta-user".into();
        login.persist_refresh_token();
        let key = login.refresh_token_account();
        self.stored(&key)
    }
}

impl Drop for Chains {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
        }
        drop(self.runtime.take());
        PENDING_ROTATIONS.lock().unwrap().clear();
        CredentialStore::clear_cache();
    }
}

/// Two deletions of one key overlap, each in another process: whichever
/// ends first, and whatever each outcome, the record settles once both
/// ended, shows one in progress while the other runs, and a rotation filed
/// between the two ends is dropped exactly when the later one succeeded.
#[test]
fn overlapping_deletions_settle_in_every_order() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::overlapping_deletions_settle_in_every_order",
    ) {
        return;
    }
    let chains = Chains::new();
    let mut failures = Vec::new();
    for (case, (first_done, second_done, first_ends_first)) in [
        ("failed,failed,begin order", (false, false, true)),
        ("failed,failed,reverse order", (false, false, false)),
        ("done,failed,begin order", (true, false, true)),
        ("done,failed,reverse order", (true, false, false)),
        ("failed,done,begin order", (false, true, true)),
        ("failed,done,reverse order", (false, true, false)),
        ("done,done,begin order", (true, true, true)),
        ("done,done,reverse order", (true, true, false)),
    ]
    .into_iter()
    .enumerate()
    .map(|(n, (case, outcome))| (format!("{n}: {case}"), outcome))
    {
        let profile = format!("overlap{}", &case[..1]);
        let key = format!("jottacloud_refresh_{profile}");
        let one = partitions::begin_chain_deletion(&chains.store, chains.owner, &key).unwrap();
        let two = partitions::begin_chain_deletion(&chains.store, chains.owner, &key).unwrap();
        let ((ends, ends_done), (last, last_done)) = if first_ends_first {
            ((one, first_done), (two, second_done))
        } else {
            ((two, second_done), (one, first_done))
        };
        partitions::end_chain_deletion(&chains.store, chains.owner, &key, ends, ends_done).unwrap();
        let between = partitions::chain_deletions(&chains.store, chains.owner, &key).unwrap();
        // A connection that begins now, and files its rotation.
        let noted = chains.noted(&profile).expect("the count is read");
        chains.file(&key, "filed-between", noted);
        partitions::end_chain_deletion(&chains.store, chains.owner, &key, last, last_done).unwrap();
        let after = partitions::chain_deletions(&chains.store, chains.owner, &key).unwrap();
        let loaded =
            JottacloudProvider::load_refresh_chain(Some(chains.first), Some(chains.owner), &key)
                .map(|((rt, _, _), _)| rt);
        if between.settled() {
            failures.push(format!("{case}: settled while one still runs: {between:?}"));
        }
        if !after.settled() {
            failures.push(format!("{case}: not settled once both ended: {after:?}"));
        }
        let expected = (!last_done).then(|| "filed-between".to_string());
        if loaded != expected {
            failures.push(format!(
                "{case}: the rotation filed between the ends: {loaded:?}, expected {expected:?}"
            ));
        }
        PENDING_ROTATIONS.lock().unwrap().clear();
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The test whose child plays another process: it begins a deletion of
/// `account` and never ends it, exiting at once or once its stdin closes.
const HOLDER: &str = "AEROFTP_TEST_CHAIN_DELETION_HOLDER";
const HOLDER_TEST: &str =
    "providers::jottacloud::chain_tests::a_deletion_its_process_left_unfinished_settles";
const HOLDER_BEGAN: &str = "chain-deletion-holder-began";

fn holder(account: &str, owner: Option<i64>, wait: bool) -> std::process::Child {
    let owner = owner.map_or_else(|| "none".to_string(), |o| o.to_string());
    let mode = if wait { "wait" } else { "exit" };
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", HOLDER_TEST, "--nocapture", "--test-threads=1"])
        .env(HOLDER, format!("{account}|{owner}|{mode}"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        let line = lines
            .next()
            .expect("the holder began its deletion")
            .unwrap();
        if line.contains(HOLDER_BEGAN) {
            break;
        }
    }
    child
}

fn run_holder(role: &str) -> ! {
    let mut parts = role.split('|');
    let (account, owner, mode) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let owner = (owner != "none").then(|| owner.parse::<i64>().unwrap());
    CredentialStore::unlock_with_master(PASSWORD, None).expect("unlock");
    let store = CredentialStore::from_cache().expect("cached vault");
    partitions::init_or_migrate_cli(&store).expect("init partitions");
    let _begun = partitions::begin_chain_deletion(&store, owner, account).expect("begin");
    println!("{HOLDER_BEGAN}");
    std::io::stdout().flush().unwrap();
    if mode == "wait" {
        let _ = std::io::stdin().lines().count();
    }
    // Never ended: the process is gone with its deletion in progress.
    std::process::exit(0)
}

/// A deletion its process left unfinished does not stop the key for ever:
/// one whose process exited, one whose process is killed once it began (the
/// key is held while it lives), one this process could not end, and one of
/// another machine once its lease ran out. One a reader settled while it ran
/// still counts when it ends.
#[test]
fn a_deletion_its_process_left_unfinished_settles() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(HOLDER_TEST) {
        return;
    }
    if let Ok(role) = std::env::var(HOLDER) {
        run_holder(&role);
    }
    let chains = Chains::new();
    let mut failures = Vec::new();

    // The process exited halfway through its delete.
    let mut crashed = holder("jottacloud_refresh_crashed", chains.owner, false);
    assert!(crashed.wait().unwrap().success());
    let after_crash = chains.login("crashed", "login-crashed");
    if after_crash != "login-crashed" {
        failures.push(format!(
            "a deletion its exited process left: the login wrote {after_crash:?}"
        ));
    }

    // The process is alive: its deletion holds the key; killed, it does not.
    let mut alive = holder("jottacloud_refresh_alive", chains.owner, true);
    let while_alive = chains.login("alive", "login-while-alive");
    if !while_alive.is_empty() {
        failures.push(format!(
            "a deletion its live process runs: the login wrote {while_alive:?}"
        ));
    }
    alive.kill().unwrap();
    alive.wait().unwrap();
    let after_kill = chains.login("alive", "login-after-kill");
    if after_kill != "login-after-kill" {
        failures.push(format!(
            "a deletion its killed process left: the login wrote {after_kill:?}"
        ));
    }

    // The delete succeeded, its end could not be recorded.
    partitions::CHAIN_DELETION_END_REFUSED.store(true, std::sync::atomic::Ordering::SeqCst);
    let unended = delete_refresh_chain::<String>(
        &chains.store,
        Some(chains.first),
        "jottacloud_refresh_unended",
        || Ok(()),
    );
    partitions::CHAIN_DELETION_END_REFUSED.store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(unended.is_ok(), "{unended:?}");
    let after_unended = chains.login("unended", "login-after-unended");
    if after_unended != "login-after-unended" {
        failures.push(format!(
            "a deletion this process could not end: the login wrote {after_unended:?}"
        ));
    }

    // Recorded on another machine sharing the data folder: held while its
    // lease runs, settled once it ran out.
    let elsewhere = |key: &str, age_ms: i64| {
        let at = chrono::Utc::now().timestamp_millis() - age_ms;
        partitions::set_chain_deletions_raw(
            &chains.store,
            chains.owner,
            key,
            &format!(
                r#"{{"generation":0,"running":[{{"id":"elsewhere","process":"elsewhere","space":"another machine","pid":1,"start":1,"at":{at}}}]}}"#
            ),
        )
        .unwrap();
    };
    elsewhere("jottacloud_refresh_remote", 0);
    let while_leased = chains.login("remote", "login-while-leased");
    if !while_leased.is_empty() {
        failures.push(format!(
            "a deletion on another machine, just begun: the login wrote {while_leased:?}"
        ));
    }
    elsewhere("jottacloud_refresh_remote", 11 * 60 * 1000);
    let after_lease = chains.login("remote", "login-after-lease");
    if after_lease != "login-after-lease" {
        failures.push(format!(
            "a deletion on another machine, its lease run out: the login wrote {after_lease:?}"
        ));
    }

    // A deletion a reader settled as left over while it still ran counts
    // as completed when it ends, whether it failed or not.
    let resettled = "jottacloud_refresh_resettled";
    for done in [false, true] {
        let before = partitions::chain_deletions(&chains.store, chains.owner, resettled)
            .unwrap()
            .generation;
        let running =
            partitions::begin_chain_deletion(&chains.store, chains.owner, resettled).unwrap();
        partitions::set_chain_deletions_raw(
            &chains.store,
            chains.owner,
            resettled,
            &format!(r#"{{"generation":{}}}"#, before + 1),
        )
        .unwrap();
        partitions::end_chain_deletion(&chains.store, chains.owner, resettled, running, done)
            .unwrap();
        let after = partitions::chain_deletions(&chains.store, chains.owner, resettled).unwrap();
        let expected = partitions::ChainDeletions {
            generation: before + 2,
            in_progress: 0,
        };
        if after != expected {
            failures.push(format!(
                "a deletion settled while it ran, ended done={done}: {after:?}, expected {expected:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
