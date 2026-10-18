//! A file, or part of one, that a server (`serve ftp`, `serve http`, `serve
//! webdav`) sends its client. It is read in windows paid to the speed limiter
//! as the client takes them, so a large file neither waits whole in memory
//! nor goes out faster than the cap. A backend that cannot read a range is
//! downloaded once into a staging file on disk and sent from there.

use super::{ProviderError, StorageProvider};
use crate::transfer_dag::governor::{GlobalTransferGovernor, TransferDirection};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncSeekExt, ReadBuf};

/// The longest window a served file reads when no cap is set.
pub const SERVED_WINDOW: u64 = 1024 * 1024;

/// What a server copies to its client.
pub type ServedReader = Box<dyn AsyncRead + Send + Sync + Unpin>;

type SharedProvider = Arc<tokio::sync::Mutex<Box<dyn StorageProvider>>>;

/// Bytes `start..end` of `path` (`end` at most the file's size), as the
/// reader a server copies to its client. The first window is read before this
/// returns, so a missing file is an error the server can answer with rather
/// than a stream that breaks after its headers went out. A backend whose
/// `read_range` answers `NotSupported`, or a server that did not answer the
/// first window for its range (`ParallelRefused`), is staged whole on disk
/// first.
pub async fn open_served_range(
    governor: Arc<GlobalTransferGovernor>,
    provider: SharedProvider,
    path: String,
    start: u64,
    end: u64,
) -> Result<ServedReader, ProviderError> {
    if start >= end {
        return Ok(Box::new(tokio::io::empty()));
    }
    match next_window(&governor, &provider, &path, start, end).await {
        Ok(first) => Ok(Box::new(stream_windows(
            governor, provider, path, start, end, first,
        ))),
        Err(ProviderError::NotSupported(_) | ProviderError::ParallelRefused(_)) => {
            staged(&provider, &path, start, end).await
        }
        Err(e) => Err(e),
    }
}

/// The file `path` from `start`, whose provider lists it at `listed` bytes,
/// and how many bytes the reader sends. A file listed as empty is downloaded
/// to find out what it holds: a provider can list 0 bytes for a file it
/// exports when downloaded (a Google Docs document), and serving the listed
/// size sent such a file empty.
pub async fn open_served_file(
    governor: Arc<GlobalTransferGovernor>,
    provider: SharedProvider,
    path: String,
    start: u64,
    listed: u64,
) -> Result<(ServedReader, u64), ProviderError> {
    if listed > 0 {
        let start = start.min(listed);
        let reader = open_served_range(governor, provider, path, start, listed).await?;
        return Ok((reader, listed - start));
    }
    let (file, temp, held) = stage_whole(&provider, &path, 0).await?;
    let start = start.min(held);
    let reader = staged_part(file, temp, start, held).await?;
    Ok((reader, held - start))
}

/// One window from `at`: about one second of the tightest download cap, at
/// most [`SERVED_WINDOW`], paid before it is read. A source that ends before
/// `end` fails the transfer: the client must not take a short file for the
/// whole one.
async fn next_window(
    governor: &GlobalTransferGovernor,
    provider: &tokio::sync::Mutex<Box<dyn StorageProvider>>,
    path: &str,
    at: u64,
    end: u64,
) -> Result<Vec<u8>, ProviderError> {
    let (window, _) = crate::transfer_dag::throttle::range_read_pacing_with(
        governor,
        TransferDirection::Download,
        SERVED_WINDOW,
    );
    let len = (end - at).min(window);
    let mut data =
        super::multi_thread::read_requested_range(governor, provider, path, at, len).await?;
    if (data.len() as u64) < len {
        return Err(ProviderError::TransferFailed(format!(
            "{path} ended at byte {} of the {end} being served",
            at + data.len() as u64
        )));
    }
    data.truncate(len as usize);
    Ok(data)
}

/// The windows after `first`, read by a task one or two windows ahead of the
/// client. The task stops when the client goes away.
fn stream_windows(
    governor: Arc<GlobalTransferGovernor>,
    provider: SharedProvider,
    path: String,
    start: u64,
    end: u64,
    first: Vec<u8>,
) -> WindowReader {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    let mut at = start + first.len() as u64;
    let _ = tx.try_send(Ok(first));
    tokio::spawn(async move {
        while at < end {
            let next = next_window(&governor, &provider, &path, at, end).await;
            let failed = next.is_err();
            let item = next
                .inspect(|data| at += data.len() as u64)
                .map_err(|e| std::io::Error::other(e.to_string()));
            if tx.send(item).await.is_err() || failed {
                return;
            }
        }
    });
    WindowReader {
        rx,
        chunk: Vec::new(),
        taken: 0,
        left: end - start,
    }
}

struct WindowReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>,
    chunk: Vec<u8>,
    taken: usize,
    /// Bytes still owed to the client: the windows ending before them is an
    /// error, not the end of the file.
    left: u64,
}

impl AsyncRead for WindowReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if self.taken < self.chunk.len() {
                let n = (self.chunk.len() - self.taken).min(buf.remaining());
                let taken = self.taken;
                buf.put_slice(&self.chunk[taken..taken + n]);
                self.taken += n;
                self.left = self.left.saturating_sub(n as u64);
                return Poll::Ready(Ok(()));
            }
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(Ok(chunk))) => {
                    self.chunk = chunk;
                    self.taken = 0;
                }
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
                Poll::Ready(None) if self.left > 0 => {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("the served file stopped {} bytes short", self.left),
                    )))
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// `start..end` of a file whose backend cannot read a range: downloaded whole
/// (the backend's own transfer, which pays the limiter) into a staging file
/// on disk, then read from there. The staging file goes with the reader.
async fn staged(
    provider: &tokio::sync::Mutex<Box<dyn StorageProvider>>,
    path: &str,
    start: u64,
    end: u64,
) -> Result<ServedReader, ProviderError> {
    let (file, temp, staged) = stage_whole(provider, path, end).await?;
    if staged < end {
        return Err(ProviderError::TransferFailed(format!(
            "{path} came down as {staged} bytes, {end} were to be served"
        )));
    }
    staged_part(file, temp, start, end).await
}

/// `path` downloaded whole into a staging file with room for `room` bytes:
/// the open file, the staging file it lives in, and its length.
async fn stage_whole(
    provider: &tokio::sync::Mutex<Box<dyn StorageProvider>>,
    path: &str,
    room: u64,
) -> Result<(tokio::fs::File, tempfile::NamedTempFile, u64), ProviderError> {
    let dir = crate::util::scratch::staging_dir(room).map_err(ProviderError::TransferFailed)?;
    let temp = tempfile::NamedTempFile::new_in(&dir)
        .map_err(|e| ProviderError::TransferFailed(format!("staging file: {e}")))?;
    provider
        .lock()
        .await
        .download(path, &temp.path().to_string_lossy(), None)
        .await?;
    let file = tokio::fs::File::open(temp.path())
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("staged file: {e}")))?;
    let held = file
        .metadata()
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("staged file: {e}")))?
        .len();
    Ok((file, temp, held))
}

/// `path` downloaded whole into a staging file on disk with room for `room`
/// bytes, by the backend's own transfer (which pays the speed limiter). For a
/// server whose client reads at offsets it chooses (an SFTP READ, a FUSE
/// read) from a backend that cannot read a range: one download, which every
/// later read answers from with [`read_staged`].
pub async fn stage_for_reading(
    provider: &tokio::sync::Mutex<Box<dyn StorageProvider>>,
    path: &str,
    room: u64,
) -> Result<tempfile::NamedTempFile, ProviderError> {
    let (_, temp, _) = stage_whole(provider, path, room).await?;
    Ok(temp)
}

/// Up to `len` bytes at `offset` of a file [`stage_for_reading`] staged:
/// fewer at its end, none past it.
pub fn read_staged(
    staged: &tempfile::NamedTempFile,
    offset: u64,
    len: u64,
) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek};
    let mut file = staged.reopen()?;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut data = Vec::new();
    file.take(len).read_to_end(&mut data)?;
    Ok(data)
}

/// Bytes `start..end` of a staged file, as a reader that keeps the staging
/// file until it is dropped.
async fn staged_part(
    mut file: tokio::fs::File,
    temp: tempfile::NamedTempFile,
    start: u64,
    end: u64,
) -> Result<ServedReader, ProviderError> {
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|e| ProviderError::TransferFailed(format!("staged file: {e}")))?;
    Ok(Box::new(StagedReader {
        file: tokio::io::AsyncReadExt::take(file, end - start),
        _temp: temp,
    }))
}

struct StagedReader {
    file: tokio::io::Take<tokio::fs::File>,
    _temp: tempfile::NamedTempFile,
}

impl AsyncRead for StagedReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.file).poll_read(cx, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_dag::governor::GovernorConfig;
    use tokio::io::AsyncReadExt;

    fn limiter(cap: u64) -> Arc<GlobalTransferGovernor> {
        let governor = GlobalTransferGovernor::new(GovernorConfig {
            buffer_bytes: 0,
            bandwidth_bps: 0,
            endpoint_slots: 4,
            disk_device_slots: 4,
        });
        governor.set_transfer_limits(0, cap);
        governor
    }

    fn paid(governor: &GlobalTransferGovernor) -> u64 {
        governor
            .directional_bandwidth(TransferDirection::Download)
            .granted_bytes()
    }

    /// A local folder holding `/big.bin` of `size` bytes, as a server shares it.
    async fn folder(size: usize) -> (tempfile::TempDir, Vec<u8>, SharedProvider) {
        let dir = tempfile::tempdir().unwrap();
        let content: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.path().join("big.bin"), &content).unwrap();
        let mut p = crate::providers::directory::DirectoryProvider::local(dir.path().into());
        p.connect().await.unwrap();
        let provider: Box<dyn StorageProvider> = Box::new(p);
        (dir, content, Arc::new(tokio::sync::Mutex::new(provider)))
    }

    async fn served(
        governor: &Arc<GlobalTransferGovernor>,
        provider: &SharedProvider,
        start: u64,
        end: u64,
    ) -> std::io::Result<Vec<u8>> {
        let mut reader = open_served_range(
            governor.clone(),
            provider.clone(),
            "/big.bin".to_string(),
            start,
            end,
        )
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await?;
        Ok(out)
    }

    /// The whole file and a part of it come out byte for byte, and every byte
    /// served was paid to the limiter once. `serve ftp` and `serve http`
    /// downloaded the file whole into memory, unpaced: 96 MiB under an 8 MiB/s
    /// cap went out in 0.12 s.
    #[tokio::test]
    async fn a_served_file_is_paid_byte_for_byte() {
        let (_dir, content, provider) = folder(3 * 1024 * 1024 + 17).await;
        let governor = limiter(1 << 40);
        let size = content.len() as u64;
        assert_eq!(
            served(&governor, &provider, 0, size).await.unwrap(),
            content
        );
        assert_eq!(
            served(&governor, &provider, 1_000_000, 2_500_000)
                .await
                .unwrap(),
            content[1_000_000..2_500_000]
        );
        assert!(served(&governor, &provider, size, size)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(paid(&governor), size + 1_500_000);
    }

    /// Under a cap a served file goes out no faster than the cap allows, past
    /// the one second of burst the limiter starts with.
    #[tokio::test(start_paused = true)]
    async fn a_served_file_goes_out_at_the_cap() {
        let (_dir, content, provider) = folder(4 * 1024 * 1024).await;
        let cap = 1024 * 1024;
        let governor = limiter(cap);
        let started = tokio::time::Instant::now();
        let out = served(&governor, &provider, 0, content.len() as u64)
            .await
            .unwrap();
        let took = started.elapsed();
        assert_eq!(out, content);
        assert!(
            took >= std::time::Duration::from_millis(2900),
            "4 MiB under a 1 MiB/s cap went out in {took:?}"
        );
    }

    /// A file shorter than what is being served fails the transfer instead of
    /// ending it early, so the client does not take a cut file for the whole.
    #[tokio::test]
    async fn a_served_file_that_ends_early_fails() {
        let (_dir, content, provider) = folder(2 * 1024 * 1024).await;
        let governor = limiter(1 << 40);
        let error = served(&governor, &provider, 0, content.len() as u64 + 4096)
            .await
            .expect_err("a source shorter than the range served");
        assert!(error.to_string().contains("ended at byte"), "{error}");
    }

    /// A backend that cannot read a range, and downloads whole files only:
    /// the trait's default answers `NotSupported`, or, with `ignores_ranges`,
    /// a server that sent something else than the range asked
    /// (`ParallelRefused`).
    struct WholeFilesOnly {
        content: Vec<u8>,
        downloads: Arc<std::sync::atomic::AtomicUsize>,
        ignores_ranges: bool,
    }

    #[async_trait::async_trait]
    impl StorageProvider for WholeFilesOnly {
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
        fn provider_type(&self) -> crate::providers::ProviderType {
            crate::providers::ProviderType::GoogleDrive
        }
        fn display_name(&self) -> String {
            "whole-files-only".to_string()
        }
        async fn connect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn disconnect(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        fn is_connected(&self) -> bool {
            true
        }
        async fn list(
            &mut self,
            _path: &str,
        ) -> Result<Vec<crate::providers::RemoteEntry>, ProviderError> {
            Ok(Vec::new())
        }
        async fn pwd(&mut self) -> Result<String, ProviderError> {
            Ok("/".to_string())
        }
        async fn cd(&mut self, _path: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn cd_up(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn download(
            &mut self,
            _remote_path: &str,
            local_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            self.downloads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::fs::write(local_path, &self.content)
                .map_err(|e| ProviderError::TransferFailed(e.to_string()))
        }
        async fn download_to_bytes(
            &mut self,
            _remote_path: &str,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(ProviderError::NotSupported("download_to_bytes".to_string()))
        }
        async fn upload(
            &mut self,
            _local_path: &str,
            _remote_path: &str,
            _progress: Option<Box<dyn Fn(u64, u64) + Send>>,
        ) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("upload".to_string()))
        }
        async fn mkdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("mkdir".to_string()))
        }
        async fn delete(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("delete".to_string()))
        }
        async fn rmdir(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir".to_string()))
        }
        async fn rmdir_recursive(&mut self, _path: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rmdir_recursive".to_string()))
        }
        async fn rename(&mut self, _from: &str, _to: &str) -> Result<(), ProviderError> {
            Err(ProviderError::NotSupported("rename".to_string()))
        }
        async fn stat(
            &mut self,
            path: &str,
        ) -> Result<crate::providers::RemoteEntry, ProviderError> {
            Err(ProviderError::NotFound(path.to_string()))
        }
        async fn size(&mut self, _path: &str) -> Result<u64, ProviderError> {
            Ok(self.content.len() as u64)
        }
        async fn exists(&mut self, _path: &str) -> Result<bool, ProviderError> {
            Ok(true)
        }
        async fn keep_alive(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn server_info(&mut self) -> Result<String, ProviderError> {
            Ok("whole-files-only".to_string())
        }
        async fn read_range(
            &mut self,
            _path: &str,
            _offset: u64,
            _len: u64,
        ) -> Result<Vec<u8>, ProviderError> {
            Err(if self.ignores_ranges {
                ProviderError::ParallelRefused("the server ignored Range".to_string())
            } else {
                ProviderError::NotSupported("read_range".to_string())
            })
        }
    }

    fn whole_files_only(
        content: &[u8],
        ignores_ranges: bool,
    ) -> (SharedProvider, Arc<std::sync::atomic::AtomicUsize>) {
        let downloads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider: Box<dyn StorageProvider> = Box::new(WholeFilesOnly {
            content: content.to_vec(),
            downloads: downloads.clone(),
            ignores_ranges,
        });
        (Arc::new(tokio::sync::Mutex::new(provider)), downloads)
    }

    /// A backend that cannot read a range, or a server that ignores the
    /// range asked, is downloaded once, to disk, and the part asked for is
    /// sent from there.
    #[tokio::test]
    async fn a_backend_without_ranges_is_served_from_a_staging_file() {
        let content: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        for ignores_ranges in [false, true] {
            let (provider, downloads) = whole_files_only(&content, ignores_ranges);
            let governor = limiter(1 << 40);
            assert_eq!(
                served(&governor, &provider, 1000, 250_000).await.unwrap(),
                content[1000..250_000],
                "ignores ranges: {ignores_ranges}"
            );
            assert_eq!(downloads.load(std::sync::atomic::Ordering::SeqCst), 1);
            let error = served(&governor, &provider, 0, content.len() as u64 + 1)
                .await
                .expect_err("more than the backend has");
            assert!(error.to_string().contains("came down as"), "{error}");
        }
    }

    /// A file its provider lists as empty is served as what it downloads to:
    /// a Google Docs document lists 0 bytes and downloads as its export.
    #[tokio::test]
    async fn a_file_listed_empty_is_served_as_what_it_downloads_to() {
        use tokio::io::AsyncReadExt;
        let content = b"an exported document".to_vec();
        for (start, sent) in [(0, &content[..]), (3, &content[3..]), (99, &b""[..])] {
            let (provider, _) = whole_files_only(&content, false);
            let (mut reader, len) =
                open_served_file(limiter(1 << 40), provider, "/doc".to_string(), start, 0)
                    .await
                    .unwrap();
            let mut served = Vec::new();
            reader.read_to_end(&mut served).await.unwrap();
            assert_eq!(served, sent, "from {start}");
            assert_eq!(len, sent.len() as u64, "from {start}");
        }
    }

    /// What a client reading at offsets it chooses gets from a staged copy:
    /// the bytes there, fewer at the end, none past it.
    #[tokio::test]
    async fn a_staged_copy_answers_reads_at_any_offset() {
        let content: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let (provider, downloads) = whole_files_only(&content, false);
        let staged = stage_for_reading(&provider, "/f", 0).await.unwrap();
        assert_eq!(read_staged(&staged, 100, 50).unwrap(), content[100..150]);
        assert_eq!(read_staged(&staged, 9_990, 50).unwrap(), content[9_990..]);
        assert!(read_staged(&staged, 20_000, 50).unwrap().is_empty());
        assert_eq!(downloads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
