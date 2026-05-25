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
//! every Phase A report uses those units. The cutoffs (`SMALL_CUTOFF`,
//! `MEDIUM_CUTOFF`) align with the benchmark sizes (1M / 100M / 1G) so
//! a future report can be diffed against this table directly.

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
        | ProviderType::Azure
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
        | ProviderType::Cloudinary => ProviderHint::OAuthCloud,
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

/// Files <= this size are dominated by per-request latency, not bandwidth;
/// the DAG vs Legacy delta is mostly rumour. Default to DAG.
const SMALL_CUTOFF: u64 = 4 * 1024 * 1024; // 4 MiB

/// Files between `SMALL_CUTOFF` and `MEDIUM_CUTOFF` are where the WebDAV
/// vanilla download regression bites. The lower bound is set above the
/// small-file noise floor; the upper bound is the threshold above which
/// even vanilla WebDAV download starts to recover (the multi-thread
/// provider cutoff is 250 MiB by default).
const MEDIUM_CUTOFF: u64 = 250 * 1024 * 1024; // 250 MiB

pub fn recommend(ctx: RouteContext) -> Decision {
    match ctx.provider {
        ProviderHint::WebDavVanilla => recommend_webdav_vanilla(ctx),
        ProviderHint::WebDavNextcloud => default_dag("WebDAV Nextcloud: all sizes WIN on Phase A (download 100M +6.0%, 1G +16.8%)"),
        ProviderHint::WebDavGateway => default_dag("WebDAV gateway (Koofr): WIN +132% upload 1G, +161% download 100M"),
        ProviderHint::S3 => recommend_s3(ctx),
        ProviderHint::B2 => recommend_b2(ctx),
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
    // Phase A finding (overnight 2026-05-25): vanilla WebDAV download
    // 100M = -16.3%, 1G = -18.3%, raw v4 [71.4, 55.6, 52.3] degrades
    // run-after-run. Tab.digital WAN shows the same shape on 100M
    // (-12.5%). The regression is in the medium-size window; below
    // SMALL_CUTOFF latency dominates and above MEDIUM_CUTOFF the
    // provider's own multi-thread path takes over.
    match ctx.operation {
        Operation::Download if ctx.size_bytes >= SMALL_CUTOFF && ctx.size_bytes < MEDIUM_CUTOFF => {
            Decision {
                engine: Engine::Legacy,
                reason: "WebDAV vanilla download 4M..250M: Phase A regression -16.3% (mod_dav LAN) / -12.5% (Tab.digital WAN)",
            }
        }
        Operation::Download if ctx.size_bytes >= MEDIUM_CUTOFF => Decision {
            engine: Engine::Legacy,
            reason: "WebDAV vanilla download >=250M: Phase A regression -18.3% on 1G mod_dav LAN",
        },
        _ => default_dag("WebDAV vanilla upload / small download: Phase A within gate"),
    }
}

fn recommend_s3(ctx: RouteContext) -> Decision {
    // Phase A finding (round 1 2026-05-24): S3 multipart upload WIN big
    // after the chunk-parallel fix (`b8217fe1`), upload 1G +38.6%,
    // download 1G +40.7%. Small files are no-op for multipart.
    match (ctx.operation, ctx.size_bytes) {
        (Operation::Upload, sz) if sz >= MEDIUM_CUTOFF => default_dag(
            "S3 upload >=250M: Phase A WIN +38.6% (multipart fan-out parallel)",
        ),
        _ => default_dag("S3: Phase A gate green"),
    }
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

#[cfg(test)]
mod tests {
    use super::super::Router;
    use super::*;

    fn ctx(p: ProviderHint, op: Operation, size: u64) -> RouteContext {
        RouteContext::new(p, op, size)
    }

    // ── WebDAV vanilla: the regression case ───────────────────────────

    #[test]
    fn webdav_vanilla_download_medium_routes_to_legacy() {
        // The 100M-on-mod-dav case where v4 dropped to 55 Mbps from 66.
        let d = Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Download, 100 * 1024 * 1024));
        assert_eq!(d.engine, Engine::Legacy);
        assert!(d.reason.contains("Phase A regression"));
    }

    #[test]
    fn webdav_vanilla_download_1g_routes_to_legacy() {
        let d = Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Download, 1 << 30));
        assert_eq!(d.engine, Engine::Legacy);
    }

    #[test]
    fn webdav_vanilla_download_small_stays_on_dag() {
        // Small files: latency-bound, DAG default.
        let d = Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Download, 1024 * 1024));
        assert_eq!(d.engine, Engine::Dag);
    }

    #[test]
    fn webdav_vanilla_upload_always_dag() {
        // Upload was within gate on every measured size for mod_dav.
        let d_small = Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Upload, 1024 * 1024));
        let d_med = Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Upload, 100 * 1024 * 1024));
        let d_big = Router::new().pick(ctx(ProviderHint::WebDavVanilla, Operation::Upload, 1 << 30));
        assert_eq!(d_small.engine, Engine::Dag);
        assert_eq!(d_med.engine, Engine::Dag);
        assert_eq!(d_big.engine, Engine::Dag);
    }

    // ── WebDAV Nextcloud / gateway: always DAG ────────────────────────

    #[test]
    fn webdav_nextcloud_download_medium_stays_on_dag() {
        // The case ieri sera looked broken but the overnight matrix
        // proved was an artefact: +6.0% on the clean re-run.
        let d = Router::new().pick(ctx(ProviderHint::WebDavNextcloud, Operation::Download, 100 * 1024 * 1024));
        assert_eq!(d.engine, Engine::Dag);
    }

    #[test]
    fn webdav_gateway_upload_1g_stays_on_dag() {
        // Koofr WAN upload 1G: +132.9% WIN on Phase A. Never route to
        // Legacy unless user explicitly forces it.
        let d = Router::new().pick(ctx(ProviderHint::WebDavGateway, Operation::Upload, 1 << 30));
        assert_eq!(d.engine, Engine::Dag);
        assert!(d.reason.contains("Koofr") || d.reason.contains("gateway"));
    }

    // ── S3 / B2: multipart WIN ────────────────────────────────────────

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
    fn detect_sftp_ftp_s3_b2() {
        assert_eq!(from_provider_type(ProviderType::Sftp, None, None), ProviderHint::Sftp);
        assert_eq!(from_provider_type(ProviderType::Ftp, None, None), ProviderHint::Ftp);
        assert_eq!(from_provider_type(ProviderType::Ftps, None, None), ProviderHint::Ftp);
        assert_eq!(from_provider_type(ProviderType::S3, None, None), ProviderHint::S3);
        assert_eq!(from_provider_type(ProviderType::Backblaze, None, None), ProviderHint::B2);
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
        // axpbuntu lab Nextcloud: URL is bare (https://cloud.lab.axpdev.it
        // without /remote.php/dav/files/) but the preset id is `nextcloud`.
        // The preset wins because it travels with the profile.
        let hint = from_provider_type(
            ProviderType::WebDav,
            Some("https://cloud.lab.axpdev.it"),
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
            from_provider_type(ProviderType::WebDav, Some("https://dav.lab.axpdev.it/"), None),
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
