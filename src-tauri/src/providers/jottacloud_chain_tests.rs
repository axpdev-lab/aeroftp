//! How a Jottacloud refresh chain's deletion is recorded across processes,
//! and what a rotation writes, keeps and hands out around one.

use super::*;
use crate::credential_store::CredentialStore;
use crate::user_partitions as partitions;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

/// A partition with its first account, a vault open in master mode and a
/// token endpoint that answers every refresh with `rotated-<n>`, the n-th
/// call, and refuses a refresh token that begins with `refused` with
/// `invalid_grant`. Dropped in reverse order of the locks it took.
pub(super) struct Chains {
    pub(super) store: CredentialStore,
    pub(super) first: i64,
    pub(super) owner: Option<i64>,
    pub(super) endpoint: String,
    pub(super) calls: Arc<Mutex<usize>>,
    /// The refresh token each exchange submitted, in order.
    pub(super) submitted: Arc<Mutex<Vec<String>>>,
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
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let (endpoint, server) = runtime.block_on(async {
            let calls = calls.clone();
            let submitted = submitted.clone();
            let app = axum::Router::new().route(
                "/token",
                axum::routing::post(move |body: String| {
                    let calls = calls.clone();
                    let submitted = submitted.clone();
                    async move {
                        let token = body
                            .split('&')
                            .find_map(|pair| pair.strip_prefix("refresh_token="))
                            .and_then(|token| urlencoding::decode(token).ok())
                            .map(|token| token.into_owned())
                            .unwrap_or_default();
                        submitted.lock().unwrap().push(token.clone());
                        let call = {
                            let mut calls = calls.lock().unwrap();
                            *calls += 1;
                            *calls
                        };
                        if token.starts_with("refused") {
                            return (
                                axum::http::StatusCode::BAD_REQUEST,
                                axum::Json(serde_json::json!({"error": "invalid_grant"})),
                            );
                        }
                        (
                            axum::http::StatusCode::OK,
                            axum::Json(serde_json::json!({
                                "access_token": format!("access-{call}"),
                                "refresh_token": format!("rotated-{call}"),
                                "expires_in": 3600,
                            })),
                        )
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
            submitted,
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
        bound_provider(self.first, profile)
    }

    /// The refresh tokens submitted since `from` exchanges were.
    pub(super) fn submitted_since(&self, from: usize) -> Vec<String> {
        self.submitted.lock().unwrap()[from..].to_vec()
    }

    /// How many exchanges were submitted so far.
    pub(super) fn submissions(&self) -> usize {
        self.submitted.lock().unwrap().len()
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
        self.try_login(profile, token).expect("begin the login")
    }

    /// [`Self::login`], or the error its begin returned, which sends nothing.
    pub(super) fn try_login(&self, profile: &str, token: &str) -> Result<String, ProviderError> {
        let mut login = self.bound(profile);
        login.bind_credential_owner().expect("bind");
        login.begin_login_chain()?;
        login.refresh_token = SecretString::from(token.to_string());
        login.token_endpoint = self.endpoint.clone();
        login.username = "jotta-user".into();
        login.persist_refresh_token();
        let key = login.refresh_token_account();
        Ok(self.stored(&key))
    }
}

/// A provider of `user` bound to `profile`, with no login token.
pub(super) fn bound_provider(user: i64, profile: &str) -> JottacloudProvider {
    let mut provider = JottacloudProvider::new(JottacloudConfig {
        login_token: SecretString::from(String::new()),
        device: "Jotta".into(),
        mountpoint: "Archive".into(),
        initial_path: None,
    })
    .with_profile_id(profile);
    provider.set_credential_user(Some(user));
    provider
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
/// `account` and never ends it, exiting at once or once its stdin closes;
/// or it serves commands that end it (see [`run_holder`]).
const HOLDER: &str = "AEROFTP_TEST_CHAIN_DELETION_HOLDER";
const HOLDER_TEST: &str =
    "providers::jottacloud::chain_tests::a_deletion_its_process_left_unfinished_settles";
const HOLDER_BEGAN: &str = "chain-deletion-holder-began";
const HOLDER_DONE: &str = "chain-deletion-holder-done";

/// Another process holding a deletion of a key, killed when dropped.
pub(super) struct Holder {
    pub(super) child: std::process::Child,
    lines: std::io::Lines<std::io::BufReader<std::process::ChildStdout>>,
}

impl Holder {
    /// Have the holder run `command`, and wait until it did.
    pub(super) fn run(&mut self, command: &str) {
        let stdin = self.child.stdin.as_mut().expect("the holder's stdin");
        writeln!(stdin, "{command}").unwrap();
        stdin.flush().unwrap();
        self.wait_for(HOLDER_DONE);
    }

    fn wait_for(&mut self, marker: &str) {
        loop {
            let line = self
                .lines
                .next()
                .unwrap_or_else(|| panic!("the holder exited before {marker}"))
                .unwrap();
            if line.contains(marker) {
                return;
            }
        }
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a holder of a deletion of `account` for `owner`, as `user`, in
/// `mode` ([`run_holder`]), once its deletion began.
pub(super) fn holder(account: &str, owner: Option<i64>, user: i64, mode: &str) -> Holder {
    let owner = owner.map_or_else(|| "none".to_string(), |o| o.to_string());
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", HOLDER_TEST, "--nocapture", "--test-threads=1"])
        .env(HOLDER, format!("{account}|{owner}|{user}|{mode}"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let mut holder = Holder { child, lines };
    holder.wait_for(HOLDER_BEGAN);
    holder
}

/// Remove both copies of `account` a deletion by the legacy owner `user`
/// removes: the vault key and the account's row.
fn remove_both_copies(store: &CredentialStore, user: i64, account: &str) -> Result<(), String> {
    let _ = store.delete(account);
    partitions::unmirror_credential_for_user_best_effort_any(user, account);
    Ok(())
}

/// The holder child. It begins a deletion of `account` for `owner`, then,
/// by mode: `exit` at once; `wait` until its stdin closes; `serve` the
/// commands `end-failed` (the delete failed) and `end-done` (both copies
/// removed first, as `user`), one per line. In `unended` mode it deletes
/// both copies through [`delete_refresh_chain`] with an end that cannot be
/// recorded, and serves `note <profile>`: a connection of `user` for that
/// profile reads its deletion counts. A deletion `exit` and `wait` began is
/// never ended: the process is gone with it in progress.
fn run_holder(role: &str) -> ! {
    use std::sync::atomic::Ordering::SeqCst;
    let mut parts = role.split('|');
    let (account, owner, user, mode) = (
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
        parts.next().unwrap(),
    );
    let owner = (owner != "none").then(|| owner.parse::<i64>().unwrap());
    let user: i64 = user.parse().unwrap();
    CredentialStore::unlock_with_master(PASSWORD, None).expect("unlock");
    let store = CredentialStore::from_cache().expect("cached vault");
    partitions::init_or_migrate_cli(&store).expect("init partitions");
    let mut begun = None;
    if mode == "unended" {
        partitions::CHAIN_DELETION_END_REFUSED.store(true, SeqCst);
        delete_refresh_chain::<String>(&store, Some(user), account, || {
            remove_both_copies(&store, user, account)
        })
        .expect("delete");
        partitions::CHAIN_DELETION_END_REFUSED.store(false, SeqCst);
    } else {
        begun = Some(partitions::begin_chain_deletion(&store, owner, account).expect("begin"));
    }
    println!("{HOLDER_BEGAN}");
    std::io::stdout().flush().unwrap();
    match mode {
        "exit" => {}
        "wait" => {
            let _ = std::io::stdin().lines().count();
        }
        _ => {
            for line in std::io::stdin().lines() {
                let line = line.unwrap();
                let (command, argument) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                match command {
                    "end-failed" | "end-done" => {
                        let done = command == "end-done";
                        if done {
                            remove_both_copies(&store, user, account).unwrap();
                        }
                        let deletion = begun.take().expect("a deletion to end");
                        partitions::end_chain_deletion(&store, owner, account, deletion, done)
                            .expect("end");
                    }
                    "note" => {
                        let mut connection = bound_provider(user, argument);
                        connection.bind_credential_owner().expect("bind");
                        connection.note_chain_generations();
                    }
                    other => panic!("unknown holder command {other}"),
                }
                println!("{HOLDER_DONE}");
                std::io::stdout().flush().unwrap();
            }
        }
    }
    std::process::exit(0)
}

/// A deletion its process left unfinished does not stop the key for ever:
/// one whose process exited, one whose process is killed once it began (the
/// key is held while it lives: a login of it does not begin), one this
/// process could not end, and one of another machine once its lease ran out. One a reader settled while it ran
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
    let mut crashed = holder(
        "jottacloud_refresh_crashed",
        chains.owner,
        chains.first,
        "exit",
    );
    assert!(crashed.child.wait().unwrap().success());
    let after_crash = chains.login("crashed", "login-crashed");
    if after_crash != "login-crashed" {
        failures.push(format!(
            "a deletion its exited process left: the login wrote {after_crash:?}"
        ));
    }

    // The process is alive: its deletion holds the key; killed, it does not.
    let mut alive = holder(
        "jottacloud_refresh_alive",
        chains.owner,
        chains.first,
        "wait",
    );
    let while_alive = chains.try_login("alive", "login-while-alive");
    if !matches!(while_alive, Err(ProviderError::ConnectionFailed(_)))
        || !chains.stored("jottacloud_refresh_alive").is_empty()
    {
        failures.push(format!(
            "a deletion its live process runs: the login began: {while_alive:?}"
        ));
    }
    alive.child.kill().unwrap();
    alive.child.wait().unwrap();
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
    let while_leased = chains.try_login("remote", "login-while-leased");
    if !matches!(while_leased, Err(ProviderError::ConnectionFailed(_)))
        || !chains.stored("jottacloud_refresh_remote").is_empty()
    {
        failures.push(format!(
            "a deletion on another machine, just begun: the login began: {while_leased:?}"
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

/// A deletion of `deleted` by another process: recorded, its copies
/// removed, ended, without this process's clock (test only). The copies are
/// what a sign-out removes: for the legacy owner (`None`) the shared vault key
/// and the owner's row, for any other account its row alone.
pub(super) fn delete_elsewhere(store: &CredentialStore, owner: Option<i64>, deleted: &str) {
    let deletion = partitions::begin_chain_deletion(store, owner, deleted).expect("begin");
    match owner {
        Some(user) => partitions::unmirror_credential_for_user_best_effort_any(user, deleted),
        None => {
            let _ = store.delete(deleted);
            let conn = partitions::open_or_init_cli().expect("partition DB");
            for user in partitions::list_users(&conn).expect("accounts") {
                if chain_owner(Some(user.id)).expect("owner").is_none() {
                    partitions::unmirror_credential_for_user_best_effort_any(user.id, deleted);
                }
            }
        }
    }
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
pub(super) static DELETE_AROUND_WRITE: Mutex<Option<DeleteAroundWrite>> = Mutex::new(None);

type DeleteAroundWrite = (Around, String, Option<i64>, String);

/// A holder ([`holder`]) started around a rotation's write of `at` (test
/// only): `(when, at, owner, user, held)`, taken once; the holder is kept in
/// [`HELD`].
pub(super) static HOLD_AROUND_WRITE: Mutex<Option<HoldAroundWrite>> = Mutex::new(None);

type HoldAroundWrite = (Around, String, Option<i64>, i64, String);

/// The holder [`HOLD_AROUND_WRITE`] started.
pub(super) static HELD: Mutex<Option<Holder>> = Mutex::new(None);

/// Run the deletion [`DELETE_AROUND_WRITE`] holds for `when` and `account`,
/// and start the holder [`HOLD_AROUND_WRITE`] holds for them.
pub(super) fn around_rotation_write(when: Around, account: &str) {
    let deletion = DELETE_AROUND_WRITE
        .lock()
        .unwrap()
        .take_if(|(w, at, _, _)| *w == when && at == account);
    if let Some((_, _, owner, deleted)) = deletion {
        let store = CredentialStore::from_cache().unwrap();
        delete_elsewhere(&store, owner, &deleted);
    }
    let hold = HOLD_AROUND_WRITE
        .lock()
        .unwrap()
        .take_if(|(w, at, _, _, _)| *w == when && at == account);
    if let Some((_, _, owner, user, held)) = hold {
        *HELD.lock().unwrap() = Some(holder(&held, owner, user, "serve"));
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
/// paths alike, and for an account that is not the legacy owner, whose
/// chains live in its rows alone. Deleted right after the write instead,
/// what was written before the deletion stays, what the deletion removed
/// stays removed, and nothing is kept for a key whose source was deleted.
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
    let private = {
        let mut conn = partitions::open_or_init_cli().expect("partition DB");
        let root_key = chains.store.derive_user_partition_wrapping_key();
        partitions::create_user(&mut conn, &root_key, "Private", None, None, None)
            .expect("a second account")
            .id
    };
    for (case, user) in [
        ("account", chains.first),
        ("vault only", partitions::LEGACY_ONLY_ACCOUNT),
        ("another account", private),
    ] {
        let owner = chain_owner(Some(user)).expect("owner");
        // Read as the path under test reads it.
        let token = |key: &str| {
            if user == partitions::LEGACY_ONLY_ACCOUNT {
                stored_refresh_token(chains.store.get(key).ok().as_deref())
            } else {
                stored_refresh_token(
                    partitions::resolve_credential_for_user(&chains.store, user, key)
                        .unwrap()
                        .as_deref()
                        .map(|value| value.as_str()),
                )
            }
        };
        // The singleton, where this account reads it.
        let store_singleton = |token: &str| {
            if user == private {
                partitions::store_credential_for_user(
                    &chains.store,
                    private,
                    singleton,
                    "jottacloud_refresh",
                    &chains.chain(token),
                )
                .unwrap();
            } else {
                chains.store.store(singleton, &chains.chain(token)).unwrap();
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
                match case {
                    "account" => "racing",
                    "vault only" => "racingvault",
                    _ => "racingprivate",
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
            store_singleton("singleton-racing");
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
            // singleton is written, or, for an account that is not the
            // legacy owner, which never writes it, the rotation is kept
            // for it.
            store_singleton("singleton-empty");
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
            if user == private {
                let kept =
                    JottacloudProvider::load_refresh_chain(Some(user), Some(owner), singleton)
                        .chain()
                        .map(|((rt, _, _), _)| rt);
                if singleton_after != "singleton-empty" || kept.as_deref() != Some(&rotated) {
                    failures.push(format!(
                        "{case}, {when:?}, empty destination deleted: the singleton holds {singleton_after} and hands out {kept:?}, expected singleton-empty and {rotated}"
                    ));
                }
            } else if singleton_after != rotated {
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
    if sent != 0 || refresh_or_login_token(result).is_ok() {
        failures.push(format!(
            "a rotation filed anew at every read: {error}, {sent} exchange(s) sent"
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A key deleted by another process right after a login noted its keys'
/// deletion counts (test only): `(profile, owner, key)`, taken once.
pub(super) static DELETED_AFTER_LOGIN_NOTE: Mutex<Option<(String, Option<i64>, String)>> =
    Mutex::new(None);

pub(super) fn after_login_note(profile: &str) {
    let deleted = DELETED_AFTER_LOGIN_NOTE
        .lock()
        .unwrap()
        .take_if(|(at, _, _)| at == profile);
    if let Some((_, owner, key)) = deleted {
        delete_elsewhere(&CredentialStore::from_cache().unwrap(), owner, &key);
    }
}

/// A deletion another process completes between a login's count read and
/// its check stops the login before its token is sent: `connect` returns a
/// retryable error with no OIDC discovery, since the chain the login starts
/// would be kept nowhere. The retry begins again and writes its chain.
#[test]
fn a_login_whose_key_was_just_deleted_sends_nothing() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_login_whose_key_was_just_deleted_sends_nothing",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let profile = "justdeleted";
    let key = format!("jottacloud_refresh_{profile}");
    let mut login = JottacloudProvider::new(JottacloudConfig {
        login_token: SecretString::from(unanswered_login_token()),
        device: "Jotta".into(),
        mountpoint: "Archive".into(),
        initial_path: None,
    })
    .with_profile_id(profile);
    login.set_credential_user(Some(chains.first));
    *DELETED_AFTER_LOGIN_NOTE.lock().unwrap() =
        Some((profile.to_string(), chains.owner, key.clone()));
    let (sent, discovered) = (chains.submissions(), super::tests::DISCOVERIES.load(SeqCst));
    let connected = chains.runtime().block_on(login.connect());
    *DELETED_AFTER_LOGIN_NOTE.lock().unwrap() = None;
    let mut failures = Vec::new();
    if !matches!(&connected, Err(ProviderError::ConnectionFailed(m)) if m.contains("just completed"))
    {
        failures.push(format!(
            "the login stops with a retryable error: {connected:?}"
        ));
    }
    let discoveries = super::tests::DISCOVERIES.load(SeqCst) - discovered;
    if discoveries != 0 || !chains.submitted_since(sent).is_empty() {
        failures.push(format!(
            "nothing is sent: {discoveries} discovery(ies), exchanges {:?}",
            chains.submitted_since(sent)
        ));
    }
    // The retry begins again, on the counts it reads now.
    let retried = chains.login(profile, "login-justdeleted");
    if retried != "login-justdeleted" {
        failures.push(format!("the retry writes its chain: {retried:?}"));
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
        connection.begin_login_chain().expect("begin the login");
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
    let began = std::time::Instant::now();
    let release = {
        let creating = creating.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(800));
            let _ = std::fs::remove_file(creating);
            std::time::Instant::now()
        })
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (ticked, connected) = runtime.block_on(async {
        // When the task beside the connection ran: a timeout alone cannot
        // tell, since a blocked worker drives no timer, and the ticker and
        // its timeout come due together once it is free again.
        let ticker = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            std::time::Instant::now()
        });
        let connection = tokio::spawn(async {
            let mut login = JottacloudProvider::new(JottacloudConfig {
                login_token: SecretString::from(String::new()),
                device: "Jotta".into(),
                mountpoint: "Archive".into(),
                initial_path: None,
            });
            let result = login.connect().await;
            (result, std::time::Instant::now())
        });
        (ticker.await, connection.await)
    });
    let released = release.join().unwrap();
    drop(runtime);
    std::env::remove_var("AEROFTP_MASTER_PASSWORD");
    CredentialStore::clear_cache();
    let ticked = ticked.expect("the ticker did not panic");
    assert!(
        ticked < released,
        "a task beside the connection ran while its login waited for the vault: ticked {:?} after the start, released {:?}",
        ticked - began,
        released - began
    );
    let (result, ended) = connected.expect("the connection task did not panic");
    // The empty login token is decoded only once the login's setup is over:
    // its error proves the connection got past it.
    let error = format!("{:?}", result.err());
    assert!(
        error.contains("Invalid login token"),
        "the connection stopped before the login token was read: {error}"
    );
    assert!(
        ended >= released,
        "the login's setup waited for the vault: ended {:?} after the start, released {:?}",
        ended - began,
        released - began
    );
}

/// The legacy owner's chain has two copies, its row and the shared vault key,
/// and a rotation writes one and has the other follow. When the other does
/// not follow, the rotation stays pending for the reader of that copy, whose
/// key still holds the chain Jotta consumed, and the reader of the written
/// copy loads the same chain, also after a second rotation the other copy
/// does not follow; the next rotation brings the other copy along. Both
/// directions: the row after a vault-only write, the vault after a row
/// write.
#[test]
fn a_copy_that_did_not_follow_keeps_the_rotation_for_its_reader() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_copy_that_did_not_follow_keeps_the_rotation_for_its_reader",
    ) {
        return;
    }
    let chains = Chains::new();
    assert_eq!(
        chains.owner, None,
        "the first account owns the shared vault"
    );
    let _ = chains.store.delete("jottacloud_refresh");
    let mut failures = Vec::new();
    let row = |key: &str| chains.stored(key);
    let vault = |key: &str| stored_refresh_token(chains.store.get(key).ok().as_deref());
    for (case, user, other) in [
        (
            "the row after a vault write",
            partitions::LEGACY_ONLY_ACCOUNT,
            chains.first,
        ),
        (
            "the vault after a row write",
            chains.first,
            partitions::LEGACY_ONLY_ACCOUNT,
        ),
    ] {
        let profile = if user == chains.first {
            "followvault"
        } else {
            "followrow"
        };
        let key = format!("jottacloud_refresh_{profile}");
        // The copy this connection writes, and the one that follows it.
        let written = |key: &str| {
            if user == chains.first {
                row(key)
            } else {
                vault(key)
            }
        };
        let follower = |key: &str| {
            if user == chains.first {
                vault(key)
            } else {
                row(key)
            }
        };
        // Both copies: the owner's row and the shared vault key.
        let both = chains.chain(&format!("both-{profile}"));
        chains.store.store(&key, &both).unwrap();
        partitions::store_imported_credential_for_user(
            &chains.store,
            chains.first,
            &key,
            Some("jottacloud_refresh"),
            &both,
        )
        .unwrap();
        let mut connection = chains.bound(profile);
        connection.set_credential_user(Some(user));
        assert!(chains
            .runtime()
            .block_on(connection.try_connect_with_refresh())
            .unwrap());
        let first = connection.refresh_token.expose_secret().to_string();
        if written(&key) != first || follower(&key) != first {
            failures.push(format!(
                "{case}: both copies follow a rotation: {} / {}, expected {first}",
                written(&key),
                follower(&key)
            ));
        }

        // Two rotations in a row whose other copy does not follow: the copy
        // left behind still holds the chain the first one consumed.
        for refused in 1..=2 {
            partitions::FOLLOW_REFUSED.store(true, SeqCst);
            connection.token_expiry = Instant::now();
            chains
                .runtime()
                .block_on(connection.refresh_if_needed())
                .expect("refresh");
            partitions::FOLLOW_REFUSED.store(false, SeqCst);
            let latest = connection.refresh_token.expose_secret().to_string();
            // The reader of the written copy first: it must not drop what the
            // reader of the other one needs.
            for (reader, reader_user) in [("written copy", user), ("copy left behind", other)] {
                let loaded = JottacloudProvider::load_refresh_chain(
                    Some(reader_user),
                    Some(chains.owner),
                    &key,
                )
                .chain()
                .map(|((rt, _, _), _)| rt);
                if loaded.as_deref() != Some(latest.as_str()) {
                    failures.push(format!(
                        "{case}, refused follow {refused}: the reader of the {reader} loads {loaded:?}, expected {latest}"
                    ));
                }
            }
        }

        connection.token_expiry = Instant::now();
        chains
            .runtime()
            .block_on(connection.refresh_if_needed())
            .expect("refresh");
        let third = connection.refresh_token.expose_secret().to_string();
        if written(&key) != third || follower(&key) != third || chains.pending(&key).is_some() {
            failures.push(format!(
                "{case}: the next rotation writes both copies: {} / {}, expected {third}, kept {}",
                written(&key),
                follower(&key),
                chains.pending(&key).is_some()
            ));
        }
        drop(connection);
        PENDING_ROTATIONS.lock().unwrap().clear();
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A profile purge names every key it found and could not remove: a refresh
/// chain whose deletion cannot be recorded stays in the vault, and the purge
/// says so instead of reporting a clean delete. Purged again once it can be
/// recorded, the key goes.
#[test]
fn a_purge_names_the_keys_it_could_not_remove() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_purge_names_the_keys_it_could_not_remove",
    ) {
        return;
    }
    let chains = Chains::new();
    let key = "jottacloud_refresh_purged";
    partitions::store_active_credential_dual(&chains.store, key, &chains.chain("purged-chain"))
        .unwrap();
    let purge = || {
        let purged = chains
            .runtime()
            .block_on(crate::purge_profile_secrets(
                "purged".into(),
                Some("jottacloud".into()),
                None,
            ))
            .expect("purge");
        serde_json::to_value(purged).unwrap()
    };
    partitions::CHAIN_DELETION_BEGIN_REFUSED.store(true, SeqCst);
    let refused = purge();
    partitions::CHAIN_DELETION_BEGIN_REFUSED.store(false, SeqCst);
    let mut failures = Vec::new();
    if refused["failed"] != serde_json::json!([key]) || chains.stored(key) != "purged-chain" {
        failures.push(format!(
            "a key left behind is named: {refused}, the key holds {:?}",
            chains.stored(key)
        ));
    }
    let purged = purge();
    if purged["failed"] != serde_json::json!([])
        || purged["removed"] != serde_json::json!([key])
        || !chains.stored(key).is_empty()
    {
        failures.push(format!(
            "purged again, the key goes: {purged}, the key holds {:?}",
            chains.stored(key)
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A rotation does not write a key another connection's rotation is pending
/// for: that rotation is the key's live chain, filed on what the key holds,
/// and stays usable; the other keys are written as usual.
#[test]
fn a_rotation_leaves_a_key_another_connection_filed_for() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_rotation_leaves_a_key_another_connection_filed_for",
    ) {
        return;
    }
    let chains = Chains::new();
    let singleton = "jottacloud_refresh";
    let own = "jottacloud_refresh_filedfor";
    chains
        .store
        .store(singleton, &chains.chain("singleton-filedfor"))
        .unwrap();
    let mut connection = chains.bound("filedfor");
    assert!(chains
        .runtime()
        .block_on(connection.try_connect_with_refresh())
        .unwrap());
    let own_before = chains.stored(own);
    // Another connection of this process filed for the profile's key, on
    // what the key holds now.
    let noted = chains.noted("filedfor").expect("count");
    let mut filed: serde_json::Value = serde_json::from_str(&chains.chain("foreign-b")).unwrap();
    filed[PENDING_BASE] = own_before.clone().into();
    filed[PENDING_GEN] = noted.into();
    PENDING_ROTATIONS.lock().unwrap().insert(
        (chains.owner, own.to_string()),
        zeroize::Zeroizing::new(filed.to_string()),
    );
    connection.token_expiry = Instant::now();
    chains
        .runtime()
        .block_on(connection.refresh_if_needed())
        .expect("refresh");
    let rotated = connection.refresh_token.expose_secret().to_string();
    drop(connection);
    let mut failures = Vec::new();
    if chains.stored(own) != own_before {
        failures.push(format!(
            "the key is left as it is: {own_before} -> {}",
            chains.stored(own)
        ));
    }
    let kept = chains
        .pending(own)
        .map(|json| stored_refresh_token(Some(&json)));
    if kept.as_deref() != Some("foreign-b") {
        failures.push(format!("the other connection's rotation stays: {kept:?}"));
    }
    if chains.stored(singleton) != rotated {
        failures.push(format!(
            "the other key is written: {}, expected {rotated}",
            chains.stored(singleton)
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A key signed out elsewhere and given a new chain right after a connection
/// read its keys' deletion counts (test only): `(profile, owner, key, the new
/// chain)`, taken once.
pub(super) static REPLACED_AFTER_NOTE: Mutex<Option<ReplacedAfterNote>> = Mutex::new(None);

type ReplacedAfterNote = (String, Option<i64>, String, String);

/// A key signed out in this process (its clock moves) and given a new chain
/// right after a connection read its keys' deletion counts (test only):
/// `(profile, user, key, the new chain)`, taken once.
pub(super) static DELETED_HERE_AFTER_NOTE: Mutex<Option<(String, i64, String, String)>> =
    Mutex::new(None);

pub(super) fn after_note(profile: &str) {
    let replaced = REPLACED_AFTER_NOTE
        .lock()
        .unwrap()
        .take_if(|(at, _, _, _)| at == profile);
    if let Some((_, owner, key, chain)) = replaced {
        let store = CredentialStore::from_cache().unwrap();
        delete_elsewhere(&store, owner, &key);
        store.store(&key, &chain).unwrap();
    }
    let here = DELETED_HERE_AFTER_NOTE
        .lock()
        .unwrap()
        .take_if(|(at, _, _, _)| at == profile);
    if let Some((_, user, key, chain)) = here {
        let store = CredentialStore::from_cache().unwrap();
        delete_refresh_chain::<String>(&store, Some(user), &key, || {
            remove_both_copies(&store, user, &key)
        })
        .expect("delete here");
        store.store(&key, &chain).unwrap();
    }
}

/// A key signed out and given a new chain between a connection's first count
/// reads and its load: the new chain is loaded, and its rotation is written,
/// checked against the count the chain was read with, not the earlier one.
#[test]
fn a_chain_stored_again_before_its_load_is_rotated() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_chain_stored_again_before_its_load_is_rotated",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let key = "jottacloud_refresh_storedagain";
    chains
        .store
        .store(key, &chains.chain("signed-out"))
        .unwrap();
    *REPLACED_AFTER_NOTE.lock().unwrap() = Some((
        "storedagain".to_string(),
        chains.owner,
        key.to_string(),
        chains.chain("stored-again"),
    ));
    let mut connection = chains.bound("storedagain");
    let from = chains.submissions();
    let connected = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    *REPLACED_AFTER_NOTE.lock().unwrap() = None;
    let rotated = connection.refresh_token.expose_secret().to_string();
    drop(connection);
    assert!(matches!(connected, Ok(true)), "{connected:?}");
    assert_eq!(
        chains.submitted_since(from),
        ["stored-again"],
        "the exchange sends the new chain"
    );
    assert_eq!(
        (chains.stored(key), chains.pending(key).is_some()),
        (rotated, false),
        "the new chain's rotation is written"
    );
}

/// The same, signed out in this process: its delete moves this process's
/// clock, not only the count. The connection counts that key's local deletes
/// from its load, so the restored chain's rotation is written, not dropped
/// as if the delete had removed it.
#[test]
fn a_chain_restored_here_before_its_load_is_rotated() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_chain_restored_here_before_its_load_is_rotated",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let key = "jottacloud_refresh_restoredhere";
    chains
        .store
        .store(key, &chains.chain("signed-out-here"))
        .unwrap();
    *DELETED_HERE_AFTER_NOTE.lock().unwrap() = Some((
        "restoredhere".to_string(),
        chains.first,
        key.to_string(),
        chains.chain("restored-here"),
    ));
    let mut connection = chains.bound("restoredhere");
    let from = chains.submissions();
    let connected = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    *DELETED_HERE_AFTER_NOTE.lock().unwrap() = None;
    let rotated = connection.refresh_token.expose_secret().to_string();
    drop(connection);
    assert!(matches!(connected, Ok(true)), "{connected:?}");
    assert_eq!(
        chains.submitted_since(from),
        ["restored-here"],
        "the exchange sends the restored chain"
    );
    assert_eq!(
        (chains.stored(key), chains.pending(key).is_some()),
        (rotated, false),
        "the restored chain's rotation is written, not dropped as deleted"
    );
}

/// What the rotation kept for `key` holds: its refresh token and the deletion
/// count its connection began with.
fn kept_rotation(chains: &Chains, key: &str) -> Option<(String, Option<u64>)> {
    chains.pending(key).map(|json| {
        let filed: serde_json::Value = serde_json::from_str(&json).unwrap();
        (
            filed["refresh_token"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            filed[PENDING_GEN].as_u64(),
        )
    })
}

/// The refresh token `load_refresh_chain` hands out for the first account's
/// `key`, if any.
fn handed_out(chains: &Chains, key: &str) -> Option<String> {
    JottacloudProvider::load_refresh_chain(Some(chains.first), Some(chains.owner), key)
        .chain()
        .map(|((rt, _, _), _)| rt)
}

/// A deletion of a key that another process is still running when a
/// rotation is persisted: the rotation is neither written nor dropped, and
/// stays kept on the deletion count its connection began with. The deletion
/// fails: the rotation is handed out. It succeeds: the rotation is dropped,
/// and the key reads as nothing. The same for a deletion that begins right
/// before the write, after the rotation's checks.
#[test]
fn a_rotation_waits_for_a_deletion_still_running_elsewhere() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_rotation_waits_for_a_deletion_still_running_elsewhere",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let mut failures = Vec::new();
    for (case, before_write, done) in [
        ("running at the checks, then failed", false, false),
        ("running at the checks, then done", false, true),
        ("begun right before the write, then failed", true, false),
        ("begun right before the write, then done", true, true),
    ] {
        let profile = format!(
            "held{}{}",
            if before_write { "write" } else { "checks" },
            if done { "done" } else { "failed" }
        );
        let key = format!("jottacloud_refresh_{profile}");
        chains
            .store
            .store(&key, &chains.chain(&format!("held-{profile}")))
            .unwrap();
        let mut connection = chains.bound(&profile);
        assert!(chains
            .runtime()
            .block_on(connection.try_connect_with_refresh())
            .unwrap());
        let written = chains.stored(&key);
        let baseline = partitions::chain_deletions(&chains.store, chains.owner, &key)
            .unwrap()
            .generation;
        let started = if before_write {
            *HOLD_AROUND_WRITE.lock().unwrap() = Some((
                Around::BeforeWrite,
                key.clone(),
                chains.owner,
                chains.first,
                key.clone(),
            ));
            None
        } else {
            Some(holder(&key, chains.owner, chains.first, "serve"))
        };
        connection.token_expiry = Instant::now();
        chains
            .runtime()
            .block_on(connection.refresh_if_needed())
            .expect("refresh");
        *HOLD_AROUND_WRITE.lock().unwrap() = None;
        let mut held = started
            .or_else(|| HELD.lock().unwrap().take())
            .expect("a deletion running elsewhere");
        let rotated = connection.refresh_token.expose_secret().to_string();
        drop(connection);
        let kept = kept_rotation(&chains, &key);
        if chains.stored(&key) != written || kept != Some((rotated.clone(), Some(baseline))) {
            failures.push(format!(
                "{case}: while the deletion runs, the key holds {} (expected {written}) and keeps {kept:?} (expected {rotated} at {baseline})",
                chains.stored(&key)
            ));
        }
        held.run(if done { "end-done" } else { "end-failed" });
        let loaded = handed_out(&chains, &key);
        let expected = (!done).then(|| rotated.clone());
        if loaded != expected {
            failures.push(format!(
                "{case}: once the deletion ended, the key hands out {loaded:?}, expected {expected:?}"
            ));
        }
        if done && chains.pending(&key).is_some() {
            failures.push(format!(
                "{case}: the rotation is kept after its key was deleted"
            ));
        }
        drop(held);
        PENDING_ROTATIONS.lock().unwrap().clear();
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A stored chain whose deletion another process is running is not
/// exchanged: the connection stops before any request, and not toward the
/// login token. Once that deletion failed, the chain is exchanged.
#[test]
fn a_stored_chain_is_not_exchanged_while_its_deletion_runs() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_stored_chain_is_not_exchanged_while_its_deletion_runs",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let mut failures = Vec::new();
    let key = "jottacloud_refresh_running";
    chains.store.store(key, &chains.chain("running-x")).unwrap();
    let mut held = holder(key, chains.owner, chains.first, "serve");
    let from = chains.submissions();
    let mut connection = chains.bound("running");
    let refused = refresh_or_login_token(
        chains
            .runtime()
            .block_on(connection.try_connect_with_refresh()),
    );
    let sent = chains.submitted_since(from);
    if !matches!(refused, Err(ProviderError::ConnectionFailed(_))) || !sent.is_empty() {
        failures.push(format!(
            "while the deletion runs: {refused:?}, sent {sent:?}"
        ));
    }
    drop(connection);
    held.run("end-failed");
    let from = chains.submissions();
    let mut connection = chains.bound("running");
    let connected = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    let sent = chains.submitted_since(from);
    let now = chains.stored(key);
    if !matches!(connected, Ok(true)) || sent != ["running-x"] || !now.starts_with("rotated-") {
        failures.push(format!(
            "once the deletion failed: {connected:?}, sent {sent:?}, the key holds {now}"
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A rotation kept for a key is not passed over for the login token while
/// it cannot be handed out: while the key's deletions cannot be read, or a
/// deletion of it runs elsewhere, the connection stops before any request
/// and the rotation stays. Once it can be checked, it is the chain sent.
#[test]
fn a_withheld_rotation_does_not_fall_through_to_the_login() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_withheld_rotation_does_not_fall_through_to_the_login",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let mut failures = Vec::new();
    for (case, running) in [("unreadable", false), ("running", true)] {
        let profile = format!("withheld{case}");
        let key = format!("jottacloud_refresh_{profile}");
        chains
            .store
            .store(&key, &chains.chain(&format!("stored-{case}")))
            .unwrap();
        let noted = chains.noted(&profile).expect("count");
        let mut filed: serde_json::Value =
            serde_json::from_str(&chains.chain(&format!("pending-{case}"))).unwrap();
        filed[PENDING_BASE] = format!("stored-{case}").into();
        filed[PENDING_GEN] = noted.into();
        PENDING_ROTATIONS.lock().unwrap().insert(
            (chains.owner, key.clone()),
            zeroize::Zeroizing::new(filed.to_string()),
        );
        let held = running.then(|| holder(&key, chains.owner, chains.first, "serve"));
        partitions::CHAIN_DELETIONS_REFUSED.store(!running, SeqCst);
        let from = chains.submissions();
        let mut connection = chains.bound(&profile);
        let refused = refresh_or_login_token(
            chains
                .runtime()
                .block_on(connection.try_connect_with_refresh()),
        );
        partitions::CHAIN_DELETIONS_REFUSED.store(false, SeqCst);
        let sent = chains.submitted_since(from);
        if !matches!(refused, Err(ProviderError::ConnectionFailed(_)))
            || !sent.is_empty()
            || chains.pending(&key).is_none()
        {
            failures.push(format!(
                "{case}: {refused:?}, sent {sent:?}, kept {}",
                chains.pending(&key).is_some()
            ));
        }
        drop(connection);
        if let Some(mut held) = held {
            held.run("end-failed");
        }
        let from = chains.submissions();
        let mut connection = chains.bound(&profile);
        let connected = chains
            .runtime()
            .block_on(connection.try_connect_with_refresh());
        let sent = chains.submitted_since(from);
        if !matches!(connected, Ok(true)) || sent != [format!("pending-{case}")] {
            failures.push(format!(
                "{case}, checked again: {connected:?}, sent {sent:?}"
            ));
        }
        drop(connection);
        PENDING_ROTATIONS.lock().unwrap().clear();
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A Personal Login Token whose fields point at nothing that answers: a
/// login that sends it fails at its OIDC discovery.
fn unanswered_login_token() -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(
        serde_json::json!({
            "username": "jotta-user",
            "auth_token": "login-auth",
            "wellKnownLink": "https://127.0.0.1:1/.well-known/openid-configuration",
        })
        .to_string(),
    )
}

/// A login that could not keep its chain spends nothing: while its key's
/// deletions cannot be read, its key cannot be read, or a deletion of its key
/// runs elsewhere, `connect` stops with a retryable error before the login
/// token is sent, and so does the login's begin. Once all is readable and
/// settled, the login begins and its chain is written.
#[test]
fn a_login_that_could_not_keep_its_chain_spends_nothing() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_login_that_could_not_keep_its_chain_spends_nothing",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let mut failures = Vec::new();
    for (case, refused) in [
        (
            "its deletions not readable",
            Some(&partitions::CHAIN_DELETIONS_REFUSED),
        ),
        ("its key not readable", Some(&super::tests::READ_REFUSED)),
        ("a deletion of its key running", None),
    ] {
        let profile = format!("blindlogin{}", case.len());
        let key = format!("jottacloud_refresh_{profile}");
        let mut login = JottacloudProvider::new(JottacloudConfig {
            login_token: SecretString::from(unanswered_login_token()),
            device: "Jotta".into(),
            mountpoint: "Archive".into(),
            initial_path: None,
        })
        .with_profile_id(&profile);
        login.set_credential_user(Some(chains.first));
        let mut held = match refused {
            Some(refused) => {
                refused.store(true, SeqCst);
                None
            }
            None => Some(holder(&key, chains.owner, chains.first, "serve")),
        };
        let from = chains.submissions();
        let connected = chains.runtime().block_on(login.connect());
        let mut direct = bound_provider(chains.first, &profile);
        direct.bind_credential_owner().expect("bind");
        let begun = direct.begin_login_chain();
        if let Some(refused) = refused {
            refused.store(false, SeqCst);
        }
        if let Some(held) = held.as_mut() {
            held.run("end-failed");
        }
        if !matches!(begun, Err(ProviderError::ConnectionFailed(_))) {
            failures.push(format!("{case}: the login began: {begun:?}"));
        }
        let sent = chains.submitted_since(from);
        if !matches!(connected, Err(ProviderError::ConnectionFailed(_))) || !sent.is_empty() {
            failures.push(format!("{case}: {connected:?}, sent {sent:?}"));
        }
        let written = chains.login(&profile, &format!("login-{profile}"));
        if written != format!("login-{profile}") {
            failures.push(format!("{case}, then settled: the login wrote {written:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A login that finds no credential vault it can open stops before the login
/// token is sent: `connect` returns the retryable error that says why and what
/// to do, with no OIDC discovery and no exchange sent. With the vault open
/// again, the same login goes on to its discovery, and a login of the profile
/// writes its chain.
#[test]
fn a_login_with_no_vault_to_open_keeps_its_login_token() {
    use std::sync::atomic::Ordering::SeqCst;
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_login_with_no_vault_to_open_keeps_its_login_token",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    let profile = "novaultlogin";
    let login = || {
        let mut login = JottacloudProvider::new(JottacloudConfig {
            login_token: SecretString::from(unanswered_login_token()),
            device: "Jotta".into(),
            mountpoint: "Archive".into(),
            initial_path: None,
        })
        .with_profile_id(profile);
        login.set_credential_user(Some(chains.first));
        login
    };
    let (sent, discovered) = (chains.submissions(), super::tests::DISCOVERIES.load(SeqCst));
    CredentialStore::lock();
    let connected = chains.runtime().block_on(login().connect());
    CredentialStore::unlock_with_master(PASSWORD, None).expect("unlock");
    assert_eq!(
        super::tests::DISCOVERIES.load(SeqCst) - discovered,
        0,
        "no OIDC discovery is sent: {connected:?}"
    );
    assert_eq!(
        chains.submitted_since(sent),
        Vec::<String>::new(),
        "no exchange is sent"
    );
    match &connected {
        Err(ProviderError::ConnectionFailed(message)) => {
            assert_eq!(
                message,
                super::NO_VAULT_FOR_LOGIN,
                "the message says why and what to do"
            );
            assert!(
                !message.contains("still valid"),
                "the message claims only what this attempt knows: {message}"
            )
        }
        other => panic!("a login with no vault stops with a retryable error: {other:?}"),
    }

    // The vault opens: the same login token goes on to its discovery (which
    // the double leaves unanswered), and a login writes the profile's chain.
    let connected = chains.runtime().block_on(login().connect());
    assert!(
        matches!(&connected, Err(ProviderError::AuthenticationFailed(m)) if m.contains("OIDC discovery failed")),
        "with the vault open the login goes ahead: {connected:?}"
    );
    assert_eq!(super::tests::DISCOVERIES.load(SeqCst) - discovered, 1);
    assert_eq!(chains.login(profile, "login-novault"), "login-novault");
}

/// A rotation kept for a profile's key, whose chain was loaded from the
/// singleton, keeps that source when it is handed out again: a sign-out of
/// the singleton by another process during the next exchange stops its
/// successor too, and a singleton that still holds the handed-out chain is
/// written with the profile's key.
#[test]
fn a_rotation_handed_out_again_keeps_the_key_it_was_loaded_from() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_rotation_handed_out_again_keeps_the_key_it_was_loaded_from",
    ) {
        return;
    }
    let chains = Chains::new();
    let singleton = "jottacloud_refresh";
    let mut failures = Vec::new();
    // A rotation kept for the profile's key, holding `own`, by a connection
    // that loaded its chain from the singleton.
    let file_from_singleton = |profile: &str, own: &str| -> String {
        let key = format!("jottacloud_refresh_{profile}");
        chains.store.store(&key, &chains.chain(own)).unwrap();
        let noted = chains.noted(profile).expect("count");
        let source = partitions::chain_deletions(&chains.store, chains.owner, singleton)
            .unwrap()
            .generation;
        let mut filed: serde_json::Value =
            serde_json::from_str(&chains.chain(&format!("pending-{profile}"))).unwrap();
        filed[PENDING_BASE] = own.into();
        filed[PENDING_GEN] = noted.into();
        filed[PENDING_SOURCE] = singleton.into();
        filed[PENDING_SOURCE_GEN] = source.into();
        PENDING_ROTATIONS.lock().unwrap().insert(
            (chains.owner, key.clone()),
            zeroize::Zeroizing::new(filed.to_string()),
        );
        key
    };

    // The singleton is signed out elsewhere right before the write.
    chains
        .store
        .store(singleton, &chains.chain("singleton-lineage"))
        .unwrap();
    let key = file_from_singleton("lineage", "own-lineage");
    *DELETE_AROUND_WRITE.lock().unwrap() = Some((
        Around::BeforeWrite,
        key.clone(),
        chains.owner,
        singleton.to_string(),
    ));
    let from = chains.submissions();
    let mut connection = chains.bound("lineage");
    let connected = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    *DELETE_AROUND_WRITE.lock().unwrap() = None;
    drop(connection);
    let sent = chains.submitted_since(from);
    if !matches!(connected, Ok(true))
        || sent != ["pending-lineage"]
        || chains.stored(&key) != "own-lineage"
        || chains.pending(&key).is_some()
    {
        failures.push(format!(
            "the source signed out: {connected:?}, sent {sent:?}, the key holds {}, kept {}",
            chains.stored(&key),
            chains.pending(&key).is_some()
        ));
    }
    PENDING_ROTATIONS.lock().unwrap().clear();

    // The singleton still holds the chain handed out: it gets the successor.
    chains
        .store
        .store(singleton, &chains.chain("pending-followed"))
        .unwrap();
    let key = file_from_singleton("followed", "own-followed");
    let mut connection = chains.bound("followed");
    let connected = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    let rotated = connection.refresh_token.expose_secret().to_string();
    drop(connection);
    if !matches!(connected, Ok(true))
        || chains.stored(&key) != rotated
        || chains.stored(singleton) != rotated
        || chains.pending(&key).is_some()
        || chains.pending(singleton).is_some()
    {
        failures.push(format!(
            "the source holds the chain: {connected:?}, the key holds {}, the singleton {}, expected {rotated}",
            chains.stored(&key),
            chains.stored(singleton)
        ));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A rotation kept for a profile's key whose chain was loaded from the
/// singleton does not withhold a chain imported into that key since: while a
/// deletion of the singleton runs elsewhere, the profile's key hands out the
/// imported chain, and the rotation, which no longer applies, is dropped.
#[test]
fn an_imported_chain_is_not_withheld_by_its_predecessors_source() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::an_imported_chain_is_not_withheld_by_its_predecessors_source",
    ) {
        return;
    }
    let chains = Chains::new();
    let singleton = "jottacloud_refresh";
    let profile = "importedover";
    let key = format!("jottacloud_refresh_{profile}");
    chains
        .store
        .store(singleton, &chains.chain("singleton-importedover"))
        .unwrap();
    chains
        .store
        .store(&key, &chains.chain("own-importedover"))
        .unwrap();
    let noted = chains.noted(profile).expect("count");
    let source = partitions::chain_deletions(&chains.store, chains.owner, singleton)
        .unwrap()
        .generation;
    let mut filed: serde_json::Value =
        serde_json::from_str(&chains.chain("pending-importedover")).unwrap();
    filed[PENDING_BASE] = "own-importedover".into();
    filed[PENDING_GEN] = noted.into();
    filed[PENDING_SOURCE] = singleton.into();
    filed[PENDING_SOURCE_GEN] = source.into();
    PENDING_ROTATIONS.lock().unwrap().insert(
        (chains.owner, key.clone()),
        zeroize::Zeroizing::new(filed.to_string()),
    );
    // An independent chain is imported into the profile's key, and the
    // singleton's deletion runs elsewhere.
    chains
        .store
        .store(&key, &chains.chain("imported-importedover"))
        .unwrap();
    let mut held = holder(singleton, chains.owner, chains.first, "serve");
    let loaded = handed_out(&chains, &key);
    let kept = chains.pending(&key).is_some();
    held.run("end-failed");
    drop(held);
    assert_eq!(
        (loaded.as_deref(), kept),
        (Some("imported-importedover"), false),
        "the imported chain is handed out and the rotation is dropped"
    );
}

/// A rotation neither key could store, of a chain a profile's connection
/// loaded from the singleton, is kept for both keys. The profile reconnects
/// with it: its successor is written to the singleton too, which still holds
/// the base the singleton's own kept rotation was filed on, so the
/// singleton's reader is not handed the chain this exchange consumed.
#[test]
fn a_singletons_kept_successor_moves_on_with_its_profile() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_singletons_kept_successor_moves_on_with_its_profile",
    ) {
        return;
    }
    let chains = Chains::new();
    let singleton = "jottacloud_refresh";
    let profile = "keptboth";
    let key = format!("jottacloud_refresh_{profile}");
    chains
        .store
        .store(singleton, &chains.chain("singleton-keptboth"))
        .unwrap();
    let noted = chains.noted(profile).expect("count");
    let source = partitions::chain_deletions(&chains.store, chains.owner, singleton)
        .unwrap()
        .generation;
    // What a connection of the profile that loaded the singleton's chain and
    // could store its rotation in neither key keeps.
    let kept = |base: &str, generation: u64, from: Option<u64>| {
        let mut filed: serde_json::Value =
            serde_json::from_str(&chains.chain("kept-keptboth")).unwrap();
        filed[PENDING_BASE] = base.into();
        filed[PENDING_GEN] = generation.into();
        if let Some(from) = from {
            filed[PENDING_SOURCE] = singleton.into();
            filed[PENDING_SOURCE_GEN] = from.into();
        }
        zeroize::Zeroizing::new(filed.to_string())
    };
    {
        let mut pending = PENDING_ROTATIONS.lock().unwrap();
        pending.insert((chains.owner, key.clone()), kept("", noted, Some(source)));
        pending.insert(
            (chains.owner, singleton.to_string()),
            kept("singleton-keptboth", source, None),
        );
    }
    let from = chains.submissions();
    let mut connection = chains.bound(profile);
    let connected = chains
        .runtime()
        .block_on(connection.try_connect_with_refresh());
    let rotated = connection.refresh_token.expose_secret().to_string();
    drop(connection);
    let mut failures = Vec::new();
    if !matches!(connected, Ok(true)) || chains.submitted_since(from) != ["kept-keptboth"] {
        failures.push(format!(
            "the kept rotation is exchanged: {connected:?}, sent {:?}",
            chains.submitted_since(from)
        ));
    }
    for written in [key.as_str(), singleton] {
        if chains.stored(written) != rotated || chains.pending(written).is_some() {
            failures.push(format!(
                "{written} holds {} (expected {rotated}), kept {}",
                chains.stored(written),
                chains.pending(written).is_some()
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A deletion whose end its process could not record, in a process that
/// stays alive, does not hold the key for every other process for as long
/// as it lives: that process settles it at its next connection, whatever
/// profile it is for. A rotation kept from before the deletion is dropped,
/// and a login writes the key again.
#[test]
fn an_unended_deletion_of_a_live_process_is_settled_by_it() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::an_unended_deletion_of_a_live_process_is_settled_by_it",
    ) {
        return;
    }
    let chains = Chains::new();
    let mut failures = Vec::new();
    let key = "jottacloud_refresh_unendedalive";
    chains.store.store(key, &chains.chain("unended-x")).unwrap();
    let noted = chains.noted("unendedalive").expect("count");
    chains.file(key, "filed-before", noted);
    let mut held = holder(key, chains.owner, chains.first, "unended");
    let during = partitions::chain_deletions(&chains.store, chains.owner, key).unwrap();
    if during.in_progress != 1 {
        failures.push(format!(
            "the unended deletion reads as running elsewhere: {during:?}"
        ));
    }
    held.run("note unendedother");
    let after = partitions::chain_deletions(&chains.store, chains.owner, key).unwrap();
    if after.in_progress != 0 || after.generation <= during.generation {
        failures.push(format!(
            "settled by its live process at its next connection: {during:?} -> {after:?}"
        ));
    }
    drop(held);
    let loaded = handed_out(&chains, key);
    if loaded.is_some() || chains.pending(key).is_some() {
        failures.push(format!(
            "the rotation kept from before the deletion: {loaded:?}, kept {}",
            chains.pending(key).is_some()
        ));
    }
    let login = chains.login("unendedalive", "login-unended");
    if login != "login-unended" {
        failures.push(format!("a login after the deletion wrote {login:?}"));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A connection does not hold up the async worker while another thread
/// holds the lock deletions and rotation writes are serialized by: the clock
/// it begins at is read off the worker, and building a provider reads none.
#[test]
fn the_deletion_clock_is_read_off_the_async_worker() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::the_deletion_clock_is_read_off_the_async_worker",
    ) {
        return;
    }
    let chains = Chains::new();
    let first = chains.first;
    let mut failures = Vec::new();
    for case in ["a connection", "a new provider"] {
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let holding = std::thread::spawn(move || {
            let _held = DELETED_CHAINS.lock().unwrap_or_else(|e| e.into_inner());
            locked_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(800));
            Instant::now()
        });
        locked_rx.recv().unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (ticked, ended) = runtime.block_on(async move {
            // When the task beside it ran (see
            // `a_login_waiting_for_the_vault_leaves_the_async_worker_free`).
            let ticker = tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                Instant::now()
            });
            let task = tokio::spawn(async move {
                if case == "a connection" {
                    let mut connection = bound_provider(first, "clockoff");
                    let _ = connection.try_connect_with_refresh().await;
                } else {
                    drop(bound_provider(first, "clockoff"));
                }
                Instant::now()
            });
            (ticker.await, task.await)
        });
        let released = holding.join().unwrap();
        drop(runtime);
        let ended = ended.expect("the task did not panic");
        if ticked.expect("the ticker did not panic") >= released {
            failures.push(format!("{case} held the only async worker"));
        }
        if case == "a connection" && ended < released {
            failures.push(format!("{case} did not wait for the lock"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A connection reads the partition database and the vault only off the
/// async worker, on every path: a stored chain exchanged, one Jotta refuses,
/// and a login (which fails at its discovery here), each run as a task.
#[test]
fn a_connection_reads_its_chains_off_the_async_worker() {
    if !crate::provider_commands::isolated_account_fixture::in_isolated_child(
        "providers::jottacloud::chain_tests::a_connection_reads_its_chains_off_the_async_worker",
    ) {
        return;
    }
    let chains = Chains::new();
    let _ = chains.store.delete("jottacloud_refresh");
    chains
        .store
        .store("jottacloud_refresh_probegood", &chains.chain("probe-good"))
        .unwrap();
    chains
        .store
        .store(
            "jottacloud_refresh_proberefused",
            &chains.chain("refused-probe"),
        )
        .unwrap();
    let first = chains.first;
    crate::credential_store::off_worker_probe::take();
    let (good, refused, login) = chains.runtime().block_on(async move {
        let good = tokio::spawn(async move {
            let mut connection = bound_provider(first, "probegood");
            connection
                .try_connect_with_refresh()
                .await
                .map_err(|e| e.to_string())
        });
        let good = good.await.unwrap();
        let refused = tokio::spawn(async move {
            let mut connection = bound_provider(first, "proberefused");
            connection
                .try_connect_with_refresh()
                .await
                .map_err(|e| e.to_string())
        });
        let refused = refused.await.unwrap();
        let login = tokio::spawn(async move {
            let mut login = JottacloudProvider::new(JottacloudConfig {
                login_token: SecretString::from(unanswered_login_token()),
                device: "Jotta".into(),
                mountpoint: "Archive".into(),
                initial_path: None,
            })
            .with_profile_id("probelogin");
            login.set_credential_user(Some(first));
            login.connect().await.map_err(|e| e.to_string())
        });
        (good, refused, login.await.unwrap())
    });
    let on_worker = crate::credential_store::off_worker_probe::take();
    assert_eq!(good, Ok(true), "the stored chain is exchanged");
    assert_eq!(refused, Ok(false), "the refused chain falls through");
    assert!(
        login
            .as_ref()
            .is_err_and(|e| e.contains("OIDC discovery failed")),
        "the login got to its discovery: {login:?}"
    );
    assert!(
        on_worker.is_empty(),
        "read on the async worker: {on_worker:?}"
    );
}
