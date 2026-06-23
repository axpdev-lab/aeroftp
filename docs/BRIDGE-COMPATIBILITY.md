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
- **13 AeroFTP-native providers have no external bridge at all** because no other
  tool speaks their API. They round-trip only through AeroFTP's own `.aeroftp`
  export: aerocloud, 4shared, Zoho WorkDrive, Internxt, kDrive, Drime, FileLu,
  GitHub, GitLab, Immich, ImageKit, Uploadcare, Cloudinary.

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

Whether a provider also **exports** depends on what its credential needs. Ones
that authenticate with a recoverable secret (user + password or account + key)
export fully. The OAuth providers cannot: rclone refreshes them with its own
provider-issued client, so AeroFTP cannot emit a usable token and they are
import-only. Jottacloud is the exception: it is not classic OAuth (AeroFTP
classifies it as an API provider) and AeroFTP persists its OIDC refresh token, so
the exported remote refreshes on rclone's first use (added in v4.0.9).

| Provider | rclone | rclone backend | Credentials carried |
|----------|:--:|----|----|
| MEGA            | IE | mega       | user + password |
| Azure Blob      | IE | azureblob  | account + key |
| OpenStack Swift | IE | swift      | user + key |
| Koofr           | IE | koofr      | user + password |
| Jottacloud      | IE | jottacloud | OIDC refresh token (AeroFTP-persisted); the exported remote refreshes on first use (v4.0.9) |
| Filen           | I. | filen      | import only: export needs the provider's own auth client |
| OpenDrive       | I. | opendrive  | import only |
| Backblaze B2    | I. | b2         | import only (native B2, not S3) |
| Google Drive    | I. | drive      | import only: OAuth, needs rclone's own auth flow |
| Dropbox         | I. | dropbox    | import only: OAuth |
| OneDrive        | I. | onedrive   | import only: OAuth |
| Box             | I. | box        | import only: OAuth |
| pCloud          | I. | pcloud     | import only: OAuth |
| Yandex Disk     | I. | yandex     | import only: OAuth |

## Per-tool exportable protocols

Which saved profiles each tool will accept on export. (Import is not filtered this
way: an importer reads whatever connection types it recognizes in the file.)

| Tool | Exportable protocols |
|-----|----------------------|
| rclone | FTP, FTPS, SFTP, WebDAV, S3, MEGA, Azure, Swift, Koofr, Jottacloud |
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
  Yandex, plus Filen) are import-only: rclone re-authenticates them through its
  own provider-issued client, so AeroFTP cannot hand it a working token. The
  recoverable-secret providers (MEGA, Azure, Swift, Koofr) and Jottacloud do
  export.
- **Cyberduck** imports Backblaze B2 by routing it onto S3, and skips Azure and
  the OAuth providers it can read but cannot represent.
- **Kopia, Duplicacy and restic** import B2 URLs onto S3 (tagged as Backblaze),
  and skip filesystem, GCS, Azure and OAuth remotes.
- **OpenSSH, PuTTY, AWS CLI, mc and s3cmd** are metadata or single-protocol
  bridges: SSH and PuTTY carry no secret, while AWS CLI, mc and s3cmd are
  S3-only credential files.

---

*This matrix tracks the shipped bridge code and is updated on every bridge change.
Last reviewed for v4.0.9.*
