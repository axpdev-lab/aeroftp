//! Routing hint table.
//!
//! Populated from the Phase A benchmark matrix:
//! - Round 1 isolated 2026-05-24: `docs/dev/benchmarks/2026-05-24_phaseA-postfix/`
//! - Overnight cross-Nextcloud 2026-05-25:
//!   `docs/dev/benchmarks/2026-05-25_phaseA-cross-nextcloud/SUMMARY.md`
//!
//! Gate definitions:
//! - throughput within +-5% = "ok" (DAG default)
//! - throughput delta >= +5% = WIN (DAG default, would not undo)
//! - throughput delta <= -5% = FAIL (candidate for Legacy override)
//! - latency (list/stat) +-10% = ok
//!
//! The size thresholds use binary units (1 MiB = 1 << 20 bytes) because
//! every Phase A report uses those units. The single-vs-multipart fan-out
//! decision is NOT made here: it lives in `transfer_dag::builder` and is gated
//! on the provider's `multipart_threshold` capability. The router only selects
//! the engine; it does not size parts.

use super::{Decision, Engine, Operation, ProviderHint, RouteContext};
use crate::providers::types::ProviderType;

/// Map a [`ProviderType`] (the protocol-level enum stored in the provider
/// registry) to a [`ProviderHint`] (the coarse routing class used by the
/// hint table).
///
/// For WebDAV the variant resolution is layered to handle every observed
/// shape:
/// 1. The `preset_id` (from the profile `providerId` field, e.g.
///    `nextcloud`, `felicloud`, `tabdigital`, `koofr`) is the most
///    reliable signal: it is set explicitly by the user when picking the
///    preset and survives any URL shape change. Matched first.
/// 2. The configured URL is inspected. The `/remote.php/dav/files/`
///    pattern (Nextcloud canonical), the `koofr.net` hostname (Koofr
///    gateway), and the hostname allowlist for known Nextcloud-backed
///    SaaS providers (FeliCloud, Tab.digital) catch profiles whose
///    URL is canonical without needing a preset id.
/// 3. Anything else is classified as WebDAV vanilla (Apache mod_dav,
///    lighttpd, nginx_dav, generic WebDAV server).
///
/// Any provider not explicitly mapped falls back to
/// [`ProviderHint::OAuthCloud`], which is the safest default because all
/// OAuth-backed cloud providers measured so far stay within the gate
/// under DAG.
pub fn from_provider_type(
    pt: ProviderType,
    server_url: Option<&str>,
    preset_id: Option<&str>,
) -> ProviderHint {
    match pt {
        ProviderType::Sftp => ProviderHint::Sftp,
        ProviderType::Ftp | ProviderType::Ftps => ProviderHint::Ftp,
        ProviderType::S3 => ProviderHint::S3,
        ProviderType::Backblaze => ProviderHint::B2,
        ProviderType::Azure => ProviderHint::AzureBlob,
        ProviderType::WebDav => webdav_variant(server_url, preset_id),
        // Cloud providers (OAuth or token-based) without per-class
        // measurement yet. DAG default until a benchmark says otherwise.
        ProviderType::AeroCloud
        | ProviderType::GoogleDrive
        | ProviderType::GooglePhotos
        | ProviderType::Dropbox
        | ProviderType::OneDrive
        | ProviderType::Mega
        | ProviderType::Box
        | ProviderType::PCloud
        | ProviderType::Filen
        | ProviderType::FourShared
        | ProviderType::ZohoWorkdrive
        | ProviderType::Internxt
        | ProviderType::KDrive
        | ProviderType::Jottacloud
        | ProviderType::DrimeCloud
        | ProviderType::FileLu
        | ProviderType::Koofr
        | ProviderType::OpenDrive
        | ProviderType::YandexDisk
        | ProviderType::GitHub
        | ProviderType::GitLab
        | ProviderType::Swift
        | ProviderType::Immich
        | ProviderType::ImageKit
        | ProviderType::Uploadcare
        | ProviderType::Cloudinary
        // Synthetic local mount; never routed through the transfer router.
        | ProviderType::AeroVaultMount => ProviderHint::OAuthCloud,
    }
}

/// Layered WebDAV variant resolution. Used both from
/// [`from_provider_type`] and (in a future iteration) directly when the
/// caller has only the URL or only the preset_id.
fn webdav_variant(server_url: Option<&str>, preset_id: Option<&str>) -> ProviderHint {
    // 1. Preset id: the most reliable signal. The user picked it
    //    explicitly in the connect flow and it travels with the profile.
    if let Some(pid) = preset_id {
        let p = pid.trim().to_ascii_lowercase();
        // Nextcloud-backed SaaS presets (or self-hosted Nextcloud).
        if matches!(
            p.as_str(),
            "nextcloud"
                | "owncloud"
                | "felicloud"
                | "felicloud-webdav"
                | "tabdigital"
                | "tabdigital-webdav"
                | "magentacloud"
                | "magentacloud-webdav"
        ) {
            return ProviderHint::WebDavNextcloud;
        }
        // Gateway-style WebDAV (per-request server overhead, big DAG win).
        if matches!(p.as_str(), "koofr" | "koofr-webdav") {
            return ProviderHint::WebDavGateway;
        }
    }
    // 2. URL inspection: covers profiles without a preset id but whose
    //    URL is canonical, plus the legacy URL-based fallback.
    if let Some(url) = server_url {
        let lower = url.to_ascii_lowercase();
        if lower.contains("/remote.php/dav/files/") {
            return ProviderHint::WebDavNextcloud;
        }
        // Known Nextcloud-backed SaaS hostnames whose URL is sometimes
        // configured without the `/remote.php/dav/files/` path.
        if lower.contains("felicloud.com")
            || lower.contains("tab.digital")
            || lower.contains("magentacloud.de")
        {
            return ProviderHint::WebDavNextcloud;
        }
        if lower.contains("koofr.net") {
            return ProviderHint::WebDavGateway;
        }
    }
    // 3. Vanilla: Apache mod_dav, lighttpd mod_webdav, nginx_dav,
    //    self-hosted WebDAV without a Nextcloud signature.
    ProviderHint::WebDavVanilla
}

pub fn recommend(ctx: RouteContext) -> Decision {
    match ctx.provider {
        ProviderHint::WebDavVanilla => recommend_webdav_vanilla(ctx),
        ProviderHint::WebDavNextcloud => recommend_webdav_nextcloud(ctx),
        ProviderHint::WebDavGateway => {
            default_dag("WebDAV gateway (Koofr): WIN +132% upload 1G, +161% download 100M")
        }
        ProviderHint::S3 => recommend_s3(ctx),
        ProviderHint::B2 => recommend_b2(ctx),
        ProviderHint::AzureBlob => {
            default_dag("Azure Blob: native Put Block multipart + Range; DAG default")
        }
        ProviderHint::Sftp => default_dag("SFTP: borderline within +-3% on Phase A, DAG default"),
        ProviderHint::Ftp => default_dag("FTP: Phase A gate green"),
        ProviderHint::OAuthCloud => default_dag("OAuth cloud: no measured regression, DAG default"),
        ProviderHint::Local => Decision {
            engine: Engine::Legacy,
            reason: "local-to-local: handled upstream by Router::pick",
        },
    }
}

fn recommend_webdav_vanilla(ctx: RouteContext) -> Decision {
    // Downloads never fan out in the DAG (multipart is upload-only), so the
    // shaped graph can only add wrapper cost on the download direction. The
    // 2026-05-29 lab re-benchmark measured WebDAV download regressing under the
    // DAG, so route downloads to the provider-direct Legacy path (the internal
    // range/concurrent-download logic lives inside `download()` and is
    // preserved either way). Uploads have no multipart on vanilla WebDAV so the
    // DAG is also pure overhead, but uploads were within gate on Phase A; keep
    // them on the default DAG for now and revisit if a re-benchmark regresses.
    match ctx.operation {
        Operation::Download => {
            default_legacy("WebDAV vanilla download: DAG adds wrapper cost, no fan-out (audit PS2)")
        }
        Operation::Upload => default_dag("WebDAV vanilla upload: Phase A within gate; DAG default"),
    }
}

fn recommend_webdav_nextcloud(ctx: RouteContext) -> Decision {
    // Upload stays on the DAG: Nextcloud chunked v2 now fans out into parallel
    // `UploadPart` nodes (clone_for_transfer is wired for Nextcloud), which is
    // the path that pays off on large uploads. Download routes to Legacy: it
    // never fans out, so the DAG only adds the node-graph wrapper, which the
    // 2026-05-29 lab re-benchmark showed regressing (audit PS2).
    match ctx.operation {
        Operation::Download => default_legacy(
            "WebDAV Nextcloud download: DAG adds wrapper cost, no fan-out (audit PS2)",
        ),
        Operation::Upload => {
            default_dag("WebDAV Nextcloud upload: parallel chunked v2 fan-out via shaped graph")
        }
    }
}

fn recommend_s3(_ctx: RouteContext) -> Decision {
    // Phase A finding (round 1 2026-05-24): S3 multipart upload WIN big after
    // the chunk-parallel fix (`b8217fe1`), upload 1G +38.6%. The single-vs-
    // multipart decision is size-gated in the builder via multipart_threshold
    // (audit DISP-01); the router only selects the engine, so there is no
    // size branch here (the previous MEDIUM_CUTOFF arm was dead: both arms
    // returned DAG, audit DISP-04).
    default_dag(
        "S3: multipart fan-out for large uploads, single PUT below threshold (builder-gated)",
    )
}

fn recommend_b2(_ctx: RouteContext) -> Decision {
    // B2 shares the multipart-parallel path with S3 via the same
    // clone-safe provider worker; assume same WIN shape until live
    // measurement says otherwise.
    default_dag("B2: shares multipart fan-out path with S3 (Phase A WIN)")
}

fn default_dag(reason: &'static str) -> Decision {
    Decision {
        engine: Engine::Dag,
        reason,
    }
}

fn default_legacy(reason: &'static str) -> Decision {
    Decision {
        engine: Engine::Legacy,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::super::Router;
    use super::*;

    fn ctx(p: ProviderHint, op: Operation, size: u64) -> RouteContext {
        RouteContext::new(p, op, size)
    }

    // ── WebDAV vanilla: revalidated DAG default ───────────────────────

    #[test]
    fn webdav_vanilla_download_routes_to_legacy() {
        // Downloads never fan out, so the DAG only adds wrapper cost; the
        // 2026-05-29 re-benchmark routed WebDAV download to Legacy (audit PS2).
        for size in [1024 * 1024, 100 * 1024 * 1024, 1 << 30] {
            let d = Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Download, size));
            assert_eq!(d.engine, Engine::Legacy, "size {size} should be Legacy");
        }
    }

    #[test]
    fn webdav_vanilla_upload_always_dag() {
        // Upload was within gate on every measured size for mod_dav.
        let d_small = Router::new().pick(ctx(
            ProviderHint::WebDavVanilla,
            Operation::Upload,
            1024 * 1024,
        ));
        let d_med = Router::new().pick(ctx(
            ProviderHint::WebDavVanilla,
            Operation::Upload,
            100 * 1024 * 1024,
        ));
        let d_big =
            Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Upload, 1 << 30));
        assert_eq!(d_small.engine, Engine::Dag);
        assert_eq!(d_med.engine, Engine::Dag);
        assert_eq!(d_big.engine, Engine::Dag);
    }

    // ── WebDAV Nextcloud / gateway: always DAG ────────────────────────

    #[test]
    fn webdav_nextcloud_download_routes_to_legacy_upload_stays_dag() {
        // Download has no fan-out so it goes Legacy (audit PS2); upload keeps
        // the DAG because Nextcloud chunked v2 now fans out in parallel.
        let down = Router::new().pick(ctx(
            ProviderHint::WebDavNextcloud,
            Operation::Download,
            100 * 1024 * 1024,
        ));
        assert_eq!(down.engine, Engine::Legacy);
        let up = Router::new().pick(ctx(
            ProviderHint::WebDavNextcloud,
            Operation::Upload,
            100 * 1024 * 1024,
        ));
        assert_eq!(up.engine, Engine::Dag);
    }

    #[test]
    fn webdav_gateway_upload_1g_stays_on_dag() {
        // Koofr WAN upload 1G: +132.9% WIN on Phase A. Never route to
        // Legacy unless user explicitly forces it.
        let d = Router::new().pick(ctx(ProviderHint::WebDavGateway, Operation::Upload, 1 << 30));
        assert_eq!(d.engine, Engine::Dag);
        assert!(d.reason.contains("Koofr") || d.reason.contains("gateway"));
    }

    // ── S3 / B2 / Azure: multipart WIN ────────────────────────────────

    #[test]
    fn s3_upload_1g_routes_to_dag_with_multipart_reason() {
        let d = Router::new().pick(ctx(ProviderHint::S3, Operation::Upload, 1 << 30));
        assert_eq!(d.engine, Engine::Dag);
        assert!(d.reason.contains("multipart"));
    }

    #[test]
    fn b2_inherits_s3_multipart_path() {
        let d = Router::new().pick(ctx(ProviderHint::B2, Operation::Upload, 500 * 1024 * 1024));
        assert_eq!(d.engine, Engine::Dag);
    }

    #[test]
    fn azure_blob_routes_to_dag_with_native_multipart_reason() {
        let d = Router::new().pick(ctx(
            ProviderHint::AzureBlob,
            Operation::Upload,
            500 * 1024 * 1024,
        ));
        assert_eq!(d.engine, Engine::Dag);
        assert!(d.reason.contains("Put Block"));
    }

    // ── SFTP / FTP / OAuth / Local ────────────────────────────────────

    #[test]
    fn sftp_always_dag() {
        let d = Router::new().pick(ctx(ProviderHint::Sftp, Operation::Download, 1 << 30));
        assert_eq!(d.engine, Engine::Dag);
    }

    #[test]
    fn ftp_always_dag() {
        let d = Router::new().pick(ctx(ProviderHint::Ftp, Operation::Upload, 100 * 1024 * 1024));
        assert_eq!(d.engine, Engine::Dag);
    }

    #[test]
    fn oauth_cloud_always_dag() {
        let d = Router::new().pick(ctx(ProviderHint::OAuthCloud, Operation::Download, 1 << 30));
        assert_eq!(d.engine, Engine::Dag);
    }

    // ── ProviderType -> ProviderHint detection ────────────────────────

    #[test]
    fn detect_sftp_ftp_s3_b2_azure() {
        assert_eq!(
            from_provider_type(ProviderType::Sftp, None, None),
            ProviderHint::Sftp
        );
        assert_eq!(
            from_provider_type(ProviderType::Ftp, None, None),
            ProviderHint::Ftp
        );
        assert_eq!(
            from_provider_type(ProviderType::Ftps, None, None),
            ProviderHint::Ftp
        );
        assert_eq!(
            from_provider_type(ProviderType::S3, None, None),
            ProviderHint::S3
        );
        assert_eq!(
            from_provider_type(ProviderType::Backblaze, None, None),
            ProviderHint::B2
        );
        assert_eq!(
            from_provider_type(ProviderType::Azure, None, None),
            ProviderHint::AzureBlob
        );
    }

    #[test]
    fn detect_webdav_nextcloud_via_url_pattern() {
        let hint = from_provider_type(
            ProviderType::WebDav,
            Some("https://cloud.felicloud.com/remote.php/dav/files/aeroftp@axpdev.it/"),
            None,
        );
        assert_eq!(hint, ProviderHint::WebDavNextcloud);
    }

    #[test]
    fn detect_webdav_nextcloud_via_preset_id() {
        // axpbuntu lab Nextcloud: URL is bare (https://cloud.lab.example.test
        // without /remote.php/dav/files/) but the preset id is `nextcloud`.
        // The preset wins because it travels with the profile.
        let hint = from_provider_type(
            ProviderType::WebDav,
            Some("https://cloud.lab.example.test"),
            Some("nextcloud"),
        );
        assert_eq!(hint, ProviderHint::WebDavNextcloud);
    }

    #[test]
    fn detect_webdav_nextcloud_via_known_hostname() {
        // Tab.digital nude URL (no /remote.php/dav/files/, no preset id):
        // hostname allowlist catches it.
        let hint = from_provider_type(
            ProviderType::WebDav,
            Some("https://fie.nl.tab.digital:443"),
            None,
        );
        assert_eq!(hint, ProviderHint::WebDavNextcloud);
    }

    #[test]
    fn detect_webdav_gateway_via_koofr_hostname() {
        let hint = from_provider_type(
            ProviderType::WebDav,
            Some("https://app.koofr.net/dav/Koofr"),
            None,
        );
        assert_eq!(hint, ProviderHint::WebDavGateway);
    }

    #[test]
    fn detect_webdav_gateway_via_preset_id() {
        let hint = from_provider_type(
            ProviderType::WebDav,
            Some("https://something-other.example.com"),
            Some("koofr"),
        );
        assert_eq!(hint, ProviderHint::WebDavGateway);
    }

    #[test]
    fn detect_webdav_vanilla_default() {
        // mod_dav puro: no /remote.php/dav/files/, no known hostname,
        // no preset id matching a Nextcloud/Koofr-like family.
        assert_eq!(
            from_provider_type(
                ProviderType::WebDav,
                Some("https://dav.lab.example.test/"),
                None
            ),
            ProviderHint::WebDavVanilla
        );
        assert_eq!(
            from_provider_type(ProviderType::WebDav, None, None),
            ProviderHint::WebDavVanilla
        );
    }

    #[test]
    fn preset_id_beats_url_when_both_present() {
        // If somebody types a Nextcloud canonical URL but explicitly
        // picks the `drivehq` preset, treat as vanilla. Preset_id wins
        // only when it matches a known family; mismatch falls through
        // to URL inspection. This test pins the *fallback to URL*
        // behaviour when preset is not in the known set.
        let hint = from_provider_type(
            ProviderType::WebDav,
            Some("https://example.com/remote.php/dav/files/alice/"),
            Some("drivehq"),
        );
        // URL pattern still wins because the preset_id doesn't match
        // any known Nextcloud-family entry; that's the correct
        // conservative default (drivehq is not in the Nextcloud list).
        assert_eq!(hint, ProviderHint::WebDavNextcloud);
    }

    #[test]
    fn detect_oauth_cloud_for_known_providers() {
        for pt in [
            ProviderType::GoogleDrive,
            ProviderType::Dropbox,
            ProviderType::OneDrive,
            ProviderType::Box,
            ProviderType::PCloud,
            ProviderType::Mega,
            ProviderType::Filen,
            ProviderType::Koofr,
            ProviderType::Cloudinary,
        ] {
            assert_eq!(
                from_provider_type(pt, None, None),
                ProviderHint::OAuthCloud,
                "expected OAuthCloud for {:?}",
                pt
            );
        }
    }
}
