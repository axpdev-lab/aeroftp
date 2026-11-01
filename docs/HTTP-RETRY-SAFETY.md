# HTTP retry safety

A replayable request body does not prove that the operation may be repeated.
A gateway error can arrive after a create, copy, revision or upload committed.
The shared `providers/http_retry.rs` transport returns ambiguous write responses
to the caller instead of repeating the write.

## Contract

* HTTP 429 is an explicit refusal: retain bounded Retry-After/backoff retries.
* By default, retry 500/502/503/504 only for GET, HEAD and OPTIONS.
* Use `for_safe_operation()` for an audited read-only POST or another operation
  whose repetition is safe. Never opt in just because the body can be cloned.
* Use `refused_only()` for mutating GET RPCs or credential rotation.
* A consumed stream in a built request cannot be replayed, including after 429.
  A rebuilding closure must recreate the same complete body for every attempt.
* Transport failures and malformed success responses do not authorize a create
  replay. Reconcile by an authoritative identity/idempotency key before doing so.

The batch upload executor also respects this policy through the provider's
`manages_upload_retries` contract (forwarded by compression/crypt overlays). It
must not multiply the retry budget by starting the whole upload again after a
5xx, timeout or malformed acknowledgement. The shared HTTP provider list lives
with the HTTP policy; specialized provider recovery still owns its own budget.

The conservative write default includes PUT/PATCH/DELETE: some APIs create a
revision for each PUT. This trades automatic recovery of uncertain writes for
avoiding duplicate objects or versions. Callers retain the original HTTP status;
this is not a promise that the failed write left the remote unchanged.

## Audited operations

| Provider / caller | Read recovery | Writes / special case |
| --- | --- | --- |
| B2 | Explicit safe POSTs: buckets, names, versions, unfinished files, upload URL and download authorization | Shared wrapper does not replay create/copy POSTs. Existing B2-specific upload recovery stays separate: multipart replaces the same file ID/part number; small uploads use their existing fresh-pod policy. |
| FileLu | Listing, info, download links and upload-endpoint lookup keep GET retries; legacy direct-link POST is explicitly a query | Create, clone/copy, rename/move, sharing, remote URL upload, password/privacy and restore GET RPCs use refusals only. Multipart POST is not replayed after 5xx. Fixed-ID delete retains existing bounded recovery. |
| 4shared | Signed GETs | Folder POST and other writes do not replay 5xx. |
| Jottacloud | GET listing and reads | POST create, copy/move, restore/trash commands do not replay 5xx. |
| kDrive | GET listing, metadata and downloads | POST mkdir, copy, share and restore/version commands do not replay 5xx. |
| Koofr | GET listing, info, downloads | POST mkdir/share/restore and other writes do not replay 5xx. |
| Azure | GET/HEAD | PUT create/copy and mutations do not replay ambiguous 5xx at the shared layer. |
| S3 | GET/HEAD incl. range reads | PUT object/copy/part and POST multipart completion do not replay 5xx at the shared layer. |
| Swift | GET/HEAD | PUT object/container, POST metadata and DELETE do not replay 5xx. One retry after explicit 401 and reauthentication remains. |
| Internxt | Drive/network GET reads | POST/PATCH/PUT writes do not replay 5xx. Refresh GET rotates credentials, so refusals only. Upload-session allocation no longer has a second outer replay on 5xx, timeout or malformed success. One explicit 401 recovery remains. |
| Twake | GET listing and reads | POST mkdir/copy/share/restore and PUT/PATCH do not replay 5xx. Explicit 401 recovery remains. |
| pCloud | Read GETs, download links, checksums, revisions, trash listing | Mutating GET create/copy/upload-create/save, rename/move, links and trash restore/clear use refusals only. `createfolderifnotexists` retains retries because it conditionally returns the same folder. |
| GitLab | GET/HEAD incl. paginated reads | Commit POST and DELETE bypass the extra outer 5xx retry; other writes get the shared conservative default. |
| Filen | GETs retain recovery; directory content, path resolution and file-version POST queries explicitly retain safe recovery | Shared POST writes do not replay 5xx. Its separate encrypted-chunk download retry is read-only. |
| Zoho | GET listing | Shared writes get the conservative default. A provider-specific reconciliation loop must retain a single-attempt transport and establish whether the item exists before repeating a create. |
| WebDAV / transfer DAG / multi-thread | Shared read/range GETs retain recovery | Specialized WebDAV part recovery is separate from this shared policy. |

## Evidence

Local HTTP fixtures cover committed-create-plus-503 for FileLu and pCloud,
whole-body single-attempt multipart upload without a false completion,
GitLab commit POST, Internxt session allocation after 503 and malformed 200,
safe POST query recovery, explicit 429 write refusal, mutating GET refusal-only,
stream preservation, and honest per-attempt first-byte timing. Provider suites
cover the affected call sites; live services are not mutated by these tests.

Backblaze's [native upload guidance](https://www.backblaze.com/docs/cloud-storage-upload-files-with-the-native-api) and [integration checklist](https://www.backblaze.com/docs/cloud-storage-integration-checklist) explicitly prescribe acquiring a fresh upload URL/token after a pod 5xx. That specialized policy is preserved; a lost success response still does not authorize another small-file POST.
