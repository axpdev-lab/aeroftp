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

- AeroFTP connects over **5 transfer protocols** (FTP, FTPS, SFTP, WebDAV, S3)
  plus roughly **28 cloud providers**.
- **15 external tools** have an import/export bridge.
- The transfer protocols interoperate widely across tools. The native cloud
  providers interoperate **only through rclone**: no other tool's config format
  can express them.
- **11 AeroFTP-native providers have no external bridge at all** because no other
  tool speaks their API. They round-trip only through AeroFTP's own `.aeroftp`
  export: aerocloud, 4shared, kDrive, Drime, FileLu, GitHub, GitLab, Immich,
  ImageKit, Uploadcare, Cloudinary.

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
custom S3. WebDAV vendors (Nextcloud, ownCloud, and custom WebDAV for
SharePoint / Fastmail / Koofr-dav) likewise ride WebDAV.

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

## Per-tool exportable protocols

Which saved profiles each tool will accept on export. (Import is not filtered this
way: an importer reads whatever connection types it recognizes in the file.)

| Tool | Exportable protocols |
|-----|----------------------|
| rclone | FTP, FTPS, SFTP, WebDAV, S3, MEGA, Internxt, Azure, Swift, Koofr, Jottacloud, Filen, Google Drive, Dropbox, OneDrive, Box, pCloud, Yandex, Zoho WorkDrive, OpenDrive, Backblaze B2 |
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
- **Cyberduck** imports Backblaze B2 by routing it onto S3, and skips Azure and
  the OAuth providers it can read but cannot represent.
- **Kopia, Duplicacy and restic** import B2 URLs onto S3 (tagged as Backblaze),
  and skip filesystem, GCS, Azure and OAuth remotes.
- **OpenSSH, PuTTY, AWS CLI, mc and s3cmd** are metadata or single-protocol
  bridges: SSH and PuTTY carry no secret, while AWS CLI, mc and s3cmd are
  S3-only credential files.

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

---

*This matrix tracks the shipped bridge code and is updated on every bridge change.
Last reviewed for v4.1.0.*
