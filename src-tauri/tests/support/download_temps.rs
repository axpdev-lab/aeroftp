//! The download temporaries a test can find left behind in a directory.

use std::path::{Path, PathBuf};

/// Every download temporary or part record in `dir`, whichever writer left it:
/// a resumable part (`.aerotmp`), the SFTP read-ahead's temporary
/// (`.aeroardtmp`), the segmented engine's (`.aerosegtmp`) and a part's record
/// (`.aerosrctmp`). A check for one of these names alone passes when another
/// writer leaves its own. A directory that cannot be read fails the test rather
/// than reading as one with nothing left in it.
pub fn download_temps_in(dir: &Path) -> Vec<PathBuf> {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read the output directory {}: {e}", dir.display()));
    let mut temps: Vec<PathBuf> = entries
        .map(|entry| entry.expect("read an output directory entry").path())
        .filter(|path| {
            ftp_client_gui_lib::providers::atomic_write::is_download_temp_path(
                &path.to_string_lossy(),
            )
        })
        .collect();
    temps.sort();
    temps
}

#[cfg(test)]
mod tests {
    use super::download_temps_in;

    /// Every writer's temporary is found, not only the resumable part's: a
    /// check for `.aerotmp` alone passed while a segmented run's `.aerosegtmp`
    /// was left.
    #[test]
    fn every_writers_temporary_is_found() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "out.bin",
            "out.bin.aerotmp",
            "out.bin.aeroardtmp",
            "out.bin.aerosegtmp",
            "out.bin.aerosrctmp",
        ] {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        let found: Vec<String> = download_temps_in(dir.path())
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            found,
            [
                "out.bin.aeroardtmp",
                "out.bin.aerosegtmp",
                "out.bin.aerosrctmp",
                "out.bin.aerotmp",
            ]
        );
    }

    /// A directory that is not there is not a directory with nothing left.
    #[test]
    #[should_panic(expected = "read the output directory")]
    fn a_missing_directory_fails_the_check() {
        let dir = tempfile::tempdir().unwrap();
        download_temps_in(&dir.path().join("never-created"));
    }
}
