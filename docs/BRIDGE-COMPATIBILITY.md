# Profile bridge compatibility matrix

AeroFTP bridges its saved server profiles with **15 external tools**, importing
their connections into the encrypted vault and exporting AeroFTP profiles back
into each tool's native config format. This page is the full per-protocol and
per-provider breakdown of what each bridge can carry. For the high-level tool
list (config file, credential recovery level, dedicated bridge pages) see the
[Profile Bridge section in the README](../README.md#profile-bridge).

**Legend:** `IE` = import and export, `I.` = import only, `.E` = export only,
`--` = no support. A protocol or provider is "supported" by a tool only when
that tool's own config format can express it.

## At a glance

- The five broadly shared bridge protocols are **FTP, FTPS, SFTP, WebDAV and S3**.
  AeroFTP itself has eight transport protocols and 26 native provider integrations;
  Azure Blob and OpenStack Swift have rclone bridges in the table below. See the
  generated [provider inventory](PROVIDER-INVENTORY.json) for product counts.
- **15 external tools** have an import/export bridge.
- The transfer protocols interoperate widely across tools. The native cloud
  providers interoperate **only through rclone**: no other tool's config format
  can express them.
- **Some AeroFTP-native providers have no external bridge at all** and
  round-trip only through AeroFTP's own `.aeroftp` export: aerocloud, 4shared,
  kDrive, GitHub, GitLab, Immich, Twake and Uploadcare, which no other tool
  speaks, and FileLu, whose rclone backend needs a different key (see the asymmetries below). Proton Drive can bridge a recoverable native session.

## Transfer protocols (all 15 tools)

| Protocol | rclone | WinSCP | FileZilla | Cyberduck | MobaXterm | Dreamweaver | lftp | OpenSSH | PuTTY | AWS CLI | mc | s3cmd | Kopia | Duplicacy | restic |
|----------|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| FTP      | IE | IE | IE | IE | IE | IE | IE | -- | -- | -- | -- | -- | -- | -- | -- |
| FTPS     | IE | IE | IE | IE | IE | IE | IE | -- | -- | -- | -- | -- | -- | -- | -- |
| SFTP     | IE | IE | IE | IE | IE | IE | IE | IE | IE | -- | -- | -- | IE | IE | IE |
| WebDAV   | IE | IE | -- | IE | IE | IE | IE | -- | -- | -- | -- | -- | IE | IE | IE |
| S3       | IE | IE | IE | IE | -- | -- | -- | -- | -- | IE | IE | IE | IE | IE | IE |

S3 sub-providers ride the single S3 protocol, so every S3-capable tool above
covers them: Amazon S3, Cloudflare R2, DigitalOcean Spaces, Wasabi, Backblaze B2
(via its S3 gateway), Linode Object Storage, Scaleway, Storj, IDrive e2, MinIO,
IONOS S3, Alibaba OSS, Tencent COS, Qiniu Kodo, Google Cloud Storage, and generic
custom S3. Fastly Object Storage exports to rclone as `provider = Fastly`,
which needs rclone 1.74.0 or newer (older versions reject the name), and is
recognized on import from that token or from a `.object.fastlystorage.app`
endpoint. Coritan Object Storage has no rclone token: it exports as
`provider = Other` with its endpoint (port 7337 included) and its stored
region, on rclone's path-style default, and is recognized on import from a
`coritan.com` endpoint, whose region label fills a missing region. WebDAV vendors (Nextcloud, ownCloud, and custom WebDAV for
SharePoint / Fastmail / Koofr-dav) likewise ride WebDAV.

Use **rclone 1.76.0 or newer** with exported configurations. Before that
release, an exported OpenStack Swift remote could disclose credentials on a
redirect ([GHSA-m97j-7x65-wh93](https://github.com/rclone/rclone/security/advisories/GHSA-m97j-7x65-wh93)).
The same release fixes authentication bypass and missing revocation when an
exported remote is exposed through `rclone serve ... --auth-proxy`
([GHSA-66qv-66mg-9h74](https://github.com/rclone/rclone/security/advisories/GHSA-66qv-66mg-9h74),
[GHSA-vmgx-g9c5-669p](https://github.com/rclone/rclone/security/advisories/GHSA-vmgx-g9c5-669p)).
The 1.76.0 release also fixes the RC request-memory, htpasswd timing, Ulozto
redirect and local metadata symlink advisories; those require enabling the
corresponding rclone service, backend or local mode and are not fields written
by this bridge.

## Native cloud providers (rclone is the only bridge)

These have a dedicated AeroFTP provider and a matching rclone backend. No other
tool's format can carry them, so every non-rclone column would be `--`.

Every provider in this table now both **imports and exports**. Recoverable-secret
backends (user + password, account + key) export their secret directly. The OAuth
backends (Google Drive, Dropbox, OneDrive, Box, pCloud, Yandex, Zoho WorkDrive) export the OAuth
token AeroFTP persists for the profile together with the `client_id` and
`client_secret` configured in AeroFTP for that provider. rclone can refresh the
token only with the OAuth app that issued it, so this works while that app is
still the one configured (after changing it, authorize the profile again before
exporting); when the vault lacks any of the three, the remote is still written,
with a comment asking for `rclone config reconnect "<remote>:"`.
Jottacloud follows the same token model
via its persisted OIDC refresh token, and Filen exports its email, password and
Filen CLI api key. (Token and credential export across these providers landed for
v4.0.9.) Internxt exports its email and password; rclone then needs one
`rclone config reconnect "<remote>:"` to sign in and store the encryption
mnemonic, which AeroFTP does not keep on disk. Exported without its password
(credentials left out of the export), the remote needs
`rclone config update "<remote>" --all` instead, which asks for the password
and then signs in. Internxt lets rclone sign in only on plans that include
rclone access (the free plan answers 402).

| Provider | rclone | rclone backend | Credentials carried |
|----------|:--:|----|----|
| MEGA            | IE | mega       | user + password |
| Azure Blob      | IE | azureblob  | account + key |
| OpenStack Swift | IE | swift      | user + key |
| Koofr           | IE | koofr      | user + password |
| Jottacloud      | IE | jottacloud | OIDC refresh token (AeroFTP-persisted); the exported remote refreshes on first use (v4.0.9) |
| Filen           | IE | filen      | email + password + Filen CLI api key |
| Internxt        | IE | internxt   | email + password; one `rclone config reconnect` before use (`rclone config update --all` when exported without the password), on plans with rclone access |
| OpenDrive       | IE | opendrive  | username + password |
| Backblaze B2    | IE | b2         | account (key ID) + application key (native B2, not S3) |
| Google Drive    | IE | drive      | OAuth token (AeroFTP-persisted); refreshes on rclone's first use |
| Dropbox         | IE | dropbox    | OAuth token (AeroFTP-persisted); refreshes on rclone's first use |
| OneDrive        | IE | onedrive   | OAuth token + drive_id/drive_type captured at connect |
| Box             | IE | box        | OAuth token (AeroFTP-persisted); refreshes on rclone's first use |
| pCloud          | IE | pcloud     | OAuth token (AeroFTP-persisted) + hostname for the EU region |
| Yandex Disk     | IE | yandex     | OAuth token (AeroFTP-persisted); refreshes on rclone's first use |
| Zoho WorkDrive  | IE | zoho       | OAuth token + region + root folder id (a profile without the root folder id is skipped) |
| Drime           | IE | drime      | API token (default workspace; a remote pinned to a workspace or root folder id is not imported) |
| Cloudinary      | IE | cloudinary | cloud name + API key + API secret (api.cloudinary.com; a remote on a regional `upload_prefix` is not imported) |
| Proton Drive    | IE | protondrive | Native mode: cached UID, access/refresh tokens and key password; unsigned native profiles and official CLI profiles are skipped on export |
| ImageKit        | IE | imagekit   | URL endpoint + public key + private key (a profile created in the app has no public key and is skipped on export) |

## Per-tool exportable protocols

Which saved profiles each tool will accept on export. (Import is not filtered this
way: an importer reads whatever connection types it recognizes in the file.)

| Tool | Exportable protocols |
|-----|----------------------|
| rclone | FTP, FTPS, SFTP, WebDAV, S3, MEGA, Internxt, Azure, Swift, Koofr, Jottacloud, Filen, Google Drive, Dropbox, OneDrive, Box, pCloud, Yandex, Zoho WorkDrive, OpenDrive, Backblaze B2, Drime, Cloudinary, ImageKit, Proton Drive |
| WinSCP | FTP, FTPS, SFTP, WebDAV, S3 |
| FileZilla | FTP, FTPS, SFTP, S3 |
| Cyberduck | FTP, FTPS, SFTP, WebDAV, S3 |
| MobaXterm | FTP, FTPS, SFTP, WebDAV |
| Dreamweaver | FTP, FTPS, SFTP, WebDAV |
| lftp | FTP, FTPS, SFTP, WebDAV |
| OpenSSH / PuTTY | SFTP |
| AWS CLI / mc / s3cmd | S3 |
| Kopia / Duplicacy / restic | S3, SFTP, WebDAV |

## Asymmetries worth knowing

- **rclone OAuth providers** (Google Drive, Dropbox, OneDrive, Box, pCloud,
  Yandex, Zoho WorkDrive) now export too: AeroFTP emits the OAuth token it persists for the
  profile with the `client_id` and `client_secret` configured in AeroFTP for
  that provider, which let rclone refresh the token while they belong to the
  app that issued it; without all three in the vault the remote asks for one
  `rclone config reconnect`. OneDrive also carries the captured
  drive_id/drive_type, Zoho WorkDrive its region and root folder id. The
  recoverable-secret providers (MEGA, Azure, Swift, Koofr, OpenDrive, Backblaze
  B2), Jottacloud, Filen (email + password + api key) and Internxt (email +
  password, then one `rclone config reconnect`, or `rclone config update --all`
  when exported without the password) export their secrets directly.
- **API-key providers** (Drime, Cloudinary, ImageKit) export their keys as they
  are: rclone does not obscure these fields. ImageKit also needs the account
  public key, which AeroFTP never uses: only a profile imported from rclone
  holds it, and one created in the app is skipped on export with that reason.
- **FileLu** has a rclone backend that needs the FileLu Rclone key (`RC_...`), a different key from the Developer API key AeroFTP holds: export skips a FileLu profile and import skips a `filelu` remote, each with that reason.
- **Proton Drive** imports `protondrive` remotes into native mode and exports a native profile only when its session can be recovered from the unlocked vault. The bridge carries the cached UID, access token, refresh token and key password; it does not need or export the account password. A native profile without a saved session is skipped with a sign-in instruction. Profiles using the official Proton Drive CLI are skipped because that CLI owns its session. Existing AeroFTP profiles without an explicit mode retain CLI behavior. Treat an exported native session as a secret and encrypt the destination configuration.
- **FileZilla** 3.72 stores a site's active/passive override as `data_mode_override`. Default-mode sites follow global `Use Pasv mode` (default `1`): global active mode produces the same per-site warning as an explicit active override, while explicit passive remains unchanged. A recognized generic proxy (`Proxy type` 1..=4: HTTP CONNECT, SOCKS5, SOCKS4, or SSH) forces passive mode in FileZilla and suppresses the separate FTP proxy warning, so it produces one file-level generic proxy warning without a false active-mode or FTP-proxy warning. An unknown generic `Proxy type` warns as unknown without suppressing active-FTP or FTP-proxy warnings. Explicit passive sites match AeroFTP's passive-only FTP; default-mode FTP sites in FileZilla can still fall back to active mode when `Allow transfermode fallback` (default `1`) is enabled and PASV/EPSV fails, which AeroFTP does not reproduce. Active-mode sites still import with a warning that AeroFTP supports passive FTP only. Site `Protocol` follows FileZilla's `ServerProtocol` numbering: `6` is plain FTP only (imported with `tlsMode` `none`) and `7` is S3, on import and export, with S3 `<RemoteDir>` mapped into `options.bucket` and `initialPath` (preserving exact prefix separators). FileZilla exports from AeroFTP releases up to v4.2.7 that wrote S3 as `Protocol=6` under `<FileZilla3 version="3.67.1" platform="*">` without native FileZilla site fields are recovered as S3 with `RemoteDir` kept as the start path and a warning to set the bucket name before connecting (those older exports did not write `options.bucket`). A standalone, unreadable, oversized, empty or truncated `filezilla.xml` produces one conditional file-level warning for proxy state and, where default-mode FTP sites exist, the unread global transfer mode. A `sitemanager.xml` that is malformed or ends before its closing tag imports the sites read before the break, with a file-level warning that later sites were not imported. Export writes `<RemoteDir>` in FileZilla's `CServerPath::GetSafePath()` format, in the server type and segments FileZilla's own path parser would store (no empty segment for FTP/SFTP, `X:\` paths as DOS), with lengths counted the way FileZilla on the exporting OS counts them (UTF-16 units on Windows); a relative start path, a path containing `..` segments, and an S3 profile without a bucket, are exported without a `<RemoteDir>`. Import decodes every FileZilla server type, accepts lengths counted by either the Windows or the macOS/Linux build, and still reads plain paths. Site-specific bookmarks inside a site are not imported and no longer replace the site's name or start directory.
- **WinSCP and Dreamweaver** sites also still import when they carry connection semantics AeroFTP cannot reproduce. WinSCP warns on `Tunnel=1` (SSH tunnel) and `FtpPasvMode=0`, on generic `ProxyMethod` (`1..=3` SOCKS4/SOCKS5/HTTP for FTP/FTPS sessions matching WinSCP's FTP engine; SOCKS4/SOCKS5 or HTTP when `ProxyHost` is set for WebDAV/S3 matching WinSCP's neon session setup; and any non-zero `ProxyMethod` for SFTP/SCP), and independently on `FtpProxyLogonType` for FTP/FTPS sessions. Dreamweaver `usefirewall=TRUE` and `usepasv=FALSE` produce explicit firewall or active-FTP warnings rather than being silently discarded.
- **PuTTY and s3cmd** likewise warn when `ProxyMethod` (with recognized values `0..=8` taking precedence, and absent or rejected `0xFFFFFFFF` falling back to legacy `ProxyType`/`ProxySOCKSVersion`) or `proxy_host` requires a proxy. The profiles still import and connect directly because those proxy settings are not imported.
- **Cyberduck** imports Backblaze B2 by routing it onto S3, and skips Azure and
  the OAuth providers it can read but cannot represent. A bookmark with `FTP Connect Mode` set to `active` still imports with an explicit warning; passive mode is unchanged. Cyberduck's separate global `ftp.connectmode` preference lives outside the imported `.duck` bookmark files and is not read.
- **rclone** FTP and SFTP remotes with `socks_proxy` or `http_proxy` still import with an explicit warning for the effective proxy rclone dials (`socks_proxy` takes precedence over `http_proxy`) that the proxy is not imported and AeroFTP will connect directly, including when the option is inherited from a dotted parent section (`[a]` for `[a.b]`) or resolved through goconfig `%(var)s` substitutions in an `rclone.conf` file unless cleared by an empty child key or an unset substitution. SFTP remotes with an effective external `ssh` command are skipped with a reason rather than imported as direct connections, because rclone ignores config `host`/`user`/`port` when `ssh` is set. Proxy values and commands, which may contain credentials, are not copied into warnings or skip reasons.
- **MobaXterm** positional session fields beyond host, port, and username are currently treated as opaque because MobaXterm does not publish a fixed schema for trailing gateway/proxy offsets; gateway and proxy warnings are not yet emitted.
- **Kopia, Duplicacy and restic** import B2 URLs onto S3 (tagged as Backblaze),
  and skip filesystem, GCS, Azure and OAuth remotes.
- **OpenSSH, PuTTY, AWS CLI, mc and s3cmd** are metadata or single-protocol
  bridges: SSH and PuTTY carry no secret, while AWS CLI, mc and s3cmd are
  S3-only credential files. AWS files are read as the AWS CLI reads them:
  `[DEFAULT]` fallback values, `[profile "name"]` and `[default]` in `config`,
  `=` or `:`, sub-sections, verbatim section names in `credentials`, static
  credentials selected as an atomic per-file set (shared credentials ahead of
  config without mixing keys or tokens across files), and the `s3` endpoint of
  a `services` section ahead of `endpoint_url`; endpoints carrying URL userinfo
  or query parameters are skipped, and a file the AWS CLI refuses (a section or
  key declared twice, or a malformed line) is refused. Profiles without a
  non-empty access key are reported as skipped, naming what they use instead (a
  role, SSO, `credential_process`), and profiles with an access key but no
  `aws_secret_access_key` are skipped. A profile with static keys and
  `role_arn` or SSO settings imports with those keys and a warning because the
  AWS CLI would sign in as another identity; unapplied settings (`ca_bundle`,
  FIPS, dual-stack, accelerate) also emit a warning. Imported STS session
  tokens are moved into the encrypted vault under owner-aware per-user storage
  and redacted from profile options, sharing the import vault-write cap with
  passwords; connecting or exporting a profile marked with a stored session
  token fails closed if its token cannot be read. `credential_cache` is an AWS
  CLI cache choice for assume-role, not a static credential. Export serializes
  concurrent writes to the same destination directory using a private lock
  under AeroFTP's own data root, stages both `0600` (owner-ACL on Windows) temp
  files before touching either target, plans both outputs together, preserves
  path-prefixed S3 endpoints, persists `config` before `credentials` (attempting
  to roll back active or side-file `config` or side-file `credentials` writes
  when they still match what was just published, preserving external edits it
  detects, though it cannot exclude a lockless external writer editing the file
  between revalidation and restoration or removal and rollback itself can fail,
  so check the reported rollback status before using the pair or retrying the
  export), and replaces an existing `config` or `credentials` file only when
  its typed line-2 marker carries both a filename-derived binding ID for the
  target credentials entry and a matching SHA-256 digest of the file body.
  Older AeroFTP exports carrying the previous unbound credentials marker are
  preserved as non-matching files, so re-exporting writes a new inactive side
  pair instead of replacing them. If either destination is a user file, a
  file `aws configure` edited, a pre-marker legacy AeroFTP export, another
  credentials pair's `config`, a symlink, or a directory, both active targets
  stay untouched, the complete pair goes to the first absent or AeroFTP-owned
  `.aeroftp` side files at write time with manual-merge warnings, and the
  export reports the actual written credentials path.
  A profile name with a space, tab or `'` is written as `[profile "name"]`; one
  with `"` or `\` is skipped when a config section is needed. When
  `aeroftp-cli export aws` runs without `--output`, it writes into a private
  `0700` temporary directory.

---

## Filen Desktop bridge notes

- The Filen Desktop local bridges (`filen-desktop-webdav`, `filen-desktop-s3`)
  default to `admin` / `admin` on their loopback server. AeroFTP applies the
  same blank-to-`admin`/`admin` fallback when those bridge presets are saved
  with empty credentials. As of v4.1.0 this fallback also applies on the
  headless paths (CLI, benchmark, MCP, schedulers), not only the GUI.
- Renaming an **empty** folder on a Filen S3 bridge cannot work: a virtual
  prefix has no underlying object for the S3 copy-then-delete rename to act on.
  AeroFTP returns an actionable message asking you to add a file inside the
  folder first, or to use the native Filen API or the WebDAV bridge for that
  operation.
- Creating a folder on `filen-desktop-s3` does not write a directory marker.
  Filen Desktop 3.x serves the bucket with `rclone serve s3`, which stored
  `PUT <name>/` as a zero-byte file and then answered 500 for a folder created
  inside it (the `aeroftp-bench` file left by a benchmark on 4.2.2 and earlier).
  Uploading a file under the name creates the folders. An empty folder is not
  kept until a file is uploaded into it, so it does not appear in a listing
  made straight after mkdir. A file that already has that name is left in
  place and mkdir says so. A download of a file just uploaded on this preset
  waits up to 30 seconds if the first GET answers 404, and returns as soon
  as the object is visible. `rclone serve s3 --vfs-write-back 5s` hides it
  for about 5 seconds and then copies it onto Filen. A zero-byte file
  that is already gone from the account but still in Filen Desktop's cache
  is invisible to that check: HEAD answers 404 and the upload PUT answers 500.
  Delete the file in Filen and clear the desktop cache before a retest.

---

*This matrix tracks the shipped bridge code and is updated on every bridge change.
Last reviewed for v4.2.5 (26 October 2026).*
