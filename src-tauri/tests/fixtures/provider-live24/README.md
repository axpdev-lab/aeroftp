# Deferred provider acceptance, tracker #1202 item 24

These are live acceptance checks for the fixes in PR #981. They complement the deterministic Rust regressions; they are not part of unattended CI and need explicitly authorized accounts. Use a new, uniquely named directory for every run. Never empty an account-wide trash bin. OneDrive may be a personal account: restrict every mutation to that run's dedicated folder and owned sentinel.

## FTPS post-data 552 (repeatable loopback fixture)

Requires Python 3, OpenSSL and a built AeroFTP CLI. No cloud account or Python package is required.

```sh
python3 src-tauri/tests/fixtures/provider-live24/ftps_final_reply.py --cli /path/to/aeroftp-cli
```

An optional `--output /new/private/directory` keeps the evidence at a chosen location; an existing directory is refused. The fixture creates a short-lived private CA, verifies the server through `SSL_CERT_FILE`, and runs both explicit and implicit FTPS. Each mode has a successful control, a normal post-data 552 refusal, and a 552 after a reset data socket. It asserts one STOR, the complete 94,208-byte payload and its SHA-256, TLS on both sockets, exit 0 for success and exit 4 with 552 for refusal. It never uses `--insecure`. The receipt contains no real credentials. Artificial certificate keys remain inside the private evidence directory.

## OneDrive: same provider, stale file ID

1. Create a unique test folder and sentinel with CLI client B.
2. Connect the owned DEV GUI to OneDrive. Invoke `provider_list_versions {path}` on the sentinel. This is a successful read that calls `OneDriveProvider::resolve_path`, priming the file ID in that provider's cache. `stat` alone does not prime this cache.
3. Keep that GUI provider connected. With separate CLI client B, trash the sentinel, then upload a different generation to the exact same path.
4. Invoke native `provider_delete_file {path, commitMessage:null}` in the original GUI session, without reconnecting or re-priming its cache. This reaches `provider.delete` directly.
5. Independently verify CLI `stat` returns exit 2. A false success on the old cached ID would leave the replacement present.
6. Remove the now-empty owned folder with `rmdir`. OneDrive Personal does not expose recycle-bin operations through Graph; do not work around that by touching other data or clearing its whole recycle bin.

## Box and kDrive: ambiguous permanent deletion

Use an owned folder at Box `/` or kDrive `/Private` (the kDrive root is not writable). Upload and trash two small generations under one unique basename. Run:

```sh
aeroftp-cli speed --profile "EXACT PROFILE" --remote-path "/OWNED-FOLDER/UNIQUE-NAME.bin" --test-size 64K --iterations 1 --json
```

The speed command creates, verifies and soft-deletes a third generation, then invokes the real `StorageProvider::delete_permanent(path)` during cleanup. Assert `cleanup_ok:true`, `trash_purged:false`, and an error beginning `Server error: Not purged: 3 items` that explains the trash cannot identify which generation is meant. Do not infer refusal from a random basename containing the word "ambiguous".

Independently connect the DEV GUI and use `box_list_trash` or `kdrive_list_trash`. Filter the exact unique name and verify three distinct IDs and the expected sizes (25, 25, 65,536 bytes in the recorded run). Capture this proof before cleanup. Purge only those exact captured IDs with `box_permanent_delete {itemId,itemType}` or `kdrive_permanently_delete_trash {fileId}`, then confirm none of that unique name remain. Remove the owned empty folder.

The GUI trash managers intentionally operate on selected IDs; they do not call the ambiguous path helper. This check exercises the path-based cleanup through its actual CLI caller, and uses the native trash manager API for independent preservation proof and exact-ID cleanup.

## Twake: committed progress across a real retry and refusal

Connect an authorized DEV profile on its own instance. Subscribe to native `transfer_event` before invoking `provider_upload_file {localPath,remotePath,commitMessage:null,useDelta:false,resume:false}`. Filter by the unique local filename and await its terminal event.

* Upload a file larger than 1 MiB (8 MiB in the recorded run). Assert transferred bytes never decrease, intermediate samples exist, the total appears once and there is one complete event.
* Pre-create a same-path file, then upload a file at most 1 MiB (512 KiB in the recorded run). The provider eagerly POSTs, receives real 409, looks up the file and retries PUT. Confirm `POST 409` and `PUT 200` in bounded DEV status logs; assert monotonic progress, one total report and one completion.
* Pre-create a directory at a third target, then try to upload a small file there. The real POST receives 409; assert an error terminal, no completed event, and no sample equal to total.
* Download the two successful files independently and compare SHA-256, then remove only the run's dedicated directory.

The shared GUI event governor coalesces progress. The small retry can therefore expose only start and accepted total; the HTTP statuses prove the retry, while the emitted samples prove the user-visible bar does not regress or count twice. The large transfer supplies the multi-sample control.

## Proton: leading-dash share password, official CLI

Install the official Proton Drive CLI and sign in through `proton-drive auth login`. Do not put an account password into AeroFTP. The official CLI's session remains in its own keyring. Use a dedicated file under `/my-files/OWNED-FOLDER` and run AeroFTP `link` with a newly generated, disposable password beginning with `-`, supplied as the single option token `--password=VALUE`.

Assert exit 0, an HTTPS Proton URL, and the requested password in the result without logging the URL, password or full sharing policy. Independently inspect the official CLI sharing policy and compare the password in memory. Revoke the link and remove the owned test file/folder afterwards. CLI 0.9.0 returns the URL under `urlAccess.url`; older top-level URL responses remain covered by the shim regression. Unrecognized sharing responses must produce a bounded error without the policy payload, which contains the custom password and link secrets.

See [RESULTS.md](RESULTS.md) for observed outcomes and limits. No credentials, public share links, raw sharing policies or account-wide listings belong in committed evidence.
