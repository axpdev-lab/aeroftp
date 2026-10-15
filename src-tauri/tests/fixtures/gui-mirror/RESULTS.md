# Mirror kept-folder GUI acceptance, 2026-10-15

**PASS**, completing Requested item 23 of
[#1202](https://github.com/axpdev-lab/aeroftp/issues/1202).
The behavior from [#979](https://github.com/axpdev-lab/aeroftp/pull/979) required
no product change.

The repeatable check in `scripts/gui-mirror-kept-folders.mjs` drove an actual
Linux WebKitGTK debug GUI in an isolated XDG fixture. It used the rendered
Compare and Plan controls and real native commands, with synthetic DEV input.
The FTP server was a loopback pyftpdlib 2.2.0 process backed by real files.
The test did not use the owner's profiles, credentials or remote storage.

| Run | Deleted | Skipped / folders kept | Intentional file errors | Result |
|---|---:|---:|---:|---|
| Local Mirror, backup off | 3 | 4 | 1 permission denial | PASS |
| FTP Mirror, versioned backup on | 3 | 6 | 1 refused backup rename | PASS |

Verified in the rendered result dialog and the report passed to it:

- An excluded child keeps its folder with `entries` and its ancestor with
  `unfinished_rows`.
- A child whose deletion or backup fails keeps both ancestors with
  `unfinished_rows`. Only the child is an error.
- A child hidden by the server keeps its folder with `server` and its
  ancestor with `unfinished_rows`.
- All three reasons are visible under **Folders kept**.

Verified independently on the filesystem and FTP command receipts:

- All five protected local/remote sentinels retain their original bytes.
- The ordinary local file and both of its emptied ancestors are removed.
- The ordinary remote file has exactly one byte-identical versioned backup;
  its original path and both emptied ancestors are removed.
- The failed backup receives RNTO 550; neither ancestor receives RMD.
- The hidden-file directory receives RMD 550. No FTP DELE is sent.
- Final cleanup disconnects, removes the artificial profile and credential,
  ends the controller session, stops the owned FTP child and restores the
  intentionally denied directory permission. No cleanup error was recorded.

The automated run began at 2026-10-15 17:30:30 UTC (19:30:30 Europe/Rome).
An exploratory run of the same scenarios also passed before the repeatable
check was prepared; its result dialogs were captured as screenshots.

Environment: English UI, Linux x86_64, Node 24.21.0, Python 3.14.4,
WebKitGTK (`AppleWebKit/605.1.15`, `Version/60.5 Safari/605.1.15`). Frontend
source: `2aee19631522a49d466214f52691f0af9adc427c` with the candidate test fixture.
Native app: existing debug 4.2.3 binary, SHA-256
`1c968fcb03e3c4480f602c57074d5504b4432c19bd5fbe6e3496bffc3e2b6409`.
This native binary was not rebuilt for the run and is identified separately
from the frontend source. The shared Rust compilation slot remained available
to the other worktree.

Focused validation: all 71 `remoteSyncRunner.test.ts` tests passed; Node syntax,
Python syntax, Rust formatting and whitespace checks passed. The full local
Rust suite was not rerun for this test/documentation-only change.
