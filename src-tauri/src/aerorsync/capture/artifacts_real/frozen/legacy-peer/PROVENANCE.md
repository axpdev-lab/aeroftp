# Legacy-peer preamble captures (2026-09-24)

Recorded through `rsync_proxy.py` (sshd ForceCommand tee) on L1.

| Directory | Client | Server | What it pins |
|---|---|---|---|
| `313-native-download-before-fix` | AeroFTP native driver before the fix | rsync 3.1.3, protocol 31 | the failure: version and both algorithm lists sent at once (46 bytes); the server answers version 31, compat `3f` (no `CF_VARINT_FLIST_FLAGS`), seed, and exits with code 12. Complete session. |
| `313-stock-download-z` | stock rsync 3.2.7, `-z` | rsync 3.1.3 | a stock client reads the compat flags first and sends no lists; compat `3e`, no lists, seed, multiplexing |
| `313-stock-download-new-compress` | stock rsync 3.2.7, `-z --new-compress` | rsync 3.1.3 | the argv a stock client sends for zlibx: `-e.LsfxCIvu --new-compress` (no `z`) |
| `313-stock-upload-new-compress` | stock rsync 3.2.7, `-rltpI -z --new-compress --stats` | rsync 3.1.3 | option order on the receiver side: `-ltprIe.iLsfxCIvu --new-compress --stats` |
| `327-stock-download-new-compress` | stock rsync 3.2.7, `-z --new-compress` | rsync 3.2.7 (Debian, protocol 32) | a negotiating server with the compression fixed on the argv sends the checksum list alone |

`*.first64.bin` hold the first 64 bytes of each stream: the whole preamble and
the first multiplex header. The rest of those sessions is a 256 KiB random
file and carries nothing the tests read.
