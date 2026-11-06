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
* Definite non-timeout connection-establishment failures can retry within the
  same bounded budget when the complete request can be rebuilt. A consumed
  built stream cannot retry. Resets after sending, timeouts and malformed
  acknowledgements do not authorize replay: reconcile uncertain writes first.
* Streaming uploads on the provider-owned budget rebuild the body and retry
  a non-timeout connection refusal and an HTTP 429. A 429 waits for
  Retry-After when that header parses, otherwise the shared backoff. Any
  other HTTP response, including a 5xx, is not replayed by that rebuild.
  S3 single PUT also retries 503 SlowDown under the same bound, and leaves
  every other 503 in place. On Fastly Object Storage the single PUT and each
  multipart part also repeat a 408, which Fastly answers when a request ran
  past its 120 second limit and was not completed; the repeat carries the same
  key and bytes, so it stores nothing twice. GitLab's byte-backed commit stays on
  `send_with_retry`.

Streaming request rebuilders for pCloud, 4shared, Azure, kDrive, Koofr,
Twake, S3 single PUT, WebDAV single PUT and both Zoho routes retain the
original opened file through the shared cursor. Each retry waits for the
preceding reader before rewinding, so replacing the pathname during backoff
cannot select another file. This is not a snapshot against in-place edits.
Twake and S3 keep a separate digest for the attempt that completes.

The batch upload executor also respects this policy through the provider's
`StorageProvider::manages_upload_retries` contract (forwarded by compression/crypt
overlays), backed by `http_retry::manages_upload_retries_for(provider)`. It
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
| FileLu | Listing, info, download links and upload-endpoint lookup keep GET retries; the obsolete direct-link POST helper was removed by the versions integration | Create, clone/copy, version restore, version delete and other non-idempotent GET RPCs use refusals only; fixed-ID rename/move/set-folder and delete retain bounded recovery, while path-based rename/move do not replay 5xx. Multipart POST is not replayed after 5xx. Uploads use the native destination-folder field and preserve version history; there is no pre-delete or post-upload relocation. Fixed-ID delete retains existing bounded recovery. |
| 4shared | Signed GETs | Folder POST and other writes do not replay 5xx. Single-stream uploads recover bounded non-timeout connection-establishment failures and an explicit HTTP 429. A 429 waits for Retry-After when present, otherwise the shared backoff. Each attempt waits until the previous read has left the shared cursor, then streams the same open file from the start, and acquires the shared request-rate permit. Other responses, timeouts and failures after the body starts do not restart the upload. |
| Jottacloud | GET listing and reads | POST create, copy/move, restore/trash commands do not replay 5xx. The in-memory upload rebuilds its multipart body and retries a connection refusal before send and an HTTP 429. |
| kDrive | GET listing, metadata and downloads | POST mkdir, copy, share and restore/version commands do not replay 5xx. The streaming upload opens the file once and streams that cursor. It retries a connection refusal before send and an HTTP 429. |
| Koofr | GET listing, info, downloads | POST mkdir/share/restore and other writes do not replay 5xx. The streaming upload opens the file once and streams that cursor. It retries a connection refusal before send and an HTTP 429. |
| Azure | GET/HEAD | PUT create/copy and mutations do not replay ambiguous 5xx at the shared layer. Single Put Blob opens the file once and streams that cursor, signs a fresh date on each attempt, and retries a connection refusal before send and an HTTP 429. A non-timeout send error omits the request URL, so a SAS query stays out of the message. Put Block stays on the byte-body path. |
| S3 | GET/HEAD incl. range reads | PUT object/copy/part and POST multipart completion do not replay 5xx at the shared layer. The single PUT rebuilds and signs the stream and retries a connection refusal before send, an HTTP 429, and 503 SlowDown. Every other 503 stays on that response. Fastly Object Storage adds one case: the single PUT and each multipart part repeat a 408 (request not completed in time), bounded by the same allowance. The baseline digest belongs to the attempt that completed. Multipart parts stay on the signed byte-body path. |
| Swift | GET/HEAD | PUT object/container, POST metadata and DELETE do not replay 5xx. One retry after explicit 401 and reauthentication remains. |
| Internxt | Drive/network GET reads | POST/PATCH/PUT writes do not replay 5xx. Refresh GET rotates credentials, so refusals only. Upload-session allocation no longer has a second outer replay on 5xx, timeout or malformed success. All existing names, including empty files, are refused with AlreadyExists before allocation; there is no UUID replacement or delete/create fallback. A concurrent create refusal is AlreadyExists. One explicit 401 recovery remains. |
| Twake | GET listing and reads | POST mkdir/copy/share/restore and PUT/PATCH do not replay 5xx. Explicit 401 recovery remains. The file upload retries a connection refusal before send and an HTTP 429; one 401 refresh and one 409 lookup stay outside that retry. The digest compared with the server belongs to the attempt that completed. |
| pCloud | Read GETs, download links, checksums, revisions, trash listing | Mutating GET create/copy/upload-create/save, rename/move, links and trash restore/clear use refusals only. `createfolderifnotexists` retains retries because it conditionally returns the same folder. Single-stream uploads recover bounded connection-establishment failures and an explicit HTTP 429, rebuilding the multipart body from the same open file through the shared cursor. A token sent in the query is omitted from a build error and from a transport error. The cursor waits out an in-flight read before it rewinds. A 429 waits for Retry-After when present, otherwise the shared backoff. Other responses, timeouts and malformed acknowledgements do not restart the upload. Each of those attempts acquires the shared request-rate permit before it is sent. |
| GitLab | GET/HEAD incl. paginated reads | Commit POST and DELETE bypass the extra outer 5xx retry; other writes get the shared conservative default. |
| Filen | GETs retain recovery; directory content, path resolution and file-version POST queries explicitly retain safe recovery | Shared POST writes do not replay 5xx. An encrypted upload chunk retries a connection refusal before send and an HTTP 429, using the provider retry budget. Its separate encrypted-chunk download retry is read-only. |
| Zoho | GET listing | Shared writes get the conservative default. A provider-specific reconciliation loop must retain a single-attempt transport and establish whether the item exists before repeating a create. Both the small upload and the large stream upload retry a connection refusal before send and an HTTP 429. Each attempt takes a pacing turn and a fresh token. A 429 waits for WorkDrive's Retry-After, or 61 seconds when that header is absent. The connection refusal stays in the shared sender. |
| WebDAV / transfer DAG / multi-thread | Shared read/range GETs retain recovery | Specialized WebDAV part recovery is separate from this shared policy. The single PUT rebuilds the body and retries a connection refusal before send and an HTTP 429. One Digest 401 replay remains. |

## Evidence

Local HTTP fixtures cover committed-create-plus-503 for FileLu and pCloud,
one 503 for FileLu version restore and version delete, 4shared resending the
already-open file after its path is replaced, a 429 refusal then success for
pCloud and 4shared uploads,
whole-body single-attempt multipart upload without a false completion,
GitLab commit POST, Internxt session allocation after 503 and malformed 200,
safe POST query recovery, explicit 429 write refusal, mutating GET refusal-only,
stream preservation, a rebuilt upload retried after a refused connection
and after an HTTP 429 (a 500 stays on the first send, and the 429 budget
stops), and honest per-attempt first-byte timing. Provider suites
cover the affected call sites; live services are not mutated by these tests.

Backblaze's [native upload guidance](https://www.backblaze.com/docs/cloud-storage-upload-files-with-the-native-api) and [integration checklist](https://www.backblaze.com/docs/cloud-storage-integration-checklist) explicitly prescribe acquiring a fresh upload URL/token after a pod 5xx. That specialized policy is preserved; a lost success response still does not authorize another small-file POST.

The pCloud `upload_save` RPC remains refusal-only. The old source comment
claimed replay safety from a stable file ID; that is insufficient because
[pCloud preserves file IDs across overwrites](https://docs.pcloud.com/structures/filenames.html)
and [uploads can add revisions](https://docs.pcloud.com/methods/file/uploadfile.html).
The inspected [official client implementation](https://github.com/pcloudcom/console-client/blob/master/pCloudCC/lib/pclsync/pupload.c)
does not establish a no-extra-revision guarantee for replay after an ambiguous
HTTP response. The local committed-save/503 fixture verifies our single-attempt
policy, not the server's internal behavior. Internxt definite 4xx refusals retain
the sanitized reason; 5xx and unreadable successful allocations retain the
uncertain-outcome message.

FileLu setting/share mutations require an explicit API status 200 even when HTTP
is 200; missing status or a refused mutation is an error, not a reported change.
`server_side_copy` and `clone_file` require that acknowledgement and a clone
file code before either moves or renames. The code is read from a result object
or from the first element of a result array. A missing code is an error and
does not report a copy.
FileLu mutation diagnostics expose only HTTP/API status or numeric JSON error
location, not arbitrary response text that can echo a password or API key.

Internxt's [published replaceFile contract](https://github.com/internxt/sdk/blob/29ad01efb9d14e32edf7a3f5c06badbab10986ea/src/drive/storage/index.ts#L875-L884)
updates an existing UUID, without an expected destination name or parent.
The public server implementation also updates by UUID and user rather than a
client-supplied name/parent condition. A concurrent rename can therefore move the
selected file before that update. AeroFTP refuses any already-existing name with
AlreadyExists before upload allocation, including empty files. There is no UUID
update or delete/create fallback. This intentionally disables Internxt overwrites
until a conditional replacement contract is established. Creation of new names,
including empty files, remains available; a concurrent create conflict returns
AlreadyExists without replay. Both existing-name conflicts use CLI exit 9,
so callers can use their normal path-conflict handling while preserving the original.

For a new nonempty file, content storage precedes a single metadata create.
A failed create may leave an orphaned stored network object; an uncertain
acknowledgement returns an error instead of allocating or publishing again.
No atomic rename/replace capability is advertised, and no claim is made about
unobserved production transactions.
