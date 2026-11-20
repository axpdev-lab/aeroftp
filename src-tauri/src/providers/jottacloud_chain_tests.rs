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
                .chain()
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

/// A deletion of `deleted` by another process: recorded, deleted from the
/// vault, ended, without this process's clock (test only).
pub(super) fn delete_elsewhere(store: &CredentialStore, owner: Option<i64>, deleted: &str) {
    let deletion = partitions::begin_chain_deletion(store, owner, deleted).expect("begin");
    let _ = store.delete(deleted);
    partitions::end_chain_deletion(store, owner, deleted, deletion, true).expect("end");
}

/// When a deletion by another process lands around a rotation's write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Around {
    /// After the rotation's last check, right before the write.
    BeforeWrite,
    /// Right after the write.
    AfterWrite,
}

/// A key deleted by another process around a rotation's write of `at`
/// (test only): `(when, at, owner, deleted)`, taken once.
pub(super) static DELETE_AROUND_WRITE: Mutex<Option<(Around, String, Option<i64>, String)>> =
    Mutex::new(None);

/// Run the deletion [`DELETE_AROUND_WRITE`] holds for `when` and `account`.
pub(super) fn around_rotation_write(when: Around, account: &str) {
    let deletion = DELETE_AROUND_WRITE
        .lock()
        .unwrap()
        .take_if(|(w, at, _, _)| *w == when && at == account);
    if let Some((_, _, owner, deleted)) = deletion {
        let store = CredentialStore::from_cache().unwrap();
        delete_elsewhere(&store, owner, &deleted);
    }
}

/// A rotation whose deletion counts cannot be read when it is persisted is
/// kept, with what it needs to be checked later, and a connection whose
/// counts cannot be read before its exchange sends nothing and leaves the
/// login token alone.
#[test]
fn a_rotation_whose_deletions_cannot_be_read_is_kept_and_checked_later() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_rotation_whose_deletions_cannot_be_read_is_kept_and_checked_later",
    ) {
        return;
    }
    let chains = Chains::new();
    let mut failures = Vec::new();
    let own = "jottacloud_refresh_unread";
    let singleton = "jottacloud_refresh";

    // Loaded from the singleton; the counts cannot be read at the next
    // rotation: kept for both keys, written to neither.
    chains
        .store
        .store(singleton, &chains.chain("singleton-unread"))
        .unwrap();
    let mut connection = chains.bound("unread");
    assert!(chains
        .runtime()
        .block_on(connection.try_connect_with_refresh())
        .unwrap());
    let written = chains.stored(own);
    partitions::CHAIN_DELETIONS_REFUSED.store(true, SeqCst);
    connection.token_expiry = Instant::now();
    chains
        .runtime()
        .block_on(connection.refresh_if_needed())
        .expect("refresh");
    partitions::CHAIN_DELETIONS_REFUSED.store(false, SeqCst);
    let rotated = connection.refresh_token.expose_secret().to_string();
    drop(connection);
    for key in [own, singleton] {
        let kept = chains
            .pending(key)
            .map(|json| stored_refresh_token(Some(&json)));
        if kept.as_deref() != Some(rotated.as_str()) {
            failures.push(format!("{key}: kept {kept:?}, expected {rotated}"));
        }
        if chains.stored(key) != written {
            failures.push(format!("{key}: written while its counts were unreadable"));
        }
    }
    // Readable again and nothing deleted: handed out.
    let loaded =
        JottacloudProvider::load_refresh_chain(Some(chains.first), Some(chains.owner), own)
            .chain()
            .map(|((rt, _, _), _)| rt);
    if loaded.as_deref() != Some(rotated.as_str()) {
        failures.push(format!("the kept rotation is handed out: {loaded:?}"));
    }
    // The singleton it came from is signed out elsewhere: dropped for the
    // profile's key too.
    delete_elsewhere(&chains.store, chains.owner, singleton);
    let after_source_deleted =
        JottacloudProvider::load_refresh_chain(Some(chains.first), Some(chains.owner), own)
            .chain()
            .map(|((rt, _, _), _)| rt);
    if after_source_deleted.as_deref() == Some(rotated.as_str()) || chains.pending(own).is_some() {
        failures.push(format!(
            "the rotation of a signed-out source is dropped: {after_source_deleted:?}, {:?}",
            chains.pending(own)
        ));
    }
    PENDING_ROTATIONS.lock().unwrap().clear();

    // The counts cannot be read before the exchange: nothing is sent, and
    // the error does not fall through to the login token.
    let blind = "jottacloud_refresh_blind";
    chains
        .store
        .store(blind, &chains.chain("own-blind"))
        .unwrap();
    let sent_before = *chains.calls.lock().unwrap();
    partitions::CHAIN_DELETIONS_REFUSED.store(true, SeqCst);
    let mut connection = chains.bound("blind");
    let refused = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    partitions::CHAIN_DELETIONS_REFUSED.store(false, SeqCst);
    let sent = *chains.calls.lock().unwrap() - sent_before;
    if sent != 0 || refused.is_ok() {
        failures.push(format!(
            "an exchange without its counts: {sent} sent, {refused:?}"
        ));
    }
    let falls_back = matches!(refresh_or_login_token(refused), Ok(false));
    if falls_back {
        failures.push("the refused connection spends the login token".to_string());
    }
    drop(connection);
    let mut connection = chains.bound("blind");
    let readable = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    let now = chains.stored(blind);
    if !matches!(readable, Ok(true)) || !now.starts_with("rotated-") {
        failures.push(format!(
            "readable again, the chain is exchanged and written: {readable:?}, {now}"
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A key deleted by another process between a rotation's last check and its
/// write: the source the chain was loaded from stops every write, a
/// destination that held nothing is not written, on the row and the vault
/// paths alike. Deleted right after the write instead, what was written
/// before the deletion stays, and what the deletion removed stays removed.
#[test]
fn a_deletion_elsewhere_right_before_the_write_stops_it() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_deletion_elsewhere_right_before_the_write_stops_it",
    ) {
        return;
    }
    let chains = Chains::new();
    let mut failures = Vec::new();
    let singleton = "jottacloud_refresh";
    for (case, user) in [
        ("account", chains.first),
        ("vault only", partitions::LEGACY_ONLY_ACCOUNT),
    ] {
        let owner = chain_owner(Some(user)).expect("owner");
        // Read as the path under test reads it.
        let token = |key: &str| {
            if user == chains.first {
                chains.stored(key)
            } else {
                stored_refresh_token(chains.store.get(key).ok().as_deref())
            }
        };
        let pending = |key: &str| {
            PENDING_ROTATIONS
                .lock()
                .unwrap()
                .contains_key(&(owner, key.to_string()))
        };
        for when in [Around::BeforeWrite, Around::AfterWrite] {
            let profile = format!(
                "{}{}",
                if user == chains.first {
                    "racing"
                } else {
                    "racingvault"
                },
                if when == Around::BeforeWrite {
                    ""
                } else {
                    "after"
                }
            );
            let own = format!("jottacloud_refresh_{profile}");

            // The source is deleted elsewhere around the write of the
            // profile's key.
            chains
                .store
                .store(singleton, &chains.chain("singleton-racing"))
                .unwrap();
            let mut connection = chains.bound(&profile);
            connection.set_credential_user(Some(user));
            assert!(chains
                .runtime()
                .block_on(connection.try_connect_with_refresh())
                .unwrap());
            let own_before = token(&own);
            *DELETE_AROUND_WRITE.lock().unwrap() =
                Some((when, own.clone(), owner, singleton.to_string()));
            connection.token_expiry = Instant::now();
            chains
                .runtime()
                .block_on(connection.refresh_if_needed())
                .expect("refresh");
            *DELETE_AROUND_WRITE.lock().unwrap() = None;
            let rotated = connection.refresh_token.expose_secret().to_string();
            let expected = match when {
                Around::BeforeWrite => own_before.clone(),
                Around::AfterWrite => rotated.clone(),
            };
            let own_after = token(&own);
            let singleton_after = token(singleton);
            if own_after != expected || !singleton_after.is_empty() {
                failures.push(format!(
                    "{case}, {when:?}, source deleted: {own} {own_before} -> {own_after} (expected {expected}), singleton {singleton_after:?}"
                ));
            }
            if pending(&own) || pending(singleton) {
                failures.push(format!(
                    "{case}, {when:?}, source deleted: a rotation is kept"
                ));
            }
            drop(connection);
            PENDING_ROTATIONS.lock().unwrap().clear();

            // The profile's key, empty, is deleted elsewhere around the
            // write of its first chain: it ends up empty either way; the
            // singleton is written.
            chains
                .store
                .store(singleton, &chains.chain("singleton-empty"))
                .unwrap();
            let empty = format!("jottacloud_refresh_{profile}empty");
            let mut connection = chains.bound(&format!("{profile}empty"));
            connection.set_credential_user(Some(user));
            *DELETE_AROUND_WRITE.lock().unwrap() =
                Some((when, empty.clone(), owner, empty.clone()));
            assert!(chains
                .runtime()
                .block_on(connection.try_connect_with_refresh())
                .unwrap());
            *DELETE_AROUND_WRITE.lock().unwrap() = None;
            let rotated = connection.refresh_token.expose_secret().to_string();
            let empty_after = token(&empty);
            let singleton_after = token(singleton);
            if !empty_after.is_empty() || pending(&empty) {
                failures.push(format!(
                    "{case}, {when:?}, empty destination deleted: written {empty_after:?}, kept {}",
                    pending(&empty)
                ));
            }
            if singleton_after != rotated {
                failures.push(format!(
                    "{case}, {when:?}, empty destination deleted: the singleton holds {singleton_after}, expected {rotated}"
                ));
            }
            drop(connection);
            PENDING_ROTATIONS.lock().unwrap().clear();
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// What happens right after a load reads its key (test only): `(owner, key,
/// what)`, a deletion elsewhere once, or a new rotation filed at every read.
pub(super) enum AfterStoredRead {
    DeleteOnce,
    RefileEveryTime(u32),
}

pub(super) static AFTER_STORED_READ: Mutex<Option<(Option<i64>, String, AfterStoredRead)>> =
    Mutex::new(None);

pub(super) fn after_stored_read(owner: Option<i64>, account: &str) {
    let mut hook = AFTER_STORED_READ.lock().unwrap();
    let Some((o, key, what)) = hook.as_mut() else {
        return;
    };
    if *o != owner || key != account {
        return;
    }
    let store = CredentialStore::from_cache().unwrap();
    match what {
        AfterStoredRead::DeleteOnce => {
            let (o, key) = (*o, key.clone());
            *hook = None;
            drop(hook);
            delete_elsewhere(&store, o, &key);
        }
        AfterStoredRead::RefileEveryTime(n) => {
            *n += 1;
            let endpoint = stored_chain_endpoint(&store, key);
            let mut chain = serde_json::json!({
                "refresh_token": format!("refiled-{n}"),
                "token_endpoint": endpoint,
                "username": "jotta-user",
            });
            chain[PENDING_BASE] = stored_refresh_token(store.get(key).ok().as_deref()).into();
            chain[PENDING_GEN] = 0.into();
            PENDING_ROTATIONS.lock().unwrap().insert(
                (owner, key.clone()),
                zeroize::Zeroizing::new(chain.to_string()),
            );
        }
    }
}

fn stored_chain_endpoint(store: &CredentialStore, key: &str) -> String {
    store
        .get(key)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|v| v["token_endpoint"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// A load reads one consistent state of the key, its pending rotation and
/// its deletions: a deletion that completes while the key is read is not
/// paired with the chain it removed, and a pending rotation replaced at every
/// read sends nothing and leaves the login token alone.
#[test]
fn a_load_reads_the_key_its_rotation_and_its_deletions_together() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_load_reads_the_key_its_rotation_and_its_deletions_together",
    ) {
        return;
    }
    let chains = Chains::new();
    let mut failures = Vec::new();
    let _ = chains.store.delete("jottacloud_refresh");

    // Deleted elsewhere right after the key was read.
    let gone = "jottacloud_refresh_readgone";
    chains
        .store
        .store(gone, &chains.chain("stored-readgone"))
        .unwrap();
    let noted = chains.noted("readgone").expect("count");
    let mut filed: serde_json::Value =
        serde_json::from_str(&chains.chain("pending-readgone")).unwrap();
    filed[PENDING_BASE] = "stored-readgone".into();
    filed[PENDING_GEN] = noted.into();
    PENDING_ROTATIONS.lock().unwrap().insert(
        (chains.owner, gone.to_string()),
        zeroize::Zeroizing::new(filed.to_string()),
    );
    *AFTER_STORED_READ.lock().unwrap() =
        Some((chains.owner, gone.to_string(), AfterStoredRead::DeleteOnce));
    let sent_before = *chains.calls.lock().unwrap();
    let mut connection = chains.bound("readgone");
    let result = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    *AFTER_STORED_READ.lock().unwrap() = None;
    let sent = *chains.calls.lock().unwrap() - sent_before;
    if !matches!(result, Ok(false)) || sent != 0 {
        failures.push(format!(
            "a key deleted while it is read: {result:?}, {sent} exchange(s) sent"
        ));
    }
    drop(connection);
    PENDING_ROTATIONS.lock().unwrap().clear();

    // A rotation filed anew at every read of the key.
    let restless = "jottacloud_refresh_restless";
    chains
        .store
        .store(restless, &chains.chain("stored-restless"))
        .unwrap();
    let mut filed: serde_json::Value =
        serde_json::from_str(&chains.chain("pending-restless")).unwrap();
    filed[PENDING_BASE] = "stored-restless".into();
    filed[PENDING_GEN] = 0.into();
    PENDING_ROTATIONS.lock().unwrap().insert(
        (chains.owner, restless.to_string()),
        zeroize::Zeroizing::new(filed.to_string()),
    );
    *AFTER_STORED_READ.lock().unwrap() = Some((
        chains.owner,
        restless.to_string(),
        AfterStoredRead::RefileEveryTime(0),
    ));
    let sent_before = *chains.calls.lock().unwrap();
    let mut connection = chains.bound("restless");
    let result = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    *AFTER_STORED_READ.lock().unwrap() = None;
    let sent = *chains.calls.lock().unwrap() - sent_before;
    let error = format!("{result:?}");
    if sent != 0 || !matches!(refresh_or_login_token(result), Err(_)) {
        failures.push(format!(
            "a rotation filed anew at every read: {error}, {sent} exchange(s) sent"
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A login that follows a failed refresh begins its own chain then: a
/// delete of its key while the refresh was tried does not stop the login's
/// chain, one after the login began does.
#[test]
fn a_login_is_checked_against_deletions_from_when_it_began() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_login_is_checked_against_deletions_from_when_it_began",
    ) {
        return;
    }
    let chains = Chains::new();
    let mut failures = Vec::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let login_of = |profile: &str, delete_before_login: bool| {
        let key = format!("jottacloud_refresh_{profile}");
        let mut connection = chains.bound(profile);
        assert!(!chains
            .runtime()
            .block_on(connection.try_connect_with_refresh())
            .unwrap());
        let delete = || {
            delete_refresh_chain::<String>(&chains.store, Some(chains.first), &key, || Ok(()))
                .unwrap()
        };
        if delete_before_login {
            delete();
        }
        connection.begin_login_chain();
        if !delete_before_login {
            delete();
        }
        connection.refresh_token = SecretString::from(format!("login-{profile}"));
        connection.token_endpoint = chains.endpoint.clone();
        connection.username = "jotta-user".into();
        connection.persist_refresh_token();
        (chains.stored(&key), chains.pending(&key).is_some())
    };
    let before = login_of("relogin", true);
    if before != ("login-relogin".to_string(), false) {
        failures.push(format!("deleted before the login began: {before:?}"));
    }
    let after = login_of("deletedlogin", false);
    if after != (String::new(), false) {
        failures.push(format!("deleted after the login began: {after:?}"));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A login's setup, which may wait for another process to create the vault,
/// runs off the async worker: a task beside the connection still runs.
#[test]
fn a_login_waiting_for_the_vault_leaves_the_async_worker_free() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_login_waiting_for_the_vault_leaves_the_async_worker_free",
    ) {
        return;
    }
    let _env = crate::test_env::lock();
    let _partition = partitions::test_lock();
    let temporary = tempfile::TempDir::new().expect("temporary config");
    std::env::set_var("XDG_CONFIG_HOME", temporary.path());
    std::env::remove_var("AEROFTP_USER_PASSPHRASE");
    // Before the first initialization: the vault is created in master mode,
    // and no keyring slot is touched.
    std::env::set_var("AEROFTP_MASTER_PASSWORD", "jotta-worker-test-pass");
    CredentialStore::clear_cache();
    // Another process is creating the vault.
    let dir = crate::portable::credential_store_dir().expect("vault folder");
    std::fs::create_dir_all(&dir).unwrap();
    let creating = dir.join(".vault.lock");
    std::fs::write(&creating, b"").unwrap();
    let release = {
        let creating = creating.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            let _ = std::fs::remove_file(creating);
        })
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let ticked = runtime.block_on(async {
        let ticker = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        });
        let connection = tokio::spawn(async {
            let mut login = JottacloudProvider::new(JottacloudConfig {
                login_token: SecretString::from(String::new()),
                device: "Jotta".into(),
                mountpoint: "Archive".into(),
                initial_path: None,
            });
            let _ = login.connect().await;
        });
        let ticked = tokio::time::timeout(std::time::Duration::from_millis(400), ticker)
            .await
            .is_ok();
        let _ = connection.await;
        ticked
    });
    release.join().unwrap();
    drop(runtime);
    std::env::remove_var("AEROFTP_MASTER_PASSWORD");
    CredentialStore::clear_cache();
    assert!(
        ticked,
        "a task beside the connection ran while its login waited for the vault"
    );
}
