# Live GUI Mirror: folders kept

Repeatable Linux DEV acceptance check for Requested item 23 of tracker
[#1202](https://github.com/axpdev-lab/aeroftp/issues/1202), following
[#979](https://github.com/axpdev-lab/aeroftp/pull/979).

The check clicks the real AeroSync Plan tab, starts Compare, selects Mirror,
changes exclusions, rescans, acknowledges destructive actions and executes.
It observes the report passed to the rendered result dialog and checks its
visible reasons, real file bytes and FTP command receipts. Native commands and
the runner are not mocked. Synthetic DEV input is used, not physical input.

| Destination | Fault / choice | Expected result |
|---|---|---|
| Local right panel | `*.keep` exclusion | Sentinel and both ancestors stay; `entries` on the lower folder, `unfinished_rows` on its parent. |
| Local right panel | Real filesystem permission denies a child delete | Child bytes and both ancestors stay; one file error, both folders `unfinished_rows`. |
| Local right panel | Ordinary removable child | Child and both emptied ancestors disappear, three completed deletions. |
| FTP | `*.keep` exclusion | Sentinel and both ancestors stay; excluded-entry reason is visible. |
| FTP, versioned backup enabled | Server refuses the only copy's backup rename with 550 | Original bytes and both ancestors stay; one backup error, both folders `unfinished_rows`; no RMD of either ancestor. |
| FTP | Server hides one file from listings but refuses RMD as non-empty | Hidden bytes and both ancestors stay; `server` on the lower folder and `unfinished_rows` on its parent. |
| FTP, versioned backup enabled | Normal backup move | One byte-identical backup exists, original child and both emptied ancestors disappear; no DELE command. |

Expected receipts: local `deleted=3, skipped=4, errors=1`; FTP
`deleted=3, skipped=6, errors=1`. These errors are intentional faults and must
remain distinct from folders kept. All three reason labels must be rendered.

## Run

Use Node 22+, English UI and an unprivileged Linux user. Python needs `pyftpdlib` (verified
with 2.2.0); use a dedicated virtual environment, not a global installation.

Start a fresh, owned DEV GUI from the checkout being tested, with the normal
`scripts/gui-controller-dev.mjs` isolation layout:

```bash
export AEROFTP_GUI_TEST_ROOT="$(mktemp -d /tmp/aeroftp-mirror-gui.XXXXXX)"
export INSPECTOR_PORT=9347
mkdir -p "$AEROFTP_GUI_TEST_ROOT"/{config,cache,data}
python3 -m venv "$AEROFTP_GUI_TEST_ROOT/venv"
"$AEROFTP_GUI_TEST_ROOT/venv/bin/pip" install pyftpdlib==2.2.0
export AEROFTP_TEST_PYTHON="$AEROFTP_GUI_TEST_ROOT/venv/bin/python"
```

The app process must inherit `XDG_CONFIG_HOME=$AEROFTP_GUI_TEST_ROOT/config`,
`XDG_CACHE_HOME=$AEROFTP_GUI_TEST_ROOT/cache`,
`XDG_DATA_HOME=$AEROFTP_GUI_TEST_ROOT/data`, and
`WEBKIT_INSPECTOR_HTTP_SERVER=127.0.0.1:$INSPECTOR_PORT`. Supply an artificial
`AEROFTP_MASTER_PASSWORD` on that child, serve this checkout through its DEV
URL, and wait for the main controller to be unlocked and idle. The fixture
must have no saved profiles or existing sessions. A prebuilt debug app can
be used with its corresponding DEV URL; no Cargo command is run by this check.

```bash
node scripts/gui-mirror-kept-folders.mjs
```

The check binds its FTP server to an ephemeral loopback port, creates fresh
children under the owned root, temporarily seeds one artificial profile, and
ends its own controller session and FTP child. It restores the intentionally
removed filesystem permission and removes its profile and credential. It
leaves the GUI running and evidence under `mirror-kept-*/receipt.json`.
Use a fresh GUI fixture for another run. Any cleanup failure is recorded in
the receipt and makes the check fail.

The receipt identifies frontend Git SHA, application version and native binary
SHA-256 separately. A prebuilt native binary is not claimed to have been built
from the frontend SHA. When source correspondence matters, build the debug
binary from that checkout before running, after the shared Rust slot is free.

This closes the GUI acceptance gap for kept-folder reporting. It is not a new
live certification of every storage provider; provider rmdir behavior has its
separate coverage in #979.
