# AeroFTP Protocol Features Matrix

> Last Updated: 28 June 2026
> Version: v4.1.0
>
> **Note**: AeroFTP organizes integrations on three tiers:
>
> 1. **7 transport protocols** (FTP, FTPS, SFTP, WebDAV, S3, Azure Blob, OpenStack Swift) - native wire-level support;
> 2. **25+ native provider integrations** with dedicated OAuth2 / API key / SDK code paths (Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Filen, Zoho WorkDrive, Internxt, kDrive, Koofr, Jottacloud, FileLu, Yandex Disk, OpenDrive, 4shared, Drime, GitHub, GitLab, Immich, ImageKit, Uploadcare, Cloudinary, InfiniCLOUD, Felicloud);
> 3. **45+ pre-configured presets** in the Discover catalog: S3-compatible (MEGA S4, Filen S5, MinIO, Wasabi, Cloudflare R2, DigitalOcean, Tencent COS, Alibaba OSS, Oracle, Storj, IDrive e2, Hetzner, Yandex Object Storage, Quotaless, Backblaze B2-S3, Filen Desktop S3, S3Drive), WebDAV-compatible (Nextcloud, TAB.DIGITAL, Felicloud, Seafile, InfiniCLOUD, CloudMe, Jianguoyun, Koofr-WebDAV, FileLu-WebDAV, Yandex-WebDAV, OpenDrive-WebDAV, Quotaless-WebDAV, MEGAcmd, Filen Desktop WebDAV), and SFTP-based (SourceForge, GitHub via SFTP).
>
> The feature matrix tables below cover the core production set. GitHub, GitLab, Immich, ImageKit, Uploadcare, and Cloudinary have repository / media-specific semantics and are documented inline in their dedicated sections.

---

## AeroShare peer-to-peer transport (Beta, v4.1.0 preview)

In addition to the cloud transports above, AeroFTP v4.1.0 previews **AeroShare**, an end-to-end-encrypted peer-to-peer user-to-user transfer path built on iroh 1.0. Peers discover each other through the Mainline DHT and connect over federated relays, with no central AeroFTP server in the path. It is a Beta preview in v4.1.0.

---

## Portable devices (MTP / WPD)

<a id="portable-devices-mtp--wpd"></a>

AeroFTP can open **attached portable devices** (phones, cameras) that speak
**MTP** (Linux libmtp) or **WPD** (Windows) even when the OS never assigns a
drive letter. They appear under **PLACES → Portable devices**. You can also
**save a device as a My Servers profile** (Add Services → **Devices**): a green
attach indicator shows when that phone is plugged in, and click-to-connect
reopens a session at the saved default remote (and local) path. Identity uses a
stable **device fingerprint** (`mtp:serial=…` preferred, else vid/pid + model),
not a USB bus address.

CLI: `aeroftp-cli --profile "Sony backup" ls` (and other profile-backed verbs)
resolve the same fingerprint against currently attached devices.

### What works

| Capability | Support |
|------------|---------|
| Discover attached devices | Yes (when the OS backend can see them) |
| Browse storages and folders | Yes (virtual path: `/Storage/…`) |
| Copy whole files to/from device | Yes (progress callbacks; single transfer slot) |
| Disconnect / release session | Yes |
| Dual-panel local ↔ device | Yes (other panel stays local FS) |
| Saved My Servers device profiles | Yes (fingerprint match, green attach, click connect) |
| CLI `--profile` open when attached | Yes |

### Hard limits (protocol / OS, not AeroFTP gaps)

These are properties of MTP/WPD and common device firmware. AeroFTP will **not**
claim them as "coming soon" for this transport:

| Limit | Reality |
|-------|---------|
| No real mount / no drive letter | Device is not a block filesystem |
| No random access / range download | Object get/send is stream-oriented |
| No honest resume | Interrupted transfer restarts the whole object |
| No multipart upload | No S3-style part commit |
| Weak concurrency | Default `max_file_slots = 1` |
| Metadata best-effort | Size/mtime often missing or wrong |
| No delta / rsync | No block-checksum protocol on the wire |
| Exclusive USB claim | Only one program can hold the phone over MTP at a time |

**Exclusive claim (on-demand):** only one program can hold the phone over MTP at
a time. A saved device profile does not claim the USB until you Connect. On
**Linux**, Connect soft-releases the system gvfs/Nautilus MTP mount (then opens
libmtp) so you get the session without a manual unmount; disconnect drops only
AeroFTP and the desktop may quietly re-automount. Open the door for the
session, close it when you leave, no lasting fight with the file manager. If
open still fails, re-toggle File Transfer on the phone and retry.

**macOS** is out of scope until a Mac station and backend exist.

### Error framing

- Device gone / not attached: reconnect the USB cable, unlock, enable File
  Transfer mode, then open again from PLACES or My Servers (or retry CLI
  `--profile`).
- Open fails while cable is connected: another program likely holds the exclusive
  claim; close the system file manager and retry.
- Unsupported op: the portable device does not support that operation (MTP limitation).
- Transfer cancelled: incomplete objects on the device are removed when the API allows.

---

## Protocol Security Matrix

### Connection Security by Protocol

| Protocol | Encryption | Auth Method | Credential Storage | Host Verification |
|----------|-----------|-------------|-------------------|-------------------|
| **FTP** | None | Password | Universal Vault | N/A |
| **FTPS** | TLS/SSL (Explicit/Implicit) | Password | Universal Vault | TLS Certificate |
| **SFTP** | SSH (hybrid: russh + ssh2/SCP) | Password / SSH Key | Universal Vault | TOFU + known_hosts |
| **WebDAV** | HTTPS | Password (Basic + Digest RFC 2617) | Universal Vault | TLS Certificate |
| **S3** | HTTPS | Access Key + Secret | Universal Vault | TLS Certificate |
| **Google Drive** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **Dropbox** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **OneDrive** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **MEGA.nz** | Client-side AES | Password (Native API or MEGAcmd) | secrecy (zero-on-drop) | E2E Encrypted |
| **Box** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **pCloud** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **Azure Blob** | HTTPS | Shared Key HMAC / SAS | Universal Vault | TLS Certificate |
| **4shared** | HTTPS | OAuth 1.0 (HMAC-SHA1) | Universal Vault | TLS Certificate |
| **Filen** | Client-side AES-256-GCM | Password (PBKDF2) | secrecy (zero-on-drop) | E2E Encrypted |
| **Zoho WorkDrive** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **Internxt Drive** | Client-side AES-256-CTR | Password (PBKDF2 + BIP39) | secrecy (zero-on-drop) | E2E Encrypted |
| **kDrive** | HTTPS | API Token (Bearer) | Universal Vault | TLS Certificate |
| **Koofr** | HTTPS | OAuth2 PKCE | Universal Vault | TLS + CSRF State |
| **FileLu** | HTTPS | API Key | Universal Vault | TLS Certificate |
| **Yandex Disk** | HTTPS | OAuth2 Token | Universal Vault | TLS Certificate |
| **OpenDrive** | HTTPS | Session (Username/Password) | Universal Vault | TLS Certificate |
| **Jottacloud** | HTTPS | Bearer Token (auto-refresh 60s pre-expiry) | Universal Vault | TLS Certificate |
| **GitHub** | HTTPS | PAT / Device Flow (PEM keys vaulted, AES-256-GCM) | Universal Vault | TLS Certificate |
| **Immich** | HTTPS | API Key (`x-api-key` header) | Universal Vault | TLS Certificate |

### Security Features by Protocol

> Tables below group OAuth providers (Google Drive · Dropbox · OneDrive · Box · pCloud · Zoho WorkDrive · Koofr · Jottacloud · Yandex Disk) under a single "OAuth Providers" column when the security semantics are identical. Protocols with distinctive semantics (E2E encryption, OAuth 1.0, etc.) keep dedicated columns.

| Feature | FTP | FTPS | SFTP | WebDAV | S3 | OAuth Providers | MEGA | Box | pCloud | Azure | 4shared | Filen | Internxt | kDrive | FileLu | Yandex | OpenDrive |
|---------|-----|------|------|--------|-----|-----------------|------|-----|--------|-------|---------|-------|----------|--------|--------|--------|-----------|
| Insecure Warning | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| TLS/SSL | No | Yes | - | Yes | Yes | Yes | - | Yes | Yes | Yes | Yes | - | - | Yes | Yes | Yes | Yes |
| SSH Tunnel | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| Host Key Check | - | - | TOFU | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| PKCE Flow | - | - | - | - | - | Yes | - | Yes | Yes | - | - | - | - | - | - | - | - |
| Digest Auth (RFC 2617) | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - |
| Ephemeral Port | - | - | - | - | - | Yes | - | Yes | Yes | - | Yes | - | - | - | - | Yes | - |
| OAuth 1.0 Flow | - | - | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - |
| E2E Encryption | - | - | - | - | - | - | Yes | - | - | - | - | Yes | Yes | - | - | - | - |
| Memory Zeroize | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |

---

## File Operations Matrix

### Core Operations

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | Google Drive | Dropbox | OneDrive | MEGA | Box | pCloud | Azure | 4shared | Filen | Zoho WD | Internxt | kDrive | FileLu | Yandex | OpenDrive |
|-----------|-----|------|------|--------|-----|--------------|---------|----------|------|-----|--------|-------|---------|-------|---------|----------|--------|--------|--------|-----------|
| List | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Upload | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Download | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Delete | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Rename | Yes | Yes | Yes | Yes | Yes* | Yes | Yes | Yes | Yes | Yes | Yes | Yes** | Yes | Yes | Yes | Yes | Yes*** | Yes | Yes | Yes |
| Mkdir | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Chmod | Yes | Yes | Yes | No | No | No | No | No | No | No | No | No | No | No | No | No | No | No | No | No |
| Stat | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| Share Link | AeroCloud | AeroCloud | AeroCloud | AeroCloud | Yes | Yes | Yes | Yes | Yes | Yes | Yes | No | No | Yes | Yes | Yes | No | Yes | Yes | Yes |

*S3 rename = copy+delete
**Azure rename = copy+delete
***kDrive rename = move to same parent with new name

### Advanced Operations (v1.4.0)

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | GDrive | Dropbox | OneDrive | MEGA | Box | pCloud | Azure | 4shared | Filen | Zoho WD | Internxt | kDrive | FileLu | Yandex | OpenDrive |
|-----------|-----|------|------|--------|-----|--------|---------|----------|------|-----|--------|-------|---------|-------|---------|----------|--------|--------|--------|-----------|
| **Server Copy** | - | - | - | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | - | - | - | Yes | - | Yes | Yes† | Yes | - |
| **Remote Search** | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| **Storage Quota** | - | - | Yes | Yes | - | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes | Yes |
| **File Versions** | - | - | - | - | - | Yes | Yes | Yes | - | Yes | Yes | - | - | - | Yes | - | - | - | - | - |
| **Thumbnails** | - | - | - | - | - | Yes | Yes | Yes | - | Yes | Yes | - | - | - | - | - | - | - | - | - |
| **Permissions** | - | - | - | - | - | Yes | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - |
| **Locking** | - | - | - | - | - | - | - | - | - | Yes‡ | - | - | - | - | - | - | - | - | - | - |
| **Resume Transfer** | Yes | Yes | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - |
| **Resumable Upload** | - | - | - | - | Yes | Yes | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - |
| **Workspace Export** | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| **Change Tracking** | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| **MLSD/MLST** | Yes | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| **Speed Limit** | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - |
| **Import Link** | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - |
| **Multipart Upload** | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| **Remote URL Fetch** | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | Yes | - | - |
| **File Password** | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - | Yes | - | - |
| **Privacy Toggle** | - | - | - | - | - | - | - | - | - | - | - | - | Yes | - | - | - | - | Yes | - | Yes |
| **Tags** | - | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - |
| **Watermark** | - | - | - | - | - | - | - | - | - | Yes‡ | - | - | - | - | - | - | - | - | - | - |
| **Comments** | - | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - |
| **Collaborations** | - | - | - | - | - | - | - | - | - | Yes | - | - | - | - | - | - | - | - | - | - |

†FileLu Server Copy = server-side clone (`filelu_clone_file`)
‡Box Enterprise only (Business/Enterprise plan required)

---

## Checksums and Hashes

Which digests a backend can hand back **without downloading the file**, and which it cannot. AeroFTP never downloads a remote file in order to hash it: a digest either comes from metadata the backend already holds (an S3 ETag, a Drive `file.hashes` field, an ownCloud `oc:checksums` property) or from work the *server* does on its own disk (`sha256sum` over an SSH exec channel). Where a backend has nothing to offer, the Properties → Checksum tab says so instead of offering a button that cannot succeed.

**Local files** are different: AeroFTP reads them itself, so MD5, SHA-1, SHA-256, SHA-512 and BLAKE3 are all available for anything on your own disk, on every platform.

A backend's own scheme keeps its own name. Dropbox's `content_hash` is a SHA-256 over the SHA-256 of each 4 MiB block and GitHub's is a git blob SHA-1; neither equals a plain digest of the same bytes, so they are reported under their own labels rather than aliased onto SHA-256 and SHA-1, where they would silently fail every comparison.

<!-- BEGIN CHECKSUM-MATRIX -->
<!-- Generated from src-tauri/src/providers/checksum_matrix.rs. Do not edit by hand:
     `cargo test --lib checksum_matrix` fails when this block drifts, and
     `AEROFTP_UPDATE_DOCS=1 cargo test --lib checksum_matrix` rewrites it. -->

| Backend | Server-side digests | Notes |
| --- | --- | --- |
| FTP | MD5, SHA-1, SHA-256, CRC32 (negotiated) | Depends on what the server advertises in FEAT: `HASH` (algorithm chosen by the server), or the older `XMD5` / `XSHA1` / `XCRC`. A server advertising none offers no digest. |
| FTPS | MD5, SHA-1, SHA-256, CRC32 (negotiated) | Depends on what the server advertises in FEAT: `HASH` (algorithm chosen by the server), or the older `XMD5` / `XSHA1` / `XCRC`. A server advertising none offers no digest. |
| SFTP | SHA-256 | Computed by the remote host with `sha256sum` over an SSH exec channel: the bytes are read on the server, never sent to us. Omitted if the host has no `sha256sum`. |
| WebDAV | SHA-1, MD5, Adler-32 (negotiated) | Only ownCloud and Nextcloud publish the `oc:checksums` property, and only for files uploaded by a client that sent one. Every other WebDAV server omits it. |
| S3 | MD5 | The ETag is the object MD5 only for single-part, non-SSE-KMS objects. For a multipart or KMS-encrypted object the digest is omitted rather than guessed. |
| Azure Blob | - |  |
| Swift | - |  |
| Google Drive | MD5, SHA-1, SHA-256 | Native Google Workspace documents have no byte stream and therefore no digest. |
| OneDrive | SHA-1, SHA-256, QuickXorHash | Which of the three is present depends on the account: personal OneDrive publishes QuickXorHash, business and SharePoint typically SHA-1 and SHA-256. |
| Dropbox | Dropbox content hash | Not a plain file digest: a SHA-256 over the SHA-256 of each 4 MiB block. Comparable only against another Dropbox content hash. |
| Box | SHA-1 |  |
| pCloud | MD5, SHA-1, SHA-256 |  |
| Yandex Disk | MD5 |  |
| Koofr | Koofr hash | Koofr's own opaque hash. Comparable only against another Koofr hash. |
| Backblaze B2 | SHA-1 | Large files and uploads B2 recorded as `unverified:` carry no usable `contentSha1`; those are omitted. |
| GitHub | Git blob SHA-1 | The git blob SHA-1, computed over `blob <len>\0` + content, so it does not match a plain SHA-1 of the same file. |
| GitLab | - |  |
| Immich | SHA-1 | The asset checksum Immich records at upload time. |
| MEGA | - |  |
| Filen | - |  |
| Internxt Drive | - |  |
| Zoho WorkDrive | - |  |
| kDrive | - |  |
| Jottacloud | - |  |
| Drime Cloud | - |  |
| FileLu | - |  |
| 4shared | - |  |
| OpenDrive | - |  |
| Google Photos | - |  |
| ImageKit | - |  |
| Uploadcare | - |  |
| Cloudinary | - |  |
| MTP | - |  |

<!-- END CHECKSUM-MATRIX -->

### Inside the Overlays Path

Under a crypt overlay (AeroCrypt or rclone `crypt` interop) the server stores ciphertext, so a server-side digest covers the encrypted bytes and **will not match a hash of your local plaintext file**. That is not a failure, it is what encryption means: the digest is still a valid answer to "has the stored object changed", and AeroFTP labels it as covering the stored ciphertext wherever it shows it.

For an integrity check against your local files inside an overlay, use `aeroftp-cli crypt verify`, which stream-decrypts the remote objects (without writing plaintext to disk) and compares the decrypted content.

The sync engine never uses these digests: `supports_checksum()` stays false for an overlay, precisely so a comparison of ciphertext-vs-plaintext hashes cannot mark every unchanged file as different.

---

## Share Link Support

| Protocol | Link | Password | Expiry | Permissions | Notes |
|----------|------|----------|--------|-------------|-------|
| **FTP/FTPS/SFTP** | AeroCloud | - | - | - | Requires `public_url_base` config |
| **WebDAV/Nextcloud** | Native | Auto-gen | Days | - | OCS API, password auto-generated if server enforces it |
| **S3** | Presigned | - | Yes (default 1h, max 7d) | - | AWS Signature V4 |
| **Google Drive** | Native | - | - | view/comment/edit | Permanent "anyone with link" |
| **Dropbox** | Native | Pro+ | Pro+ | view/edit | Password and expiry require Professional plan or higher |
| **OneDrive** | Native | Personal | Yes | view/edit | Password only on Personal accounts, not Business |
| **MEGA.nz** | Native | - | - | - | Native API (recommended) or MEGAcmd bridge |
| **Box** | Native | Paid | Paid | view/edit | Password and expiry require paid Box plan |
| **pCloud** | Native | Premium | Premium | - | Password and expiry require pCloud Premium |
| **Filen** | Native | Yes (Argon2id) | Presets (1h-30d) | - | E2E encrypted, password hashed client-side |
| **Zoho WorkDrive** | Native | Yes | Yes | view/edit | Full advanced options on all plans |
| **kDrive** | Native | Paid | Paid | view/edit | Password and expiry require paid Infomaniak plan |
| **Jottacloud** | Native | - | - | - | Basic share only (API limitation) |
| **Drime Cloud** | Native | Yes | Yes | view/edit | Full advanced options |
| **Koofr** | Native | - | - | - | Basic share link |
| **FileLu** | Native | - | - | - | Makes file public, returns share URL |
| **Yandex Disk** | Native | - | - | - | Publish/unpublish via REST API |
| **OpenDrive** | Native | - | Yes (default 1 year) | - | Expiring share links |
| **MEGA S4** | Presigned | - | Yes (max 7d) | - | S3-compatible presigned URLs, 4 regions (EU/CA) |
| **Azure Blob** | SAS | - | Yes (default 7d) | - | Service SAS with HMAC-SHA256 |
| **GitHub** | URL | - | - | - | Permanent blob/release URL |

**CLI**: `aeroftp-cli link --profile "name" /path [--password pw] [--expires 7d] [--permissions edit]`

---

## Privacy and Visibility Controls

This section explains what "privacy" means in practice for each provider that exposes per-item visibility controls, and what AeroFTP's `Set as Private` / `Set as Public` actions actually do at the API level. See also the **Privacy Toggle** row in the Advanced Operations matrix.

### What contributes to privacy in a cloud-sync app

When evaluating how private your data is on a given provider, the following independent aspects all matter:

- **Encryption at rest**: whether the provider can read your files, or whether the encryption keys never leave your device (zero-knowledge / E2E). AeroFTP's MEGA, Filen, and Internxt integrations are end-to-end encrypted; all the others store cleartext server-side.
- **Default visibility on create**: whether a newly-created file or folder is private by default, hidden, or world-accessible. Some providers (notably 4shared, designed around community sharing) default to public.
- **Granular per-file / per-folder visibility control**: whether you can toggle individual items between visibility levels after creation, and how many levels are available (binary public/private vs three-level public/hidden/private).
- **Share-link semantics**: whether shared links can be password-protected, time-limited, and scoped (view / download / edit). See the Share Link Support matrix above.
- **IP-level access controls and audit logs**: whether the provider exposes IP allow/deny lists and access logs (mostly an enterprise feature on Box / Zoho WorkDrive).
- **AeroVault overlay**: AeroFTP's built-in encrypted container format (AES-256-GCM-SIV + Argon2id, see "Client-Side Encryption" below). Storing an `.aerovault` file on any provider makes that provider's per-item privacy semantics irrelevant for the actual data, since the underlying provider only ever sees opaque ciphertext.

### Provider-specific behavior

#### OpenDrive

OpenDrive's REST API exposes three documented visibility levels, surfaced as `RemoteEntry.permissions` in AeroFTP and visible in the file Properties dialog (Permissions tab) and the right-click context menu:

| Level | Meaning | API value |
|-------|---------|-----------|
| `private` | Owner-only; no public link works. | `Access=0` |
| `public` | Anyone with the link can access; item is searchable on OpenDrive. | `Access=1` |
| `hidden` | Accessible by direct link only; not searchable. | `Access=2` |

The context menu actions `Set as Private` and `Set as Public` call `file/access.json` and `folder/setaccess.json` (with `with_child_files=true` for folders, so children inherit the new state). The inapplicable action is hidden, not greyed out, so right-clicking a private item shows only `Set as Public` and vice versa. When the current state is `hidden`, both actions are shown.

`Set as Private` and `Set as Public` are the simple binary toggles available directly from the context menu. A separate three-level chooser (Public / Private / Hidden) is reachable via the `Privacy...` entry on the same menu and via the Properties dialog. The richer multi-axis flags (`folder_public_upl`, `folder_public_display`, `folder_public_dnl`) accepted by `folder/create.json` are not exposed on `folder/setaccess.json` and therefore cannot be changed after creation; they only apply when a new folder is being created.

Default visibility on create follows the OpenDrive account setting (typically `hidden`).

**OpenDrive REST API vs WebDAV**: OpenDrive enforces per-IP rate limits on its REST API (`session/login.json` and related endpoints) that can lead to temporary IP blacklisting under heavy or repeated authentication attempts. The WebDAV endpoint (`https://webdav.opendrive.com`) is more permissive in this regard. If you see authentication failures that resolve only after waiting several minutes, switching to the WebDAV preset (also available in AeroFTP) is a workable fallback. The reverse trade-off applies for per-item privacy controls, which are only available on the REST API path.

#### 4shared

4shared is community-oriented and creates files and folders as **public** by default. The `Set as Private` / `Set as Public` actions toggle the `owner_only` flag on the REST API and surface in the right-click menu (only the inapplicable action is hidden). 4shared has no `hidden` level; visibility is binary.

The Quick Connect "Default privacy" toggle for 4shared lets you flip the default to `private` so that uploads no longer land in 4shared's public catalog.

#### FileLu

FileLu exposes a binary `only_me` flag per file (private / public). Toggling this is the `Set as Private` / `Set as Public` action; the inapplicable action is hidden. Folders carry a separate Folder Settings dialog (FileDrop for anonymous uploads, Public Folder for shared catalog membership) and an optional per-file or per-folder password. See "FileLu Special Features" below for the full surface.

#### Filen and Internxt

Both providers are **zero-knowledge end-to-end encrypted**: visibility is implicitly private at all times since the provider cannot read the cleartext. Public sharing is opt-in, file-by-file, by generating a share link. Internxt additionally encrypts file names; Filen encrypts both content and file names.

#### MEGA

MEGA is end-to-end encrypted; the provider cannot read your data. Per-item privacy is binary: an item is private unless you mint a public link (which embeds the decryption key in the URL fragment). The MEGAcmd bridge has the same semantics; the AeroFTP MEGAcmd preset is a path-based wrapper that delegates to the native MEGA CLI.

#### Box, Google Drive, Dropbox, OneDrive, pCloud, Zoho WorkDrive, kDrive

These providers do not expose a per-item public/private toggle in the same shape; their privacy model is anchored to share links (Share Link Support matrix above) and, on Box and Zoho WorkDrive, to enterprise access policies (Box Enterprise: watermarks, folder locks, collaborations; Zoho: team labels). On Google Drive, the closest analog is the `starred` flag plus the granular share permissions (`view` / `comment` / `edit`) settable per-collaborator.

---

## Restricted Filename Handling (v4.0.4)

Several providers reject filenames that contain characters their storage layer treats as reserved, which previously surfaced as a silent rename or upload failure. AeroFTP handles this in two ways depending on the provider:

- **Reversible encoding** (Box, Dropbox, Jottacloud, OpenDrive): a filename containing a reserved character is transparently encoded before it reaches the provider and decoded back when the entry is listed, so the name you see in AeroFTP is always the original. The scheme is a faithful port of the rclone `lib/encoder` mapping (fullwidth substitutes for the reserved set, control-picture substitutes for control characters, a `QuoteRune` escape for the collision case, and position-dependent rules for a trailing space or a whole-name dot), and is property-tested so that `decode(encode(name)) == name` over tens of thousands of generated cases per provider. The reserved set is per provider (for example Dropbox and Box reserve `\`, Jottacloud reserves `" * : < > ? | %`, OpenDrive reserves `" * : < > ? \ |`); control characters are always encoded.

- **Clear rejection** (control-character-only providers): where a provider only forbids control characters and a reversible mapping would add no value, the operation fails with a clear, localized error naming the offending character rather than silently dropping the file.

The encode/decode runs once at the provider boundary, so it applies uniformly across the GUI, every CLI command, CLI `sync` and `benchmark`, the transfer engine and the session manager. No setting controls it; the behavior is always on.

---

## FileLu Special Features (v2.7.0)

FileLu exposes privacy and management features beyond generic file operations:

| Feature | API Endpoint | Tauri Command | Notes |
|---------|-------------|---------------|-------|
| **File Password** | `/file/set_password` | `filelu_set_file_password` | Set or remove password on any file |
| **Privacy Toggle** | `/file/only_me` | `filelu_set_file_privacy` | Toggle private (only-me) or public visibility |
| **Server Clone** | `/file/clone` | `filelu_clone_file` | Duplicate file server-side; returns share URL |
| **Folder Password** | `/folder/set_password` | `filelu_set_folder_password` | Requires `fld_token` from folder listing |
| **Folder Settings** | `/folder/setting` | `filelu_set_folder_settings` | FileDrop (anonymous uploads) + Public Folder |
| **List Trash** | `/files/deleted` | `filelu_list_deleted` | Returns deleted files with timestamps |
| **Restore File** | `/file/restore` | `filelu_restore_file` | Restore by `file_code` |
| **Restore Folder** | `/folder/restore` | `filelu_restore_folder` | Restore by `fld_id` |
| **Permanent Delete** | `/file/remove` | `filelu_permanent_delete` | Bypass trash, irrecoverable |
| **Remote URL Upload** | `/upload/url` | `filelu_remote_url_upload` | FileLu fetches file from URL server-side |

## Media Providers (ImageKit, Uploadcare, Cloudinary)

Three image / media-CDN integrations now share a dedicated tier in IntroHub Discover and CrossProfile. They use the standard `StorageProvider` trait for list / upload / download / delete / mkdir / rename, and add CDN-specific metadata (transformation URLs, public IDs, version history) on top.

| Capability | ImageKit | Uploadcare | Cloudinary |
|------------|----------|------------|------------|
| Shipped | v3.7.2 (23rd protocol) | v3.7.2 (24th protocol) | v3.7.4 (25th protocol) |
| Auth | Private key (HTTP Basic) | Public + Secret key | Cloudname + API key + secret |
| REST endpoint | `api.imagekit.io` | `api.uploadcare.com` (REST) + `upload.uploadcare.com` (multipart) | `api.cloudinary.com/v1_1/{cloud_name}/` |
| Free tier | 20 GB media + 20 GB BW/month | 1 GB media + 5 GB BW/month | 25 GB / 25k transformations / 25 GB BW (free plan) |
| Folder semantics | Native folders | Cursor-based listing, store-once mapped to directory model | Asset folders (paid) + tag-based pseudo-folders (all plans) |
| Listing | `/files` with `type=all`, `null_to_default` deserializer | Cursor pagination | `/resources/by_asset_folder` and `/folders` |
| Upload path | `/files/upload` (multipart) | `/base/` (multipart) + `/files/{uuid}/storage/` to commit | `/resources/{type}/upload` |
| CDN transformations | Passthrough URL parameters | Effects pipeline | URL-based transformations (`f_auto`, `q_auto`, etc.) |
| Used as | Media-CDN + storage | EU-friendly media storage | Image / video CDN + AI media services |

> Activity Log "Authenticated as ..." now renders public account identifiers (ImageKit URL endpoint, Uploadcare project, Cloudinary cloudname) in the clear instead of being mis-masked as a 3-char prefix; the credential masker detects `https://...` and returns `host + pathname` unmasked.

---

## Nextcloud-as-a-Service Presets

Nextcloud-compatible WebDAV stacks share a single backend (WebDAV + OCS) and ship as discrete presets so users see provider-specific branding, signup links, and trust signals in Discover.

| Preset | Status | Notes |
|--------|--------|-------|
| **Nextcloud** | Stable | Self-hosted WebDAV + OCS, auto-discovery of `/remote.php/dav/files/{username}/` (v3.5.0) |
| **Felicloud** | Stable | EU-based Nextcloud SaaS (v3.1.6), share links + trash via OCS API |
| **TAB.DIGITAL** | Stable | EU / GDPR Nextcloud SaaS (v3.7.4), 8 GB free, OCS badge, Felicloud-parity Edit form, healthCheckUrl preset |
| **Jianguoyun** | Stable | China-based WebDAV |
| **InfiniCLOUD** | Stable | Japan-based Nextcloud-derivative, 25 GB free, REST v2 (Muramasa) auto-discovery + real-time quota (v3.7.0) |
| **Seafile** | Stable | Self-hosted or cloud, WebDAV via `seafdav` endpoint |
| **CloudMe** | Stable | Swedish, 3 GB free, Digest auth (auto-detected) |

---

## Box Special Features (v2.7.4)

Box exposes management and collaboration features beyond generic file operations:

| Feature | API Endpoint | Tauri Command | Notes |
|---------|-------------|---------------|-------|
| **Trash Management** | `/folders/trash/items` | `box_list_trash` | Paginated listing with trashed_at timestamps |
| **Soft Delete** | `DELETE /files/{id}` | `box_trash_files` | Moves to trash (recoverable) |
| **Restore from Trash** | `POST /files/{id}` | `box_restore_from_trash` | Supports file and folder types |
| **Permanent Delete** | `DELETE /files/{id}/trash` | `box_permanent_delete` | Irrecoverable deletion |
| **Move Item** | `PUT /files/{id}` | `box_move_file` | Server-side move between folders |
| **Tags** | `PUT /files/{id}` | `box_set_tags` | Free-text tags, shown as inline chips |
| **Comments** | `/files/{id}/comments` | `box_add_comment` / `box_delete_comment` | File-level comments |
| **Collaborations** | `/collaborations` | `box_add_collaboration` / `box_remove_collaboration` | Role-based sharing |
| **Watermark** ‡ | `/files/{id}/watermark` | `box_set_watermark` / `box_remove_watermark` | Enterprise: viewer email overlay |
| **Folder Lock** ‡ | `/folder_locks` | `box_lock_folder` / `box_unlock_folder` | Enterprise: prevent move/delete |

‡ Requires Box Business or Enterprise plan

---

## Archive Support Matrix (v4.1.2)

Compression levels use the 7-Zip canonical presets (Store=0, Fastest=1, Fast=3, Normal=5, Maximum=7, Ultra=9); formats whose codec has no store mode omit Store. Default is Normal (5).

| Format | Compress | Extract | Browse | Selective Extract | Encryption | Levels | Backend |
|--------|----------|---------|--------|-------------------|------------|--------|---------|
| **ZIP** | Yes (Store/Deflate) | Yes (+ BZip2, LZMA, Deflate64, Zstd, XZ members) | Yes | Yes | AES-256 (read+write) | Store/Fastest/Fast/Normal/Maximum/Ultra | `zip` v8 |
| **7z** | Yes (LZMA2/LZMA/PPMd/BZip2 + dictionary, solid, threads) | Yes | Yes | Yes | AES-256 + encrypted header (read+write) | Fastest/Fast/Normal/Maximum/Ultra | `sevenz-rust2` v0.21 |
| **TAR** | Yes | Yes (safe symlinks) | Yes | Yes | No | - | `tar` v0.4 |
| **TAR.GZ** | Yes | Yes | Yes | Yes | No | Fastest/Fast/Normal/Maximum/Ultra | `tar` + `flate2` v1.0 |
| **TAR.XZ** | Yes | Yes | Yes | Yes | No | Fastest/Fast/Normal/Maximum/Ultra | `tar` + `xz2` v0.1 |
| **TAR.BZ2** | Yes | Yes | Yes | Yes | No | Fastest/Fast/Normal/Maximum/Ultra | `tar` + `bzip2` v0.6 |
| **GZ** (standalone) | Yes | Yes | - (single stream) | - | No | Fastest/Fast/Normal/Maximum/Ultra | `flate2` v1.0 |
| **XZ** (standalone) | Yes | Yes | - (single stream) | - | No | Fastest/Fast/Normal/Maximum/Ultra | `xz2` v0.1 |
| **BZ2** (standalone) | Yes | Yes | - (single stream) | - | No | Fastest/Fast/Normal/Maximum/Ultra | `bzip2` v0.6 |
| **RAR** | No | Yes | Yes | Yes | Password support | - | `unrar` v0.5 |

**Standalone GZ / XZ / BZ2** (v4.1.2): a single file compresses to and extracts from a plain `.gz` / `.xz` / `.bz2` with no tar wrapper (single-stream codecs, offered only for a lone non-folder file; nothing to browse). The member name on extract is the archive name minus the codec extension.

**7z Advanced options** (v4.1.2): content method (LZMA2 default, plus LZMA, PPMd, BZip2), LZMA2 dictionary size, thread count, and a solid-block option, in the Compress dialog and as `aeroftp compress` flags. Every method round-trips through the extractor.

**Safe tar links** (v4.1.2): symlink and hardlink entries are validated with the same in-root check as file paths; an in-root symlink is recreated (unix), a link pointing outside the extraction root is never materialised and is surfaced in the report.

**Multi-volume parts** (v4.1.2): a split part (`.7z.001`, `.zip.001`, `.z01`, `.r00`) gets a clear "rejoin the volumes" message instead of a generic error; multi-part RAR (`.partN.rar`) extracts normally (unrar follows co-located volumes).

**Archive Browser** (v1.7.0): Browse archive contents in-app without extracting. Password dialog for encrypted ZIP/7z/RAR. Selective extraction of individual files.

**CompressDialog** (v1.7.0): Unified compression UI with format selection, compression levels, editable archive name, password protection (ZIP/7z), and file info display.

---

## Client-Side Encryption (v1.8.0)

### AeroVault v2 - Military-Grade Containers

| Component | Algorithm | RFC/Standard | Notes |
|-----------|-----------|--------------|-------|
| **Content encryption** | AES-256-GCM-SIV | RFC 8452 | Nonce misuse-resistant AEAD |
| **Key wrapping** | AES-256-KW | RFC 3394 | Integrity-protected key encapsulation |
| **Filename encryption** | AES-256-SIV | RFC 5297 | Deterministic, hides file names |
| **Key derivation** | Argon2id | IETF draft | 128 MiB / 4 iterations / 4 parallelism |
| **Header integrity** | HMAC-SHA512 | RFC 2104 | 512-bit MAC, detects tampering |
| **Cascade mode** | ChaCha20-Poly1305 | RFC 8439 | Optional double encryption |
| **Chunk size** | 64 KB | - | Per-chunk nonce + auth tag |

### AeroVault v2 vs Cryptomator

| Feature | AeroVault v2 | Cryptomator |
|---------|--------------|-------------|
| **Nonce misuse resistance** | Yes (GCM-SIV) | No (GCM) |
| **KDF memory** | 128 MiB | 64-128 MiB |
| **KDF algorithm** | Argon2id | scrypt |
| **Header integrity** | SHA-512 | SHA-256 |
| **Cascade encryption** | Optional | No |
| **Chunk size** | 64 KB | 32 KB |

### AeroVault v3 / v4 (current default)

AeroVault v3 (AEROVAULT3) is the current default container format on the CLI (`vault create` defaults to `--vault-version v3`; v1 and v2 remain selectable). It restructures the container as a content-addressed store: small files are packed together, the data is split with gear content-defined chunking, each chunk is compressed with zstd and then encrypted with AES-256-GCM-SIV, chunks are identified by a keyed-BLAKE3-128 id (deduplication), and an authenticated manifest plus block table indexes the result. See [AEROVAULT-V3-SPEC.md](AEROVAULT-V3-SPEC.md) for the full format.

AeroVault v4 is v3 plus a Reed-Solomon error-correction wrapper (`reed-solomon-erasure`, K=10 / P=2 grid) for self-healing vaults; enable it with `vault create --error-correction` (alias `--ec`) and maintain it with `vault scrub` / `vault repair`.

Existing vaults can be repacked between v2 and v3 (keeping the same password) via the GUI "Change Mode" action or the CLI `aeroftp-cli vault change-mode <path> --security-level <standard|advanced|paranoid|archive>` command, where `standard`/`advanced`/`paranoid` target v2 and `archive` targets v3. The AeroVault v3 engine and the v4 error-correction layer ship in the published `aerovault` crate, with the app keeping thin wrappers.

### Cryptomator (Format 8)

Create and browse Cryptomator vaults via context menu:

| Component | Algorithm |
|-----------|-----------|
| Master key derivation | scrypt (N=2^15, r=8, p=1) |
| Key wrapping | AES-256-KW (RFC 3394) |
| Filename encryption | AES-SIV |
| Content encryption | AES-256-GCM (32KB chunks) |

---

## FTP Protocol Enhancements (v1.4.0)

### MLSD/MLST (RFC 3659)
- **FEAT detection**: Server capabilities checked on connect
- **MLSD listings**: Machine-readable format preferred over LIST
- **MLST stat**: Single-file info without listing parent directory
- **Automatic fallback**: Falls back to LIST when MLSD not supported
- **Fact parsing**: type, size, modify, unix.mode, unix.owner/group, perm

### Resume Transfers (REST)
- **resume_download**: REST offset + RETR for partial downloads
- **resume_upload**: APPE (append) for partial uploads

### FTPS TLS Encryption (v1.4.0)
- **Explicit TLS (AUTH TLS)**: Upgrades plain connection on port 21 via `into_secure()`
- **Implicit TLS**: Direct TLS connection on port 990
- **Explicit if available**: Attempts AUTH TLS, falls back to plain FTP if unsupported
- **Certificate verification**: Configurable per-connection. When verification is enabled, invalid, self-signed, or hostname-mismatched certificates are rejected. Disabling verification explicitly accepts those certificates and weakens MITM protection.
- **Backend**: suppaftp v8 with `tokio-async-native-tls` feature

**Default changed in v1.5.0**: FTP now defaults to 'explicit_if_available' (TLS opportunistic) instead of plain FTP

---

## Directory Sync (v1.5.2)

Bidirectional directory synchronization compares local and remote files by timestamp and size, then uploads/downloads as needed.

### Sync Support by Protocol

| Protocol | Compare | Upload | Download | Progress | Notes |
|----------|---------|--------|----------|----------|-------|
| **FTP** | Yes | Yes | Yes | Yes | Via provider/session pipeline |
| **FTPS** | Yes | Yes | Yes | Yes | Via provider/session pipeline |
| **SFTP** | Yes | Yes (ssh2/SCP) | Yes | Yes | Uploads via ssh2/SCP fallback (russh write bug) |
| **WebDAV** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **S3** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Google Drive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Dropbox** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **OneDrive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **MEGA** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Box** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **pCloud** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Azure Blob** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **4shared** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Filen** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Zoho WorkDrive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Internxt Drive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **kDrive** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **Koofr** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |
| **FileLu** | Yes | Yes | Yes | Yes | Via `StorageProvider` trait |

> For transfer-scheduler capabilities per protocol (parallel streams, chunked/segmented/delta paths, session pooling), see [CLI-GUIDE: Transfer Capabilities by Protocol](CLI-GUIDE.md#transfer-capabilities-by-protocol). That matrix is the single source and is not duplicated here.

### Sync Modes
- **Remote → Local**: Download newer remote files
- **Local → Remote**: Upload newer local files
- **Bidirectional**: Sync in both directions (default)

### Comparison Options
- Timestamp comparison (2-second tolerance for filesystem differences)
- File size comparison
- Configurable exclude patterns (`node_modules`, `.git`, `.DS_Store`, etc.)

### Sync Index Cache (v1.5.3)
Persistent JSON index stored at `~/.config/aeroftp/sync-index/` enables:
- **True conflict detection**: Both sides changed since last sync → Conflict status
- **Faster re-scans**: Unchanged files detected via cached size/mtime without full comparison
- **Per-path-pair storage**: Stable filename generated from hash of local+remote path pair
- **Auto-save after sync**: Index updated with final file states after successful sync

### FTP Transfer Retry (v1.5.3)
- Automatic retry with exponential backoff (3 attempts, 500ms base delay)
- Targets "Data connection" errors specifically
- FTP-only (cloud providers handle retries internally)
- Inter-transfer delay increased to 350ms for server stability

---

## Provider Keep-Alive (v1.5.1)

All non-FTP providers receive periodic keep-alive pings to prevent connection timeouts during idle sessions. This applies to WebDAV, S3, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, 4shared, Filen, Zoho WorkDrive, Internxt Drive, kDrive, Koofr, and FileLu.

---

## WebDAV Presets (v1.5.1)

| Preset | Status | Notes |
|--------|--------|-------|
| **Koofr** | Stable | EU-based, 10 GB free |
| **Jianguoyun** | Stable | China-based WebDAV |
| **InfiniCLOUD** | Stable | Japan-based, 25 GB free. Native REST API v2 integration: auto-discovery, real-time quota |
| **CloudMe** | Stable | Swedish, 3 GB free, Digest auth (auto-detected) |
| **Nextcloud** | Beta | Self-hosted WebDAV |
| **Seafile** | Stable | Self-hosted or cloud, WebDAV via `seafdav` endpoint |

---

## New Cloud Providers (v1.5.0)

### Box (Beta)
- OAuth2 PKCE via OAuth2Manager
- API: `https://api.box.com/2.0/`, upload: `https://upload.box.com/api/2.0/`
- ID-based file system (root folder = "0"), path→ID cache
- Share links, storage quota, file versions

### pCloud (Beta)
- OAuth2 via OAuth2Manager
- API: `https://api.pcloud.com/` (US) or `https://eapi.pcloud.com/` (EU)
- Path-based REST API (simplest of all providers)
- Share links, storage quota

### Azure Blob Storage (Beta)
- Shared Key HMAC-SHA256 or SAS token authentication
- API: `https://{account}.blob.core.windows.net/{container}/`
- Flat namespace with `/` delimiter (like S3)
- XML response parsing via quick-xml

### 4shared (v2.0.5)
- Native REST API v1.2 with OAuth 1.0 (HMAC-SHA1) authentication
- ID-based file system with folder/file caching
- API: `https://api.4shared.com/v1_2/`
- Upload: `https://upload.4shared.com/v1_2/files`
- 15 GB free storage, storage quota support

### Filen (Beta)
- Zero-knowledge E2E encryption: PBKDF2(SHA512, 200k iterations) + AES-256-GCM
- All metadata and file content encrypted client-side
- Chunk-based upload/download (1MB chunks)
- API: `https://gateway.filen.io/`

### Zoho WorkDrive (v2.4.0, labels/versioning v2.7.4)
- OAuth2 PKCE with 8 regional endpoints (US/EU/IN/AU/JP/UK/CA/SA)
- Team-based storage with team ID auto-detection
- Share links, trash management, storage quota
- Labels: team-level color labels, add/remove from files (v2.7.4)
- Versioning: list/download/restore file versions (v2.7.4)
- API: `https://www.zohoapis.{region}/workdrive/api/v1/`

### Internxt Drive (v2.6.0)
- Zero-knowledge E2E encryption: AES-256-CTR with BIP39 mnemonic key derivation
- Auth: PBKDF2-SHA1 + AES-CBC encrypted password + JWT
- Client-side file encryption/decryption, plainName metadata
- API: `https://drive.internxt.com/`

### Infomaniak kDrive (v2.6.0)
- Swiss-hosted cloud storage (GDPR + FADP compliant)
- Auth: Bearer API Token (generated from Infomaniak dashboard)
- ID-based file system, cursor-based pagination, server-side copy
- API: `https://api.infomaniak.com/`

### Koofr (v2.8.0)

- EU-based cloud storage (Slovenia) with 10 GB free
- Auth: OAuth2 PKCE (authorization code flow)
- Native REST API with full `StorageProvider` trait implementation
- Trash management: list, restore, empty trash
- Also available as WebDAV preset for standard protocol access
- API: `https://app.koofr.net/api/v2/`

### FileLu (v2.7.0)

- Privacy-focused cloud storage with 20 GB free
- Auth: API Key (generated from Account Settings → Developer API Key)
- Native REST API with full `StorageProvider` trait implementation
- FTP/FTPS/WebDAV/S3-compatible access also available as separate presets
- Special features: file/folder password, private/public toggle, server-side clone, trash management, remote URL upload
- API: `https://api.filelu.com/`

---

## AeroAgent AI (v1.6.0)

### AI Provider Support

| Provider | Native Tools | Streaming | Token Counting | Auth |
|----------|-------------|-----------|----------------|------|
| **Google Gemini** | Yes (`functionDeclarations`) | Yes (SSE) | Yes (`usageMetadata`) | API Key |
| **OpenAI** | Yes (`tools[]`) | Yes (SSE) | Yes (`usage`) | API Key |
| **Anthropic** | Yes (`tool_use`) | Yes (`content_block_delta`) | Yes (`usage`) | API Key |
| **xAI (Grok)** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **OpenRouter** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Ollama** | No (text fallback) | Yes (NDJSON) | Yes (`eval_count`) | None |
| **Kimi (Moonshot)** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Qwen (Alibaba)** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **DeepSeek** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Mistral** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Groq** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Perplexity** | No (text fallback) | Yes (SSE) | Yes | API Key |
| **Cohere** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Together AI** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **AI21 Labs** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Cerebras** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **SambaNova** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Fireworks AI** | Yes (OpenAI-compat) | Yes (SSE) | Yes | API Key |
| **Custom** | No (text fallback) | Yes (SSE) | Varies | API Key |

### AI Tool Support by Protocol

The remote-facing tools route through the `StorageProvider` trait, so they behave identically across the 7 transport protocols and 25+ native provider integrations. Local-only tools (file management, search, archives, clipboard, shell) act on the local filesystem.

**Remote tools (protocol-routed)**

| Danger | Tools |
|--------|-------|
| Read-only | `remote_list`, `remote_read`, `remote_info`, `remote_search`, `remote_head`, `remote_tail`, `remote_tree`, `remote_storage_quota`, `remote_hashsum`, `remote_sync_doctor`, `remote_reconcile` |
| Medium | `remote_upload`, `upload_files`, `upload_many`, `remote_download`, `download_files`, `remote_mkdir`, `remote_rename`, `remote_edit`, `remote_touch`, `remote_speed`, `transfer`, `transfer_tree`, `cross_profile_transfer` |
| High | `remote_delete`, `remote_delete_many`, `remote_dedupe`, `remote_cleanup`, `server_exec` |

See the full tool breakdown in [AeroAgent Tool Categories](#aeroagent-tool-categories-50-agent-tools-35-exposed-over-mcp) below.

### AI Features

| Feature | Status | Notes |
|---------|--------|-------|
| Native function calling | Done (v1.6.0) | OpenAI, Anthropic, Gemini; text fallback for Ollama |
| Streaming responses | Done (v1.6.0) | Incremental rendering via Tauri events |
| Chat history | Done (v1.6.0) | 50 conversations, 200 msgs each, persisted to disk |
| Cost tracking | Done (v1.6.0) | Per-message token count + cost estimate |
| Context awareness | Done (v1.7.0) | Provider, server host/port/user, path, selected files |
| Protocol expertise | Done (v1.7.0) | System prompt with all 14 provider configs, ports, auth |
| Styled tool display | Done (v1.7.0) | Inline chips with wrench icon replace raw TOOL/ARGS |
| Auto-routing | Done (v1.4.0) | Task-type detection routes to optimal model |
| Rate limiting | Done (v1.4.0) | 20 RPM per provider, frontend token bucket |
| Speech input | Done (v1.4.0) | Web Speech API |
| Multi-step autonomous tools | Done (v1.9.0) | Agent chains multiple tool calls without repeated prompts |
| Ollama auto-detection | Done (v1.9.0) | Discovers local Ollama instances and available models |
| Conversation export | Done (v1.9.0) | Export as Markdown or JSON |
| Monaco bidirectional sync | Done (v1.9.0) | Live two-way sync between editor and AI agent |
| Terminal command execution | Done (v1.9.0) | AI executes terminal commands with user approval |
| Streaming markdown renderer | Done (v2.0.0) | Incremental rendering with finalized segments (React.memo) |
| Code block actions | Done (v2.0.0) | Copy/Apply/Diff/Run buttons on AI code blocks |
| Thought visualization | Done (v2.0.0) | ThinkingBlock for Anthropic/OpenAI/Gemini reasoning |
| Prompt templates | Done (v2.0.0) | 15 built-in templates via `/` prefix, vault-persisted custom |
| Multi-file diff | Done (v2.0.0) | PR-style diff with per-file checkboxes |
| Cost budget tracking | Done (v2.0.0) | Per-provider monthly limits, vault-persisted |
| Chat search | Done (v2.0.0) | Ctrl+F with role filter and keyboard navigation |
| Anthropic prompt caching | Done (v2.0.0) | cache_control ephemeral, 90% read discount |
| OpenAI structured outputs | Done (v2.0.0) | strict: true for OpenAI/xAI/OpenRouter |
| Ollama model templates | Done (v2.0.0) | 8 family profiles with tailored prompts |
| Ollama pull from UI | Done (v2.0.0) | NDJSON streaming progress in AI Settings |
| Gemini code execution | Done (v2.0.0) | executableCode/codeExecutionResult parsing |
| Kimi web search | Done (v2.0.1) | `$web_search` builtin_function tool injection |
| Kimi context caching | Done (v2.0.1) | `/v1/caching` endpoint, cache_id passthrough |
| Kimi file analysis | Done (v2.0.1) | `/v1/files` upload, fileid:// references |
| Qwen thinking mode | Done (v2.0.1) | `enable_thinking` + `thinking_budget` parameters |
| Qwen web search | Done (v2.0.1) | `enable_search` + `search_options.search_strategy` |
| DeepSeek thinking mode | Done (v2.0.1) | `reasoning_content` streaming (shared with o3) |
| DeepSeek FIM completion | Done (v2.0.1) | `/beta/completions` with prompt + suffix |
| DeepSeek prefix completion | Done (v2.0.1) | `prefix: true` on last assistant message |

---

## AeroAgent Pro (v2.0.0)

### AI Provider Feature Matrix

| Feature | OpenAI | Anthropic | Google Gemini | xAI | OpenRouter | Ollama | Kimi | Qwen | DeepSeek | Custom |
|---------|--------|-----------|---------------|-----|------------|--------|------|------|----------|--------|
| Native Tool Calling | Yes | Yes | Yes | Yes | Yes | Text format | Yes | Yes | Yes | Yes |
| Streaming | SSE | SSE | SSE | SSE | SSE | NDJSON | SSE | SSE | SSE | SSE |
| Vision/Multimodal | Yes | Yes | Yes | Yes | Via model | Yes | Yes | Yes | N/A | Via model |
| Structured Outputs | **strict: true** | N/A | N/A | **strict: true** | **strict: true** | N/A | N/A | N/A | N/A | N/A |
| Prompt Caching | N/A | **Ephemeral** | **cachedContent** | N/A | N/A | N/A | **Context cache** | N/A | N/A | N/A |
| Extended Thinking | o3/o3-mini | Claude 3.5+ | Gemini 2.0+ | N/A | Via model | deepseek-r1 | N/A | **enable_thinking** | **reasoning_content** | N/A |
| Web Search | N/A | N/A | N/A | N/A | N/A | N/A | **$web_search** | **enable_search** | N/A | N/A |
| Code Execution | N/A | N/A | **Python sandbox** | N/A | N/A | N/A | N/A | N/A | N/A | N/A |
| FIM Completion | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | **beta/completions** | N/A |
| File Analysis | N/A | N/A | N/A | N/A | N/A | N/A | **v1/files** | N/A | N/A | N/A |
| Prefix Completion | N/A | N/A | N/A | N/A | N/A | N/A | N/A | N/A | **prefix: true** | N/A |
| Model Pull from UI | N/A | N/A | N/A | N/A | N/A | **Yes** | N/A | N/A | N/A | N/A |
| GPU Monitoring | N/A | N/A | N/A | N/A | N/A | **Yes** | N/A | N/A | N/A | N/A |
| Model Family Templates | N/A | N/A | N/A | N/A | N/A | **8 families** | N/A | N/A | N/A | N/A |

### AeroAgent Tool Categories (50+ agent tools, 35+ exposed over MCP)

| Category | Tools | Danger |
|----------|-------|--------|
| Remote operations (21) | `remote_list`, `remote_read`, `remote_info`, `remote_search`, `remote_head`, `remote_tail`, `remote_tree`, `remote_storage_quota`, `remote_hashsum`, `remote_upload`, `upload_files`, `upload_many`, `remote_download`, `download_files`, `remote_mkdir`, `remote_rename`, `remote_edit`, `remote_touch`, `remote_delete`, `remote_delete_many`, `list_servers` | read-only / medium / high |
| Local file operations (21) | `local_list`, `local_read`, `local_write`, `local_mkdir`, `local_delete`, `local_rename`, `local_search`, `local_edit`, `local_move_files`, `local_batch_rename`, `local_copy_files`, `local_trash`, `local_file_info`, `local_disk_usage`, `local_find_duplicates`, `local_diff`, `local_stat_batch`, `local_tree`, `local_grep`, `local_head`, `local_tail` | read-only / medium / high |
| Cross-profile transfer (4) | `transfer`, `transfer_tree`, `cross_profile_transfer`, `generate_transfer_plan` | safe / medium |
| Sync & verification (7) | `sync_doctor`, `reconcile`, `dedupe`, `cleanup`, `sync_control`, `sync_preview`, `speed` | read-only / medium / high |
| Archives (2) | `archive_compress`, `archive_decompress` | medium |
| System & shell (4) | `shell_execute`, `clipboard_read`, `clipboard_write`, `hash_file` | safe / high |
| Knowledge & memory (3) | `rag_index`, `rag_search`, `agent_memory_write` | read-only / medium |
| App & server control (6) | `app_info`, `set_theme`, `vault_peek`, `preview_edit`, `agent_connect`, `server_exec` | safe / read-only / high |

> Many remote tools ship under both an `aeroftp_*` (MCP) and a `remote_*` (GUI / cross-profile) alias for the same capability, so the per-category rows above count every alias name and tally higher than the distinct-tool figure. 35+ canonical capabilities are exposed over the MCP server.

---

## Credential Storage Architecture (v1.9.0)

### Unified Keystore

Since v1.9.0, **all sensitive data** is stored in the Universal Vault (`vault.db`). The previous layered approach (OS Keyring primary, vault fallback) has been replaced by a single encrypted backend.

| Data Type | Storage | Notes |
|-----------|---------|-------|
| **Server passwords** | vault.db (AES-256-GCM) | Per-entry encryption with random nonce |
| **Server profiles** | vault.db (AES-256-GCM) | Host, port, username, protocol config (v1.9.0) |
| **OAuth tokens** | vault.db (AES-256-GCM) | Access + refresh tokens for all OAuth providers |
| **AI API keys** | vault.db (AES-256-GCM) | All AI provider keys |
| **AI settings** | vault.db (AES-256-GCM) | Model selection, provider config (v1.9.0) |
| **App config** | vault.db (AES-256-GCM) | Sensitive application settings (v1.9.0) |
| **MEGA credentials** | secrecy crate (zero-on-drop) | In-memory only during session |

### Keystore Backup/Restore (v1.9.0)

| Parameter | Value |
|-----------|-------|
| Format | `.aeroftp-keystore` binary |
| KDF | Argon2id (64 MB, t=3, p=4) |
| Encryption | AES-256-GCM |
| Integrity | HMAC-SHA256 |
| Merge modes | Skip existing, Overwrite all |

### Key Derivation (Vault)

| Parameter | Value |
|-----------|-------|
| Algorithm | HKDF-SHA256 (RFC 5869) |
| Input | 512-bit CSPRNG passphrase |
| Output | 256-bit vault key |
| Per-entry | AES-256-GCM with random 12-byte nonce |

---

## Release History

| Version | Feature | Status |
|---------|---------|--------|
| v1.2.8 | Properties Dialog, Compress/Archive, Checksum, Overwrite, Drag & Drop | Done |
| v1.3.0 | SFTP Integration, 7z Archives, Analytics | Done |
| v1.3.1 | Multi-format TAR, Keyboard Shortcuts, Context Submenus | Done |
| v1.3.2 | Secure Credential Storage, Argon2id Vault, Permission Hardening | Done |
| v1.3.3 | OS Keyring Fix (Linux), Migration Removal, Session Tabs Fix | Done |
| v1.3.4 | SFTP Host Key Verification, Ephemeral OAuth Port, FTP Warning | Done |
| v1.4.0 | Cross-provider search/quota/versions/thumbnails/permissions/locking, S3 multipart, FTP resume + MLSD, dep upgrades | Done |
| v1.4.1 | AI API keys → OS Keyring, ZIP/7z password dialog, ErrorBoundary, hook extractions, dead code cleanup | Done |
| v1.5.0 | 4 new providers (Box, pCloud, Azure, Filen), FTP TLS default, S3/WebDAV stable badges | Done |
| v1.5.1 | WebDAV directory fix, provider keep-alive, drag-to-reorder tabs/servers, 4 new presets (30 total), provider logos | Done |
| v1.5.2 | Multi-protocol sync, codebase audit, credential fix, SEC-001/SEC-004 fixes | Done |
| v1.5.3 | Sync index cache, storage quota display, OAuth session switching fix, FTP retry with backoff | Done |
| v1.5.4 | In-app auto-updater, download progress, AppImage auto-install, terminal empty-start | Done |
| v1.6.0 | AeroAgent Pro: native function calling (SEC-002), streaming, provider-agnostic tools, chat history, cost tracking, context awareness, 122 i18n keys | Done |
| v1.7.0 | Encryption Block: AeroVault v1, archive browser + selective extraction, Cryptomator format 8, CompressDialog, AeroFile mode, preview panel, Type column, 7z password fix | Done |
| v1.8.0 | Smart Sync (3 intelligent modes), Batch Rename (4 modes), Inline Rename, **AeroVault v2** (AES-256-GCM-SIV + AES-KW + AES-SIV + Argon2id 128MiB + HMAC-SHA512 + ChaCha20 cascade) | Done |
| v1.8.6 | Universal Credential Vault (AES-256-GCM + Argon2id + HKDF-SHA256), smart folder transfers, folder conflict resolution | Done |
| v1.8.7 | File clipboard (cut/copy/paste), cross-panel paste, Ubuntu/macOS audits | Done |
| v1.8.8 | Vision/multimodal AI, auto panel refresh, XSS hardening, security audit | Done |
| v1.9.0 | **Unified Keystore** (localStorage to vault migration), **keystore backup/restore** (.aeroftp-keystore), **migration wizard**, AI multi-step tools, Ollama auto-detection, conversation export, Monaco bidirectional sync, terminal command execution | Done |
| v2.0.0 | **AeroAgent Pro** - Provider Intelligence (7 provider profiles, model registry, parameter presets), Advanced Tool Execution (DAG pipeline, diff preview, intelligent retry, tool validation, composite macros, progress indicators), Context Intelligence (project detection, file dependency graph, persistent agent memory, conversation branching, smart context injection, token budget optimizer), Professional UX (streaming markdown, code block actions, thought visualization, prompt templates, multi-file diff, cost budget, chat search), Provider Features (Anthropic caching/thinking, OpenAI strict outputs, Ollama templates/pull/GPU, Gemini code execution) | Done |
| v2.0.1 | **3 Asian AI Providers** (Kimi, Qwen, DeepSeek) with thinking modes, web search, FIM, context caching, file analysis. **AeroFile Pro** Places Sidebar, BreadcrumbBar, Large Icons, drive detection, custom sidebar locations. Official SVG provider logos for all 10 providers | Done |
| v2.0.5 | **4shared native REST API** (OAuth 1.0, 14th protocol), CloudMe WebDAV preset, Places Sidebar GVFS/unmounted partitions, Autostart, Windows Explorer badges, OwnCloud removal | Done |
| v2.5.0 | **AeroFile Pro** - LocalFilePanel extraction, local path tabs (12 max, drag-to-reorder), file tags SQLite (7 Finder-style labels), FileTagBadge, tags context menu, sidebar filter, macOS FinderSync, event-driven volume detection, keyboard navigation, ARIA accessibility | Done |
| v2.5.2 | **AeroImage** - Built-in image editor (crop, resize, rotate, flip, color adjustments, effects, 6 output formats) | Done |
| v2.6.0 | **AeroAgent Ecosystem** - 4 new AI providers (AI21, Cerebras, SambaNova, Fireworks), Command Palette, Plugin Registry with GitHub-based browser, plugin hooks, context menu AI, AI status widget, drag & drop to agent | Done |
| v2.7.0 | **FileLu native REST API** - 19th protocol, file/folder passwords, privacy toggle, server-side clone, trash manager, remote URL upload | Done |
| v2.7.4 | **Complete Provider Integration** - Box (trash, move, comments, collaborations, watermark, folder locks, tags), Google Drive (starring, comments, properties), Dropbox (tags, trash UI), OneDrive (trash lifecycle). 33 new commands, PRO badge system | Done |
| v2.8.0 | **Koofr Native API** (20th protocol), **Production CLI** (13 commands, 20 protocols), **AeroAgent Server Exec** (2 new tools, vault-secured), Ed25519 license foundation | Done |
| v2.9.2 | **CLI Expansion** - Batch scripting engine (17 commands), glob transfers, tree command, exit codes, path traversal protection, BFS caps, NO_COLOR, SIGPIPE | Done |
| v2.9.4 | **Server Health Check** - DNS/TCP/TLS/HTTP probes, latency gauge, batch diagnostics. Download mtime preservation. AeroVault Pro modular refactor, recent vaults, folder encryption | Done |
| v2.9.5 | **Dual-Engine Security Audit** - Claude Opus 4.6 + GPT-5.4 (117 findings), DOMPurify, vault write safety, shell denylist expansion, TOTP-before-cache. Yandex Object Storage S3 preset | Done |
| v2.9.6 | **Remote Timestamp Timezone Fix** - MLSD/SFTP/cloud UTC→local conversion, sync comparison fix, 11 provider backends updated | Done |
| v2.9.7 | **Share Links for FTP/SFTP/WebDAV** - Per-server Public URL Base mapping, folder scan progress toast, update install overlay, security audit remediation | Done |
| v2.9.8 | **OpenDrive Native API** (22nd protocol), Settings OAuth for Yandex/Zoho | Done |
| v3.0.5 | **SFTP upload 0-byte fix** (russh→ssh2/SCP backend on embedded SFTP servers), atomic downloads (`.aerotmp` rename) on all 22 providers, **GitHub PEM vault** (AES-256-GCM encrypted in vault on import), GitHub token expiry badges, 0 B file alert badge | Done |
| v3.1.x | Filen Encrypted Notes (Beta), CSP Phase 2 prep, biometric unlock investigation | Done |
| v3.3.4 | **`serve http`** + **`serve webdav`** local servers (axum-based, Range requests, PROPFIND, atomic PUT) | Done |
| v3.5.2 | **CLI determinism**: 12-code exit map covering all `ProviderError` variants, `mkdir --parents`, `rm --force`, `put --no-clobber`, `--chunk-size`/`--buffer-size` overrides, Azure server-side copy, parallel S3 multipart, threat model + LLM Integration Guide | Done |
| v3.5.3 | **CLI `sync --watch`** continuous bidirectional sync (filesystem watcher native, anti-loop cooldown, NDJSON output), **MCP Server dialog** + VS Code extension `axpdev-lab.aeroftp-mcp`, `--files-from`/`--immutable`/`--no-check-dest`/`--max-depth`/`--inplace`/`--fast-list`, `cleanup` orphan `.aerotmp` scanner, dedupe 7 modes, bisync `--conflict-mode rename`, FTP/WebDAV listing path fixes | Done |
| v3.5.4 | **MCP hardening**: top-level `aeroftp mcp` subcommand, shared `profile_loader` for CLI+MCP, S3 bucket fix from vault, vault auto-init in MCP, per-profile serialization mutex, shutdown drain, schema validation, FTP/SFTP/WebDAV/Filen/FileLu/Drime/Immich error message + retry hardening, CLI `hashsum --algorithm`, `speed --size` aliases | Done |
| v3.5.8 / v3.5.9 | MCP pool auto-reset, `delete_many`, `list_servers` filter, `read_file preview_kb`, `upload_file create_parents`, `sync_tree plan[]`, two-sided checksum, CLI parent explainer, scan reconnect, ssh2 OpenSSL Windows CI quirks | Done |
| v3.6.0 | MCP `read_file` soft-truncate, `check_tree compare_method`, cluster doc reorg | Done |
| v3.6.1 | **Windows first-class delta sync** - native rsync protocol 31 in pure Rust, no rsync.exe bundle, no WSL requirement (`aerorsync` cross-OS, `#![cfg(unix)]` removed surgically) | Done |
| v3.6.6 | Community wishlist quick wins, Server Speed Test dialog (`speed-compare`), `aeroftp_agent_connect`, AeroRsync wired into Cross-Profile Transfer + AeroTools Code Editor save | Done |
| v3.6.7 / v3.6.8 / v3.6.10 | Share-link reliability + `link --verify`, suppaftp 8.0.3 with 14 upstream FTP fixes, MEGA Native canonical key layout interop fix, MEGA/Filen 2FA modal hookup, TOTP QR | Done |
| v3.7.0 | **AeroRsync session-cached batch transport** (`AerorsyncBatch` trait, `delta_files[]`, `bytes_on_wire`), **AeroVault overlay session model** (transparent encrypted browse), **rclone crypt full read/write** (provider session + filename obfuscation), **Server Health Check** engine (DNS/TCP/TLS/HTTP probes, IntroHub Pro), **MCP wave-5 cross-profile transfer** + **wave-6 ops tools** (`touch` / `cleanup` / `speed` / `sync_doctor` / `dedupe` / `reconcile`), MCP tool count 27 to 39, `aerovault` crate 0.3.4 with `rename_entry` / `move_entry` / `copy_entry` | Done |
| v3.7.1 | **Persistent Mount Manager** (FUSE / WebDAV) with cross-platform autostart (systemd-user / Task Scheduler ONLOGON), sidecar JSON or vault-backed storage, "Open mount in file manager" auto-creates default mount, Windows free-drive-letter helper. **Filen Desktop local bridges** (WebDAV port 1900 + S3 port 1700) on top of layered WebDAV scheme detection (explicit / `tls_mode` / auto for localhost / RFC 1918 / `*.local`). **AeroFile community polish**: aggregate multi-file Properties dialog, recursive `*` flatten search, smart Open with default app, PathBar empty-area edit + trailing chevron, configurable provider-icon size, drag-reorder custom icons, Server Health overlay dot on Discover. **AeroSync wrapper script export** round-trips `.sh` / `.ps1` with embedded `# AEROFTP-META`. **My Servers unified table** (Used / Total / %, semantic table with sticky thead/tfoot, click-to-sort, dedup-aware footer + per-protocol breakdown, drag-to-reorder + resize on three surfaces). **Filen v3 Argon2id auth**, MEGAcmd anonymous WebDAV, Backblaze B2 native Quick Connect form, S3Drive icon + setup-with-rclone banner, Filen Desktop S3 / WebDAV logos. CLI `profiles -i` interactive prompt loop. | Done |
| v3.7.2 | **AeroCrypt overlay first-class** (rclone-crypt promoted next to AeroVault, folder transfers traverse encrypted directory trees end to end, BFS depth 64, filename obfuscation, AEROCRYPT path-bar badge), **ImageKit (23rd protocol)**, **Uploadcare (24th protocol)**, **Codex CLI security audit** (17 paired fixes CLI-AUDIT-01..17 across GUI tool approval, MCP path validation, server_exec read-only contract, atomic temp-file safety, SFTP packet bounds, daemon token mode 0600, sync direction validation, exit-code 130 cancellation), **T-TOPBAR-3-CLUSTER** titlebar restructure (page-nav / utility / window clusters, fixes #129 click-shift drift), **T-EDITOR-DRAG-RUN** AeroFile to Editor to Terminal flow (.ps1 / .sh / .py with shell quoting and no auto-Enter), **T-AUTO-RECONNECT-IDLE** SFTP silent reconnect (#161, ConnectionLost classification, cwd restore, toast lifecycle), Ctrl+T theme cycle and Ctrl+S editor save bindings (#171), Ehud table polish (per-column L/C/R alignment, sticky header, sentence-case, redundant Detailed cards toggle removed), CLI profiles dynamic terminal-width-aware layout + unified --breakdown + --hide=fav alias surface, Esc closes Quick Connect, Delete confirmation grammatical messaging across 47 languages, draggable modal X-click first time fix. Direct `rsa = "0.9"` dropped, `jsonwebtoken` on `aws-lc-rs`, `audit.toml` documents transitive RUSTSEC ignores with written threat-model justifications. | Done |
| v3.7.4 | **Filen v3 chunked AES-GCM upload** (per-chunk retry on egest decode failures) + dedicated TOTP two-factor prompt, **Activity Log** reachable from the View menu, **Cloudinary** provider + **TAB.DIGITAL** preset, B2 quota/share/recoverability UX, WebDAV URL-bar drill-down, OneDrive mkdir, Drime listing polish, Windows MSI/NSIS silent update | Done |
| v3.7.9 | **AeroFile Dual Panel Slice A** (two local panels side by side, full keyboard parity on the second pane, Total-Commander F5/F6/F7 + drag-and-drop), **AeroVault v3 (Experimental)** (gear-CDC chunking, per-chunk zstd, AES-256-GCM-SIV, BLAKE3-128 chunk ids, reserved v4 Error Correction region), **TOTP secret passthrough** for Filen and MEGA | Done |
| v3.8.0 | **AeroVault wrapper-stack hardening + telemetry + full CLI vault parity** (`aeroftp-cli vault` for v1/v2/v3), **storage quota override + recursive used-storage scan + compression columns**, **AeroRsync native streaming default ON + local-to-local** (256 MiB cap removed, batch SSH session reuse), **AeroFile Dual Panel Slice B + Slice C** (any-endpoint panes, FreeFileSync-style compare/mirror/backup/bisync) | Done |
| v4.0.0 | **Shaped Graph Transfer (DAG) core and selected production runners** (single-file multipart is the complete path; batch/sync wrappers remain conservative, copy uses the direct fallback helper, and the DAG range path is opt-in; rollout flags and the old batch orchestrator removed), **Multi-User Account Partition** (per-user encrypted vault partitions, boot-time Account Lock Screen, opt-in admin role, CLI `--user`) | Done |
| v4.0.1 | **S3 native AssumeRole** ([#301](https://github.com/axpdev-lab/aeroftp/issues/301), hand-rolled STS client, MFA, auto-refresh before expiry), **AeroVault dual-audit remediation** (crate hardened to v3), **unified profile bridge** (rclone/WinSCP/FileZilla through one dispatcher), **Servers tab folded into the Backup interoperability table** ([#270](https://github.com/axpdev-lab/aeroftp/issues/270)), community wishlist batch ([#300](https://github.com/axpdev-lab/aeroftp/issues/300)), embedded-rsync (WD MyCloud) download integrity fix | Done |
| v4.0.2 | **Cross-Machine Keystore Portability** and agent-facing polish | Done |
| v4.0.3 | Restricted-filename clear-rejection groundwork, CLI polish, stability fixes | Done |
| v4.0.4 | **Reversible restricted-filename encoding** (Box, Dropbox, Jottacloud, OpenDrive; rclone-compatible scheme), CLI `profiles -i` polish, lightweight CI checks job, tray suspend/resume + macOS Tahoe ([#290](https://github.com/axpdev-lab/aeroftp/issues/290)) + OAuth reconnect fixes | Done |
| v4.0.5 | **AeroCrypt native encrypted overlay (format AECR)**, **AeroVault v4 (v3 + Reed-Solomon error correction)** with `vault create --error-correction`/`scrub`/`repair`, **interactive CLI TUI** (`aeroftp tui`, alpha) and `profiles -i` shell, **Server Groups** ([#320](https://github.com/axpdev-lab/aeroftp/issues/320)), **Filen/MEGAcmd file versioning** + CLI `versions`, CLI `compress`/`extract`, russh 0.61.2 SSH advisory clearance | Done |
| v4.0.6 | **AeroVault crate convergence** (AEROVAULT3 engine + revision 4 Reed-Solomon moved into the published `aerovault` crate 0.6.0; app keeps thin wrappers), folder-upload skip/overwrite policy fix | Done |
| v4.0.7 | **AeroVault dual blind security audit (grade A)** + full remediation, error correction converged onto `aerovault` crate 0.6.2, interrupted-seal temp/lock cleanup, reparse-point extract hardening, IntroHub My Servers layout fixes | Done |
| v4.0.8 | **AeroCrypt promoted to a first-class `Crypt` profile type** (native AeroCrypt + rclone-crypt interop shown as one `Crypt` class in My Servers and `profiles`, navigate-out encrypted scope, [#272](https://github.com/axpdev-lab/aeroftp/issues/272)), **AeroProgress** transfer card, **Change Mode** (v2 <-> v3 repack), **Cryptomator byte-for-byte interop** ([#322](https://github.com/axpdev-lab/aeroftp/issues/322)), Quick Connect credential isolation ([#215](https://github.com/axpdev-lab/aeroftp/issues/215)), `aerovault` crate 0.6.3 | Done |
| v4.0.9 | **AeroVault create redesign** (Compressor-style, named vaults, tabbed Home/Recent/Files) + **AeroVault Zip plaintext lane** (`.aerozip`, `AEROVAULT3` with cipher off, opt-out recovery), **AeroMount read-only vault mount + Save-All** ([#322](https://github.com/axpdev-lab/aeroftp/issues/322)), **universal rclone export** (Filen + all OAuth providers, [#128](https://github.com/axpdev-lab/aeroftp/issues/128)), **Crypt overlay padlock badge** ([#272](https://github.com/axpdev-lab/aeroftp/issues/272)), interactive `groups`/`users` CLI ([#311](https://github.com/axpdev-lab/aeroftp/discussions/311)), rclone export CR/LF hardening + RUSTSEC-2026-0185 patch | Done |

### Planned

| Version | Feature |
|---------|---------|
| v3.7.x | P3-T01 streaming multi-file + session reuse (rimuove cap 256 MiB native rsync, batch SSH session per N file). 4 onde, ~24 gg netti |
| v3.x | Advanced Share Links (expiration, password, permissions), Full Activity Logging, AeroVault biometric unlock |

---

*This document is maintained as part of AeroFTP protocol documentation.*
