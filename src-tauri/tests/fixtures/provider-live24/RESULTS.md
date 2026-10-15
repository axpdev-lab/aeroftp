# Live provider acceptance, 2026-10-15

Tracker #1202 Requested item 24, deferred acceptance from #981. Linux L1, real provider accounts and real loopback FTPS sockets. Every remote mutation was restricted to a new uniquely named test directory; OneDrive was explicitly treated as a personal account. No saved credentials were read directly. The owner authorized destructive tests within this scope.

| Scenario | Observed result |
| --- | --- |
| FTPS explicit and implicit | All six cases passed: successful control, normal post-data 552, and reset-data-socket 552 for each TLS mode. Exactly one STOR and all 94,208 bytes received in every case. Refusals returned exit 4 with 552; controls returned 0. |
| OneDrive stale ID | The same DEV provider first cached the sentinel ID through a successful versions read. A separate CLI process trashed and recreated it. Deletion through the original provider succeeded; independent CLI stat returned exit 2 for the replacement path. The empty owned directory was removed. |
| Box ambiguous purge | Real CLI speed cleanup refused to pick among three trashed generations. Independent native trash listing confirmed three distinct IDs, sizes 25/25/65,536. Exact-ID cleanup then removed only these items, followed by the empty owned directory. |
| kDrive ambiguous purge | Same refusal and independent three-generation proof as Box, under an owned `/Private` directory. Exact-ID cleanup and empty-folder removal passed. |
| Twake large upload | Real 8 MiB upload returned 201. Nine emitted progress samples advanced from 0 to 8,388,608, never backwards; total appeared once and one complete event followed. Independent download SHA-256 matched. |
| Twake retry | Existing-path 512 KiB upload produced POST 409 then PUT 200. Coalesced GUI samples were 0 and 524,288, with one total and one completion. Independent download SHA-256 matched. |
| Twake refusal | Upload over a directory produced real POST 409, an error terminal, samples 0 and 262,144 of 524,288 bytes, and no completion or total report. The dedicated directory was removed afterwards. |
| Proton leading-dash password | Initial live run created a protected link upstream but exposed a CLI 0.9.0 compatibility bug: its URL is nested under `urlAccess.url`. The rebuilt AeroFTP CLI returned exit 0. Its URL and exact leading-dash password matched the official CLI sharing policy in memory. The link was revoked and both owned file/folder were removed and purged after checking their captured UIDs and unique trash names. |

All five deferred acceptance areas passed.

## Proton regression uncovered by the live run

The parser now accepts official CLI 0.9.0's sharing-policy response while retaining the older top-level URL formats. An unrecognized response produces a bounded diagnostic without dumping the policy's custom password or link secrets. Regression tests cover the new response and payload-free parse errors alongside the existing one-token leading-dash argv test.

## Provenance and limits

* FTPS, Box/kDrive speed and OneDrive's independent client used installed AeroFTP CLI 4.2.3, SHA-256 `5c806e045a176ef4f87f66e363d16e52c2f55c381821e14996a656b64c475eb7`.
* OneDrive's retained provider, independent trash observations, and Twake native uploads used the prebuilt debug application, SHA-256 `1c968fcb03e3c4480f602c57074d5504b4432c19bd5fbe6e3496bffc3e2b6409`, with frontend source `2aee19631522a49d466214f52691f0af9adc427c`. These checks verify the shipped provider fixes, not a newly rebuilt native app.
* The patched Proton live rerun used a freshly built debug AeroFTP CLI, SHA-256 `5ea8654b2363ec90a55cfc807a4239aca83d3700f0a944dd360e689c2d64362f`. All 35 focused Proton Rust tests, `cargo fmt --all -- --check`, the CLI build, Python syntax and whitespace checks passed. No complete local Rust suite was duplicated.
* Proton used the official `cli-drive@0.9.0+75612299` and SDK `js@0.22.2+75612299`, installed from Proton's published binary and verified against its published SHA-512. The authenticated session stays in Proton's keyring.
* Proton CLI 0.9.0 reports `undefined` for an unshared node even with `-j`, and rejected UID-based trash paths in this run. Cleanup therefore verified each pre-captured UID against a unique trash name before deleting by that name, then confirmed both UIDs absent. No account-wide purge was used.
* The Box/kDrive path-based purge is reached by CLI speed cleanup. GUI trash selection operates by explicit IDs; no ambiguous-name GUI deletion is claimed.
* OneDrive Personal's recycle bin is unavailable through Graph. Only the owned sentinel generations and empty folder were soft-deleted; their recycle-bin retention was left to OneDrive. No whole-bin operation was attempted.
* Twake's shared GUI event governor coalesces callbacks. The real HTTP status sequence establishes the retry; the recorded samples establish visible progress behavior. This does not claim a forced 401/token-refresh run.

Sanitized machine-readable observations are in [receipt.json](receipt.json). Raw local receipts and transfer logs are private; credentials, account listings, share URLs and sharing policies are excluded from this directory. Reproduction and cleanup steps are in [README.md](README.md).
