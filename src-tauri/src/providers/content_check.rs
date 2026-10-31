//! The check of a finished download against the hash the server publishes.
//!
//! A transfer that corrupts bytes, or a server that returns wrong ones, ended
//! as a success unless the size differed. [`ContentCheck`] holds the digest read
//! before the transfer. Every provider renames its temporary inside its own
//! `download`, so a caller would check too late: the writers ask
//! [`before_publish`] first, through a task-local, and a file that differs never
//! reaches its name. A writer outside the task, or a provider that writes the
//! name directly, is checked by [`ContentCheck::settle`], which removes the file.
//!
//! Only `checksum` digests count: its contract is the plaintext, so a crypt
//! overlay and a proprietary or uploader-claimed hash stay out by themselves.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use super::{ProviderError, ProviderType};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static ENABLED: AtomicBool = AtomicBool::new(true);

/// The way to turn the check off: CLI `--ignore-checksum`, the GUI setting.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether a download is checked at all.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A hash the local file can be compared with: lowercase hex of a known length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentDigest {
    algorithm: &'static str,
    hex: String,
}

impl ContentDigest {
    /// The strongest digest of a `checksum` answer that can be computed here. A
    /// value that is neither hex nor base64 of the right length is not one: some
    /// servers fill the field with something else, and that is no corruption.
    pub fn strongest(digests: &HashMap<String, String>) -> Option<Self> {
        [("sha256", 32), ("sha1", 20), ("md5", 16)]
            .into_iter()
            .find_map(|(algorithm, bytes)| {
                let hex = normalized_hex(digests.get(algorithm)?, bytes)?;
                Some(Self { algorithm, hex })
            })
    }
}

fn normalized_hex(value: &str, bytes: usize) -> Option<String> {
    use base64::Engine as _;
    let value = value.trim();
    if value.len() == bytes * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(value.to_ascii_lowercase());
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(value)
        .ok()?;
    (decoded.len() == bytes).then(|| hex::encode(decoded))
}

/// Whether `checksum` is asked for a download on this provider. FTP's `HASH`
/// holds the control connection while the server reads the whole file and no
/// other client asks for it. Proton's digest is what the uploader claimed, and
/// its download already checks every block and the manifest signature. Immich's
/// is a digest of the path for an asset of an external library, and a local copy
/// has no transfer to check.
pub fn worth_asking(provider: ProviderType) -> bool {
    worth_asking_when(enabled(), provider)
}

fn worth_asking_when(enabled: bool, provider: ProviderType) -> bool {
    enabled
        && !matches!(
            provider,
            ProviderType::Ftp
                | ProviderType::Ftps
                | ProviderType::Proton
                | ProviderType::Immich
                | ProviderType::Local
        )
}

/// How long one `checksum` may take. Most answer from what they store, and SFTP
/// hashes on the host at about disk speed. Past this the file is not checked: a
/// server that does not answer must not cost more than the transfer's own limits.
pub fn reading_limit(size_hint: Option<u64>) -> Duration {
    const BYTES_PER_SECOND: u64 = 50 * 1024 * 1024;
    Duration::from_secs(size_hint.unwrap_or(0).div_euclid(BYTES_PER_SECOND).max(15))
}

#[derive(Debug)]
struct State {
    local: PathBuf,
    expected: ContentDigest,
    /// `None` until a writer or [`ContentCheck::settle`] compared the file.
    differs: Option<Option<String>>,
}

tokio::task_local! {
    static CHECK: Arc<Mutex<State>>;
}

/// One download's check. Empty where there is no digest: size alone decides.
#[derive(Clone, Debug, Default)]
pub struct ContentCheck(Option<Arc<Mutex<State>>>);

impl ContentCheck {
    /// The check of the file that will be published at `local`.
    pub fn expecting(expected: Option<ContentDigest>, local: &str) -> Self {
        Self(expected.map(|expected| {
            Arc::new(Mutex::new(State {
                local: PathBuf::from(local),
                expected,
                differs: None,
            }))
        }))
    }

    /// Run a download with this check in reach of the writers it calls.
    pub async fn scope<F: std::future::Future>(&self, download: F) -> F::Output {
        match &self.0 {
            Some(state) => CHECK.scope(Arc::clone(state), download).await,
            None => download.await,
        }
    }

    /// What the download comes to. A difference a writer found is the error,
    /// whatever wording the provider gave it. A file no writer checked is hashed
    /// here, under its name, and removed if it differs.
    pub async fn settle(&self, result: Result<(), ProviderError>) -> Result<(), ProviderError> {
        let Some(state) = &self.0 else {
            return result;
        };
        let (local, expected, found) = {
            let state = lock(state);
            (
                state.local.clone(),
                state.expected.clone(),
                state.differs.clone(),
            )
        };
        let why = match found {
            Some(why) => why,
            None if result.is_ok() => {
                let why = file_differs(local.clone(), &expected).await;
                lock(state).differs = Some(why.clone());
                if why.is_some() {
                    // Already under its name: a file known to be wrong does not stay.
                    let _ = tokio::fs::remove_file(&local).await;
                }
                why
            }
            None => None,
        };
        let Some(why) = why else {
            if result.is_ok() {
                log::info!(
                    "[content-check] {}: {} matches what the server publishes",
                    local.display(),
                    expected.algorithm
                );
            }
            return result;
        };
        Err(ProviderError::TransferFailed(format!(
            "the download was not kept: {why}"
        )))
    }

    /// Whether the file was compared and differs.
    pub fn differs(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|state| matches!(lock(state).differs, Some(Some(_))))
    }
}

fn lock(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Called by a writer with the finished bytes at `written`, before they take the
/// name `final_path`. An error means they must not.
pub async fn before_publish(written: &Path, final_path: &Path) -> std::io::Result<()> {
    let Ok(state) = CHECK.try_with(Arc::clone) else {
        return Ok(());
    };
    let expected = {
        let state = lock(&state);
        // The staged copy of an overlay is on its way to another name. A commit
        // after a refusal is the provider's own second try: it is compared too.
        if state.local != final_path || matches!(state.differs, Some(None)) {
            return Ok(());
        }
        state.expected.clone()
    };
    let why = file_differs(written.to_path_buf(), &expected).await;
    lock(&state).differs = Some(why.clone());
    match why {
        Some(why) => {
            // Written in place there is no temporary to drop, and no old file left.
            if written == final_path {
                let _ = tokio::fs::remove_file(written).await;
            }
            Err(std::io::Error::other(why))
        }
        None => Ok(()),
    }
}

/// Why the file at `path` does not have the content `expected` names, or `None`
/// when it does. A file that cannot be read differs: nothing shows it is the one.
/// Read on the blocking pool, and stopped when the future awaiting it is dropped.
async fn file_differs(path: PathBuf, expected: &ContentDigest) -> Option<String> {
    struct StopOnDrop(Arc<AtomicBool>);
    impl Drop for StopOnDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_on_drop = StopOnDrop(Arc::clone(&stop));
    let algorithm = expected.algorithm;
    let actual = tokio::task::spawn_blocking(move || match algorithm {
        "sha256" => hash_file::<sha2::Sha256>(&path, &stop),
        "sha1" => hash_file::<sha1::Sha1>(&path, &stop),
        _ => hash_file::<md5::Md5>(&path, &stop),
    })
    .await;
    match actual {
        Ok(Ok(actual)) if actual == expected.hex => None,
        Ok(Ok(actual)) => Some(format!(
            "its content hashes to {algorithm} {actual}, and the server publishes {}",
            expected.hex
        )),
        Ok(Err(error)) => Some(format!("its content could not be hashed: {error}")),
        Err(error) => Some(format!("its content could not be hashed: {error}")),
    }
}

fn hash_file<D: sha2::Digest>(path: &Path, stop: &AtomicBool) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = D::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("stopped"));
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(hex::encode(hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO_MD5: &str = "5d41402abc4b2a76b9719d911017c592";

    fn md5_of_hello() -> Option<ContentDigest> {
        let digests = HashMap::from([("md5".to_string(), HELLO_MD5.to_string())]);
        ContentDigest::strongest(&digests)
    }

    #[test]
    fn the_strongest_usable_digest_is_chosen() {
        let digests = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        let sha1 = "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d";
        let chosen = ContentDigest::strongest(&digests(&[("md5", HELLO_MD5), ("sha1", sha1)]));
        assert_eq!(chosen.unwrap().algorithm, "sha1");
        let upper = ContentDigest::strongest(&digests(&[("md5", &HELLO_MD5.to_uppercase())]));
        assert_eq!(upper.unwrap().hex, HELLO_MD5);
        let base64 = ContentDigest::strongest(&digests(&[("md5", "XUFAKrxLKna5cZ2REBfFkg==")]));
        assert_eq!(base64.unwrap().hex, HELLO_MD5);
        for unusable in [
            digests(&[("md5", "not-a-digest")]),
            digests(&[("md5", "abc123")]),
            digests(&[("quickxor", HELLO_MD5), ("adler32", "01020304")]),
        ] {
            assert_eq!(ContentDigest::strongest(&unusable), None, "{unusable:?}");
        }
        let falls_through = digests(&[("sha256", "short"), ("md5", HELLO_MD5)]);
        assert_eq!(
            ContentDigest::strongest(&falls_through).unwrap().algorithm,
            "md5"
        );
    }

    #[tokio::test]
    async fn a_writer_refuses_bytes_that_differ_and_the_settlement_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("x.aerotmp");
        let local = dir.path().join("x");
        let local_text = local.to_str().unwrap();

        std::fs::write(&temp, b"hello").unwrap();
        let check = ContentCheck::expecting(md5_of_hello(), local_text);
        check
            .scope(before_publish(&temp, &local))
            .await
            .expect("the same content passes");
        assert!(!check.differs());
        std::fs::write(&local, b"hello").unwrap();
        check.settle(Ok(())).await.unwrap();
        assert!(local.exists());

        std::fs::write(&temp, b"hullo").unwrap();
        let check = ContentCheck::expecting(md5_of_hello(), local_text);
        let refused = check.scope(before_publish(&temp, &local)).await;
        assert!(refused.is_err());
        assert!(check.differs());
        let settled = check
            .settle(Err(ProviderError::TransferFailed("rename".into())))
            .await
            .unwrap_err()
            .to_string();
        assert!(settled.contains(HELLO_MD5), "{settled}");
    }

    #[tokio::test]
    async fn a_file_no_writer_checked_is_hashed_and_removed_if_it_differs() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("x");
        let local_text = local.to_str().unwrap();

        std::fs::write(&local, b"hello").unwrap();
        let check = ContentCheck::expecting(md5_of_hello(), local_text);
        check.settle(Ok(())).await.unwrap();
        assert!(local.exists());

        std::fs::write(&local, b"hullo").unwrap();
        let check = ContentCheck::expecting(md5_of_hello(), local_text);
        assert!(check.settle(Ok(())).await.is_err());
        assert!(!local.exists(), "a file known to be wrong does not stay");
    }

    #[tokio::test]
    async fn a_file_written_in_place_that_differs_does_not_stay() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("x");
        std::fs::write(&local, b"hullo").unwrap();
        let check = ContentCheck::expecting(md5_of_hello(), local.to_str().unwrap());
        assert!(check.scope(before_publish(&local, &local)).await.is_err());
        assert!(!local.exists());
    }

    #[tokio::test]
    async fn only_the_file_going_to_the_name_is_checked_and_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let staged = dir.path().join("staged");
        let local = dir.path().join("x");
        std::fs::write(&staged, b"another stage of an overlay").unwrap();
        let check = ContentCheck::expecting(md5_of_hello(), local.to_str().unwrap());
        check
            .scope(async {
                before_publish(&staged, &staged).await.unwrap();
                assert!(!check.differs());
                std::fs::write(&local, b"hello").unwrap();
                before_publish(&local, &local).await.unwrap();
                // Already compared and fine: a later commit is not asked.
                before_publish(&staged, &local).await.unwrap();
            })
            .await;
        assert!(!check.differs());
    }

    /// A provider that falls back inside its `download` commits twice. The second
    /// is compared as well, and what it finds is what the download comes to.
    #[tokio::test]
    async fn a_second_commit_after_a_refusal_is_compared_too() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("t");
        let local = dir.path().join("x");
        let check = ContentCheck::expecting(md5_of_hello(), local.to_str().unwrap());
        check
            .scope(async {
                std::fs::write(&temp, b"hullo").unwrap();
                assert!(before_publish(&temp, &local).await.is_err());
                assert!(before_publish(&temp, &local).await.is_err(), "still wrong");
                std::fs::write(&temp, b"hello").unwrap();
                before_publish(&temp, &local).await.unwrap();
            })
            .await;
        assert!(!check.differs());
        std::fs::write(&local, b"hello").unwrap();
        check.settle(Ok(())).await.unwrap();
    }

    #[tokio::test]
    async fn a_segmented_temporary_that_differs_is_not_published() {
        use crate::providers::multi_thread::create_segmented_temp;
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("x");
        std::fs::write(&local, b"what was there").unwrap();
        // Five zero bytes, pre-allocated: not "hello".
        let temp = create_segmented_temp(dir.path().join("x.aerosegtmp"), 5).unwrap();
        let check = ContentCheck::expecting(md5_of_hello(), local.to_str().unwrap());
        assert!(check.scope(temp.check_content(&local)).await.is_err());
        assert!(check.scope(temp.publish(&local)).await.is_err());
        assert_eq!(std::fs::read(&local).unwrap(), b"what was there");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn the_switch_and_the_providers_left_out_stop_the_question() {
        assert!(worth_asking_when(true, ProviderType::S3));
        assert!(worth_asking_when(true, ProviderType::Sftp));
        assert!(!worth_asking_when(false, ProviderType::S3));
        for left_out in [
            ProviderType::Ftp,
            ProviderType::Ftps,
            ProviderType::Proton,
            ProviderType::Immich,
            ProviderType::Local,
        ] {
            assert!(!worth_asking_when(true, left_out));
        }
        assert_eq!(reading_limit(None), Duration::from_secs(15));
        assert_eq!(
            reading_limit(Some(100 * 1024 * 1024 * 1024)),
            Duration::from_secs(2048)
        );
    }

    /// The bytes never take the name, and a part is not kept to resume from.
    #[tokio::test]
    async fn the_writers_refuse_to_publish_what_differs() {
        use crate::providers::atomic_write::{AtomicFile, ResumableFile};
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("x");
        let local_text = local.to_str().unwrap();
        std::fs::write(&local, b"what was there").unwrap();

        let check = ContentCheck::expecting(md5_of_hello(), local_text);
        let mut atomic = AtomicFile::new(local_text).await.unwrap();
        atomic.write_all(b"hullo").await.unwrap();
        assert!(check.scope(atomic.commit()).await.is_err());
        assert_eq!(std::fs::read(&local).unwrap(), b"what was there");

        let check = ContentCheck::expecting(md5_of_hello(), local_text);
        let mut resumable = ResumableFile::open_fresh(local_text).await.unwrap();
        resumable.write_all(b"hullo").await.unwrap();
        assert!(check.scope(resumable.commit()).await.is_err());
        assert_eq!(std::fs::read(&local).unwrap(), b"what was there");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["x"], "neither temporary stays");

        let check = ContentCheck::expecting(md5_of_hello(), local_text);
        let mut atomic = AtomicFile::new(local_text).await.unwrap();
        atomic.write_all(b"hello").await.unwrap();
        check.scope(atomic.commit()).await.unwrap();
        assert_eq!(std::fs::read(&local).unwrap(), b"hello");
    }

    #[tokio::test]
    async fn with_no_check_in_reach_a_writer_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("x");
        std::fs::write(&local, b"anything").unwrap();
        before_publish(&local, &local).await.unwrap();
        let empty = ContentCheck::expecting(None, local.to_str().unwrap());
        empty.scope(before_publish(&local, &local)).await.unwrap();
        empty.settle(Ok(())).await.unwrap();
        assert!(local.exists());
    }
}
