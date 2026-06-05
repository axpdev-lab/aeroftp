# Provider catalog

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
| Alibaba OSS | CN | paid plan | S3* |
| Amazon S3 | US | 5 GB 12-month trial | S3* |
| Azure Blob | US | 12-month trial | Blob* |
| Backblaze B2 | US | 10 GB | API, S3 |
| Blomp | US | 20 GB (referral bonus) | Swift |
| Box | US | 10 GB | OAuth |
| Cloudflare R2 | US | 10 GB (egress-free) | S3 |
| Cloudinary | US | credit-based | API |
| CloudMe | SE | 3 GB | WebDAV |
| DigitalOcean Spaces | US | paid plan | S3* |
| Drime | FR | 20 GB | API |
| DriveHQ | US | 1 GB | WebDAV |
| Dropbox | US | 2 GB | OAuth |
| Felicloud | - | Nextcloud host | WebDAV |
| FileLu | US | 1 GB | API, WebDAV, S3 |
| Filen | DE | 10 GB (E2E) | API, S3, WebDAV |
| GitHub | US | repo storage | API |
| GitLab | US | repo storage | API |
| Google Cloud Storage | US | 5 GB (always-free tier) | S3 |
| Google Drive | US | 15 GB | OAuth |
| Hetzner Storage Box | DE | paid plan | SFTP* |
| IDrive e2 | US | 10 GB | S3 |
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
| Oracle Cloud | US | 20 GB (always-free) | S3 |
| pCloud | CH | 10 GB | OAuth, WebDAV* |
| PixelUnion | EU | managed Immich | API* |
| Seafile | - | self-hosted | WebDAV |
| SourceForge | US | OSS hosting | SFTP |
| Storj | US | 25 GB (decentralized) | S3 |
| Tab.digital | IN | managed Nextcloud | WebDAV* |
| Tencent COS | CN | paid plan | S3* |
| Uploadcare | US | 3 GB (media CDN) | API |
| Wasabi | US | 30-day trial | S3* |
| Yandex Disk | RU | 5 GB | OAuth, WebDAV |
| Yandex Object Storage | RU | paid plan | S3* |
| Zoho WorkDrive | IN | 5 GB | OAuth |

<sub>50 providers, 61 connection methods. `*` marks a paid / credit-card-gated plan. HQ is the ISO 3166-1 alpha-2 of the company HQ (EU = pan-European). Free-tier sizes are approximate: verify with the provider.</sub>

<!-- END PROVIDERS-TABLE -->
