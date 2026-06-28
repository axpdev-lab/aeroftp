# Provider catalog

> _Last updated: 2026-06-22_

Authoritative, always-in-sync list of every storage company AeroFTP can connect
to, with its HQ country, free tier and connection methods.

This table is **generated** from the single source of truth
[`src/components/providerCatalog.ts`](../src/components/providerCatalog.ts). Do not
edit it by hand: run `npm run gen:providers-table` to regenerate, and a drift-guard
test fails CI if it falls out of sync. The README integration grid and the
aeroftp.app / docs.aeroftp.app provider tables mirror this list.

<!-- BEGIN PROVIDERS-TABLE -->

<!-- Generated from src/components/providerCatalog.ts by `npm run gen:providers-table`. Do not edit by hand. -->

| Provider | HQ | Free tier | Connection methods |
| --- | --- | --- | --- |
| 4shared | VG | 15 GB | OAuth, WebDAV |
| Alibaba OSS | CN | 5 GB (overseas only, card req.) | S3* |
| Amazon S3 | US | 5 GB (always-free, card req.) | S3* |
| Azure Blob | US | 5 GB (always-free, card req.) | Blob* |
| Backblaze B2 | US | 10 GB | API, S3 |
| Box | US | 10 GB | OAuth |
| Cloudflare R2 | US | 10 GB (egress-free, card req.) | S3* |
| Cloudinary | US | credit-based | API |
| CloudMe | SE | 3 GB | WebDAV |
| DigitalOcean Spaces | US | paid plan | S3* |
| Drime | FR | 20 GB | API |
| DriveHQ | US | 1 GB | WebDAV |
| Dropbox | US | 2 GB | OAuth |
| Felicloud | - | 10 GB (Nextcloud host) | WebDAV |
| FileLu | US | 10 GB | API, WebDAV, S3 |
| Filen | DE | 10 GB (E2E) | API, S3, WebDAV |
| GitHub | US | repo storage | API |
| GitLab | US | repo storage | API |
| Google Cloud Storage | US | 5 GB (always-free, card req.) | S3* |
| Google Drive | US | 15 GB | OAuth |
| Hetzner Storage Box | DE | paid plan | SFTP* |
| IDrive e2 | US | 7-day trial | S3* |
| ImageKit | IN | 20 GB (media CDN) | API |
| Immich | - | self-hosted | API |
| InfiniCloud | JP | 20 GB | WebDAV |
| Internxt | ES | 1 GB (E2E) | API |
| Jianguoyun | CN | 1 GB (monthly traffic cap) | WebDAV |
| Jottacloud | NO | 5 GB | API |
| kDrive | CH | 15 GB | API |
| Koofr | SI | 10 GB | API, WebDAV |
| MEGA | NZ | 20 GB (E2E) | API, MEGAcmd |
| MEGA S4 Object Storage | EU | Pro plan | S3* |
| MinIO | - | self-hosted | S3 |
| Nextcloud | - | self-hosted | WebDAV |
| OneDrive | US | 5 GB | OAuth |
| OpenDrive | US | 5 GB | API, WebDAV |
| Oracle Cloud | US | 20 GB (always-free, card req.) | S3* |
| pCloud | CH | 10 GB | OAuth, WebDAV* |
| PixelUnion | EU | 16 GB (managed Immich) | API |
| Seafile | - | self-hosted | WebDAV |
| SourceForge | US | OSS hosting | SFTP |
| Storj | US | 30-day trial | S3* |
| Tab.digital | IN | 8 GB (managed Nextcloud) | WebDAV |
| Tencent COS | CN | 6-month trial | S3* |
| Uploadcare | US | 3 GB (media CDN) | API |
| Wasabi | US | 30-day trial | S3* |
| Yandex Disk | RU | 5 GB | OAuth, WebDAV |
| Yandex Object Storage | RU | 1 GB (always-free, card req.) | S3* |
| Zoho WorkDrive | IN | 5 GB | OAuth |

<sub>49 providers, 60 connection methods. `*` marks a paid / credit-card-gated plan. HQ is the ISO 3166-1 alpha-2 of the company HQ (EU = pan-European). Free-tier sizes are approximate: verify with the provider.</sub>

<!-- END PROVIDERS-TABLE -->

## Provider notes

- **Filen (S3 bridge):** renaming an *empty* folder over the Filen S3 bridge
  cannot work, because a virtual prefix has no underlying object for the S3
  copy-then-delete rename to act on. AeroFTP returns an actionable message
  asking you to add a file inside the folder first, or to use the native Filen
  API or the WebDAV bridge for that operation.
- **OpenDrive (host self-heal):** a profile switched from the WebDAV preset
  (`webdav.opendrive.com`) into native API mode now normalizes its host to
  `dev.opendrive.com`, so the native session flow no longer leaks the WebDAV
  hostname.
- **OpenDrive (privacy levels):** OpenDrive models a three-level access scheme
  per file and folder: **private** (not listed or shared, reachable only by the
  owner), **public** (anyone with the link can access; searchable) and
  **hidden** (reachable by direct link only; not searchable). You can read and
  change the level on an existing item from **Properties > Permissions** (a
  multi-selection applies the same level to every selected item), set a
  per-account **"Default privacy for new items"** on the Quick Connect form so
  new uploads and folders inherit it, and drive the same model from the CLI with
  `access`, `put --access` and `mkdir --access` (see the CLI guide). CLI creates
  default to **private** when no level is given; folder privacy cascades to
  children server-side. (issue #252)
