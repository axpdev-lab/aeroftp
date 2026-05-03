// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

//! Source-of-truth implementation of the My Servers Table dedup logic.
//!
//! Two profiles backed by the same physical disk (Koofr WebDAV + REST + S3,
//! Yandex Disk WebDAV + OAuth, multiple Wasabi buckets under the same access
//! key, ...) must collapse into a single entry when computing storage totals.
//! Otherwise the footer double- or triple-counts the same gigabytes.
//!
//! `dedup_key` produces a canonical string per profile. `aggregate` consumes a
//! list of profiles and returns both the global summary and a per-protocol
//! class breakdown.
//!
//! The TypeScript twin lives at `src/utils/storageDedup.ts` and must produce
//! the same output for the same input — `cargo test` cross-checks the six
//! canonical scenarios documented in the Phase 4 handoff.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Subset of `ServerProfile` consumed by the dedup algorithm. Both the CLI
/// (working on `serde_json::Value`) and the lib API (working on typed structs)
/// can build a `ProfileView` cheaply.
#[derive(Clone, Debug, Default)]
pub struct ProfileView<'a> {
    pub id: &'a str,
    pub protocol: &'a str,
    pub provider_id: Option<&'a str>,
    pub host: &'a str,
    pub port: u64,
    pub username: &'a str,
    pub used: Option<u64>,
    pub total: Option<u64>,
}

/// Canonical protocol class — mirrors `getProtocolClass` from `src/types.ts`.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProtocolClass {
    OAuth,
    Api,
    WebDav,
    E2e,
    S3,
    Azure,
    Ftp,
    Ftps,
    Sftp,
    AeroCloud,
}

impl ProtocolClass {
    pub fn label(self) -> &'static str {
        match self {
            ProtocolClass::OAuth => "OAuth",
            ProtocolClass::Api => "API",
            ProtocolClass::WebDav => "WebDAV",
            ProtocolClass::E2e => "E2E",
            ProtocolClass::S3 => "S3",
            ProtocolClass::Azure => "Azure",
            ProtocolClass::Ftp => "FTP",
            ProtocolClass::Ftps => "FTPS",
            ProtocolClass::Sftp => "SFTP",
            ProtocolClass::AeroCloud => "AeroCloud",
        }
    }
}

pub fn classify(protocol: &str) -> ProtocolClass {
    match protocol {
        "googledrive" | "googlephotos" | "dropbox" | "onedrive" | "box" | "pcloud"
        | "zohoworkdrive" | "yandexdisk" | "fourshared" => ProtocolClass::OAuth,
        "aerocloud" => ProtocolClass::AeroCloud,
        "filen" | "internxt" | "mega" => ProtocolClass::E2e,
        "webdav" => ProtocolClass::WebDav,
        "ftps" => ProtocolClass::Ftps,
        "ftp" => ProtocolClass::Ftp,
        "sftp" => ProtocolClass::Sftp,
        "s3" => ProtocolClass::S3,
        "azure" => ProtocolClass::Azure,
        _ => ProtocolClass::Api,
    }
}

/// Lowercase, trim, strip leading scheme + `www.` and trailing path. Mirrors
/// the TS `normalizeHost` helper.
pub fn normalize_host(raw: &str) -> String {
    let mut s = raw.trim().to_ascii_lowercase();
    for scheme in ["https://", "http://", "webdavs://", "webdav://"] {
        if s.starts_with(scheme) {
            s = s[scheme.len()..].to_string();
            break;
        }
    }
    if let Some(stripped) = s.strip_prefix("www.") {
        s = stripped.to_string();
    }
    if let Some(idx) = s.find('/') {
        s.truncate(idx);
    }
    while s.ends_with('/') {
        s.pop();
    }
    s
}

/// Lowercase + trim. Returns `None` for opaque tokens (long random strings)
/// where dedup would risk false positives. Mirrors `normalizeUser` (TS) and
/// `looksLikeOpaqueToken` from `src/utils/serverSubtitle.ts`.
pub fn normalize_user(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if looks_like_opaque_token(trimmed) {
        return None;
    }
    Some(trimmed.to_ascii_lowercase())
}

fn looks_like_opaque_token(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.len() > 40 && !s.contains('@') && !s.contains(' ') {
        return true;
    }
    if s.len() >= 32 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    if s.len() >= 36
        && !s.contains('@')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return true;
    }
    false
}

/// SHA-256 of the access key, hex-truncated to 12 chars. Used to dedup S3
/// profiles without leaking the key into the dedup string.
pub fn access_key_hash(access_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(access_key.trim().as_bytes());
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>();
    hex[..12].to_string()
}

/// Identify cloud-preset WebDAV providers (Koofr, InfiniCloud, Nextcloud, ...)
/// vs custom self-hosted WebDAV servers. The list mirrors `deriveProviderId`
/// and the registry presets recognised by the GUI.
pub fn is_webdav_preset(provider_id: Option<&str>) -> bool {
    matches!(
        provider_id,
        Some(
            "koofr-webdav"
                | "opendrive-webdav"
                | "yandex-storage-webdav"
                | "infinicloud"
                | "nextcloud"
                | "seafile"
                | "cloudme"
                | "drivehq"
                | "jianguoyun"
                | "filelu-webdav"
                | "felicloud-webdav"
        )
    )
}

/// Identify S3-preset providers (Backblaze, Wasabi, Cloudflare R2, ...) vs raw
/// `amazon-s3`/custom endpoints.
pub fn is_s3_preset(provider_id: Option<&str>) -> bool {
    matches!(
        provider_id,
        Some(
            "backblaze"
                | "wasabi"
                | "cloudflare-r2"
                | "idrive-e2"
                | "storj"
                | "mega-s4"
                | "digitalocean-spaces"
                | "alibaba-oss"
                | "tencent-cos"
                | "oracle-cloud"
                | "yandex-storage"
                | "filelu-s3"
        )
    )
}

/// Cross-protocol provider families. Koofr exposes WebDAV + REST that map to
/// the same physical disk; OpenDrive does the same; Yandex Disk has WebDAV +
/// OAuth + Object Storage. When a profile belongs to a known family AND the
/// username is dedup-able, the key uses the `family:` prefix so all surfaces
/// of the same account collapse into one drive.
fn dedup_family(provider_id: Option<&str>, protocol: &str) -> Option<&'static str> {
    match (provider_id, protocol) {
        (Some("koofr-webdav") | Some("koofr"), _) => Some("koofr"),
        (_, "koofr") => Some("koofr"),
        (Some("opendrive-webdav") | Some("opendrive"), _) => Some("opendrive"),
        (_, "opendrive") => Some("opendrive"),
        (Some("yandex-storage-webdav") | Some("yandex-storage") | Some("yandexdisk"), _) => {
            Some("yandex")
        }
        (_, "yandexdisk") => Some("yandex"),
        (Some("filelu-webdav") | Some("filelu"), _) => Some("filelu"),
        (_, "filelu") => Some("filelu"),
        // FileLu S3 deliberately omitted: its access-key-based dedup cannot
        // reconcile with username-based WebDAV/REST without leaking the key.
        _ => None,
    }
}

const OAUTH_PROTOCOLS: &[&str] = &[
    "googledrive",
    "googlephotos",
    "dropbox",
    "onedrive",
    "box",
    "pcloud",
    "zohoworkdrive",
    "yandexdisk",
    "fourshared",
];

const NATIVE_API_PROTOCOLS: &[&str] = &[
    "mega",
    "filen",
    "internxt",
    "kdrive",
    "drime",
    "filelu",
    "koofr",
    "opendrive",
    "jottacloud",
    "github",
    "gitlab",
];

/// Compute the canonical dedup key for `profile`. See the Phase 4 handoff for
/// the per-category formula. The fallback is `id:<profileId>` so distinct
/// profiles never collapse by mistake.
pub fn dedup_key(profile: &ProfileView<'_>) -> String {
    let proto = profile.protocol;
    let user_norm = normalize_user(profile.username);
    let user_or_id = || -> String {
        match &user_norm {
            Some(u) => u.clone(),
            None => format!("id:{}", profile.id),
        }
    };

    // Family-based dedup wins over per-protocol prefixes when both the family
    // and a usable username are available — that's the path that collapses
    // Koofr WebDAV + REST (or OpenDrive WebDAV + REST) to a single drive.
    if let (Some(family), Some(user)) = (dedup_family(profile.provider_id, proto), &user_norm) {
        return format!("family:{}:{}", family, user);
    }

    if OAUTH_PROTOCOLS.contains(&proto) {
        let pid = profile.provider_id.unwrap_or(proto);
        return format!("oauth:{}:{}", pid, user_or_id());
    }

    if proto == "webdav" {
        if is_webdav_preset(profile.provider_id) {
            let pid = profile.provider_id.unwrap_or(proto);
            return format!("webdav:{}:{}", pid, user_or_id());
        }
        let host = normalize_host(profile.host);
        return format!("webdav-host:{}:{}", host, user_or_id());
    }

    if proto == "s3" {
        let access_hash = access_key_hash(profile.username);
        if is_s3_preset(profile.provider_id) {
            let pid = profile.provider_id.unwrap_or(proto);
            return format!("s3:{}:{}", pid, access_hash);
        }
        let host = normalize_host(profile.host);
        return format!("s3-host:{}:{}", host, access_hash);
    }

    if proto == "azure" {
        // `username` carries the storage account name in CLI/GUI flows.
        return format!("azure:{}", user_or_id());
    }

    if proto == "aerocloud" {
        // AeroCloud profiles always represent a single logical drive per id.
        return format!("aerocloud:{}", profile.id);
    }

    if NATIVE_API_PROTOCOLS.contains(&proto) {
        let pid = profile.provider_id.unwrap_or(proto);
        return format!("api:{}:{}", pid, user_or_id());
    }

    if matches!(proto, "ftp" | "ftps" | "sftp") {
        let host = normalize_host(profile.host);
        let port = if profile.port == 0 {
            default_port(proto)
        } else {
            profile.port
        };
        return format!(
            "host:{}:{}:{}:{}",
            proto,
            host,
            port,
            user_or_id()
        );
    }

    format!("id:{}", profile.id)
}

fn default_port(proto: &str) -> u64 {
    match proto {
        "ftp" | "ftps" => 21,
        "sftp" => 22,
        "webdav" => 443,
        _ => 0,
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ProtocolBreakdownRow {
    pub protocol_class: String,
    pub profiles: usize,
    pub unique: usize,
    pub used: u128,
    pub total: u128,
    pub quota_count: usize,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AggregateSummary {
    pub profiles: usize,
    pub unique_count: usize,
    pub total_used: u128,
    pub total_total: u128,
    pub deduped_quota_count: usize,
    pub by_protocol_class: Vec<ProtocolBreakdownRow>,
}

/// Aggregate a list of profiles. Quotes are summed once per `dedup_key`; when
/// two profiles in the same group disagree on `used` or `total`, the maximum
/// is taken (conservative — never undercounts).
pub fn aggregate(profiles: &[ProfileView<'_>]) -> AggregateSummary {
    use std::collections::HashMap;

    #[derive(Default)]
    struct Bucket {
        used: Option<u64>,
        total: Option<u64>,
        protocol_classes: BTreeMap<ProtocolClass, ()>,
    }

    let mut buckets: HashMap<String, Bucket> = HashMap::new();
    let mut profile_class_buckets: BTreeMap<ProtocolClass, BTreeMap<String, ()>> =
        BTreeMap::new();
    let mut profile_class_counts: BTreeMap<ProtocolClass, usize> = BTreeMap::new();

    for p in profiles {
        let key = dedup_key(p);
        let class = classify(p.protocol);
        *profile_class_counts.entry(class).or_default() += 1;
        profile_class_buckets
            .entry(class)
            .or_default()
            .insert(key.clone(), ());

        let entry = buckets.entry(key).or_default();
        entry.protocol_classes.insert(class, ());
        if let (Some(u), Some(t)) = (p.used, p.total) {
            if t > 0 {
                entry.used = Some(entry.used.unwrap_or(0).max(u));
                entry.total = Some(entry.total.unwrap_or(0).max(t));
            }
        }
    }

    let mut total_used: u128 = 0;
    let mut total_total: u128 = 0;
    let mut deduped_quota_count: usize = 0;
    for bucket in buckets.values() {
        if let (Some(u), Some(t)) = (bucket.used, bucket.total) {
            if t > 0 {
                total_used += u as u128;
                total_total += t as u128;
                deduped_quota_count += 1;
            }
        }
    }

    // Per-protocol breakdown. Each profile contributes to the class derived
    // from its own protocol; quotas in each class come from the deduped
    // buckets that include that class. A bucket spanning multiple classes
    // (rare — only happens for cross-class dedup like Koofr) contributes its
    // single (used, total) pair to **each** class it touches, mirroring how
    // the TS aggregator surfaces the same drive in every protocol it served.
    let mut breakdown: Vec<ProtocolBreakdownRow> = Vec::new();
    for (class, profile_count) in profile_class_counts.iter() {
        let bucket_keys = profile_class_buckets.get(class).cloned().unwrap_or_default();
        let unique = bucket_keys.len();
        let mut class_used: u128 = 0;
        let mut class_total: u128 = 0;
        let mut class_quota_count: usize = 0;
        for key in bucket_keys.keys() {
            let Some(bucket) = buckets.get(key) else {
                continue;
            };
            if let (Some(u), Some(t)) = (bucket.used, bucket.total) {
                if t > 0 {
                    class_used += u as u128;
                    class_total += t as u128;
                    class_quota_count += 1;
                }
            }
        }
        breakdown.push(ProtocolBreakdownRow {
            protocol_class: class.label().to_string(),
            profiles: *profile_count,
            unique,
            used: class_used,
            total: class_total,
            quota_count: class_quota_count,
        });
    }

    // Sort: classes with quota first (Total desc), then alphabetically.
    breakdown.sort_by(|a, b| match (a.total, b.total) {
        (0, 0) => a.protocol_class.cmp(&b.protocol_class),
        (0, _) => std::cmp::Ordering::Greater,
        (_, 0) => std::cmp::Ordering::Less,
        (x, y) => y.cmp(&x),
    });

    AggregateSummary {
        profiles: profiles.len(),
        unique_count: buckets.len(),
        total_used,
        total_total,
        deduped_quota_count,
        by_protocol_class: breakdown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn p<'a>(
        id: &'a str,
        protocol: &'a str,
        provider_id: Option<&'a str>,
        host: &'a str,
        port: u64,
        username: &'a str,
        used: Option<u64>,
        total: Option<u64>,
    ) -> ProfileView<'a> {
        ProfileView {
            id,
            protocol,
            provider_id,
            host,
            port,
            username,
            used,
            total,
        }
    }

    #[test]
    fn case1_koofr_webdav_and_rest_collapse_to_one_drive() {
        // Two Koofr profiles backed by the same account but exposed via WebDAV
        // and the native REST API. The provider family lookup keys them on
        // `family:koofr:<user>`, so the deduped quota counts only once.
        let profiles = [
            p(
                "k1",
                "webdav",
                Some("koofr-webdav"),
                "https://app.koofr.net/dav/Koofr",
                443,
                "user@example.com",
                Some(2_000_000_000),
                Some(10_000_000_000),
            ),
            p(
                "k2",
                "koofr",
                Some("koofr"),
                "",
                0,
                "user@example.com",
                Some(2_000_000_000),
                Some(10_000_000_000),
            ),
        ];
        let summary = aggregate(&profiles);
        assert_eq!(summary.profiles, 2);
        assert_eq!(summary.unique_count, 1);
        // The two surfaces of the same disk together with their identical
        // quota readings sum once, not twice.
        assert_eq!(summary.total_used, 2_000_000_000);
        assert_eq!(summary.total_total, 10_000_000_000);
        assert_eq!(summary.deduped_quota_count, 1);
        let key = dedup_key(&profiles[0]);
        assert_eq!(key, "family:koofr:user@example.com");
        assert_eq!(dedup_key(&profiles[0]), dedup_key(&profiles[1]));
    }

    #[test]
    fn case2_filen_with_different_users_two_keys() {
        let profiles = [
            p(
                "f1",
                "filen",
                Some("filen"),
                "",
                0,
                "alice@proton.me",
                Some(1_000_000),
                Some(1_000_000_000),
            ),
            p(
                "f2",
                "filen",
                Some("filen"),
                "",
                0,
                "bob@proton.me",
                Some(2_000_000),
                Some(1_000_000_000),
            ),
        ];
        let summary = aggregate(&profiles);
        assert_eq!(summary.profiles, 2);
        assert_eq!(summary.unique_count, 2);
        assert_eq!(summary.total_used, 3_000_000);
        assert_eq!(summary.total_total, 2_000_000_000);
    }

    #[test]
    fn case3_sftp_same_host_different_ports_two_keys() {
        let profiles = [
            p("s1", "sftp", None, "nas.local", 22, "axp", None, None),
            p("s2", "sftp", None, "nas.local", 2222, "axp", None, None),
        ];
        let summary = aggregate(&profiles);
        assert_eq!(summary.profiles, 2);
        assert_eq!(summary.unique_count, 2);
    }

    #[test]
    fn case4_wasabi_same_access_key_dedups_to_one() {
        // Two Wasabi profiles with the same access key (different bucket
        // names captured in `options`, irrelevant for dedup) collapse to one.
        let profiles = [
            p(
                "w1",
                "s3",
                Some("wasabi"),
                "s3.wasabisys.com",
                443,
                "AKIAEXAMPLE12345",
                Some(50_000_000_000),
                Some(1_000_000_000_000),
            ),
            p(
                "w2",
                "s3",
                Some("wasabi"),
                "s3.eu-west-1.wasabisys.com",
                443,
                "AKIAEXAMPLE12345",
                Some(50_000_000_000),
                Some(1_000_000_000_000),
            ),
        ];
        let summary = aggregate(&profiles);
        assert_eq!(summary.profiles, 2);
        assert_eq!(summary.unique_count, 1);
        // Quotas counted once thanks to dedup, not summed twice.
        assert_eq!(summary.total_used, 50_000_000_000);
        assert_eq!(summary.total_total, 1_000_000_000_000);
        assert_eq!(summary.deduped_quota_count, 1);
    }

    #[test]
    fn case5_oauth_with_email_username_keyed_by_email() {
        let profiles = [
            p(
                "o1",
                "googledrive",
                Some("googledrive"),
                "",
                0,
                "user@gmail.com",
                Some(1_000_000),
                Some(15_000_000_000),
            ),
            p(
                "o2",
                "googledrive",
                Some("googledrive"),
                "",
                0,
                "user@gmail.com",
                Some(1_000_000),
                Some(15_000_000_000),
            ),
        ];
        let summary = aggregate(&profiles);
        // Two profiles, same email -> same dedup key.
        assert_eq!(summary.unique_count, 1);
        let key = dedup_key(&profiles[0]);
        assert!(key.starts_with("oauth:googledrive:"));
        assert!(key.contains("user@gmail.com"));
    }

    #[test]
    fn case6_quota_summed_once_per_dedup_key() {
        // Three Wasabi profiles, same access key -> 1 unique drive.
        let profiles: Vec<ProfileView> = (0..3)
            .map(|i| {
                ProfileView {
                    id: ["a", "b", "c"][i],
                    protocol: "s3",
                    provider_id: Some("wasabi"),
                    host: "s3.wasabisys.com",
                    port: 443,
                    username: "AKIA_SAME_KEY_HERE_X",
                    used: Some(1_000_000_000),
                    total: Some(10_000_000_000),
                }
            })
            .collect();
        let summary = aggregate(&profiles);
        assert_eq!(summary.profiles, 3);
        assert_eq!(summary.unique_count, 1);
        // 1 GB, NOT 3 GB — the whole point of Phase 4.
        assert_eq!(summary.total_used, 1_000_000_000);
        assert_eq!(summary.total_total, 10_000_000_000);
    }

    #[test]
    fn opaque_token_username_falls_back_to_id_no_false_dedup() {
        // Two Drime profiles with very long opaque tokens as username -> two
        // separate dedup keys (no false dedup).
        let profiles = [
            p(
                "d1",
                "drime",
                Some("drime"),
                "",
                0,
                "thisisaverylongopaquetokenstringwithoutemailorspacesabc",
                Some(0),
                Some(20_000_000_000),
            ),
            p(
                "d2",
                "drime",
                Some("drime"),
                "",
                0,
                "thisisaverylongopaquetokenstringwithoutemailorspacesabc",
                Some(0),
                Some(20_000_000_000),
            ),
        ];
        let summary = aggregate(&profiles);
        assert_eq!(summary.profiles, 2);
        assert_eq!(summary.unique_count, 2); // distinct id:<profileId>
    }

    #[test]
    fn divergent_quotas_use_max_not_sum() {
        // Two duplicate profiles whose quota readings disagree (one was
        // refreshed before the user uploaded a big file). Conservative `max`
        // wins — never sum.
        let profiles = [
            p(
                "p1",
                "s3",
                Some("wasabi"),
                "s3.wasabisys.com",
                443,
                "AKIA_SAME",
                Some(40_000_000_000),
                Some(1_000_000_000_000),
            ),
            p(
                "p2",
                "s3",
                Some("wasabi"),
                "s3.wasabisys.com",
                443,
                "AKIA_SAME",
                Some(50_000_000_000),
                Some(1_000_000_000_000),
            ),
        ];
        let summary = aggregate(&profiles);
        assert_eq!(summary.unique_count, 1);
        assert_eq!(summary.total_used, 50_000_000_000);
    }

    #[test]
    fn normalize_host_strips_scheme_path_and_www() {
        assert_eq!(normalize_host("https://Www.Example.com/dav/"), "example.com");
        assert_eq!(normalize_host("webdavs://nas.local:8080/share"), "nas.local:8080");
    }

    #[test]
    fn classify_covers_all_provider_categories() {
        assert_eq!(classify("googledrive"), ProtocolClass::OAuth);
        assert_eq!(classify("filen"), ProtocolClass::E2e);
        assert_eq!(classify("webdav"), ProtocolClass::WebDav);
        assert_eq!(classify("s3"), ProtocolClass::S3);
        assert_eq!(classify("azure"), ProtocolClass::Azure);
        assert_eq!(classify("ftp"), ProtocolClass::Ftp);
        assert_eq!(classify("ftps"), ProtocolClass::Ftps);
        assert_eq!(classify("sftp"), ProtocolClass::Sftp);
        assert_eq!(classify("aerocloud"), ProtocolClass::AeroCloud);
        assert_eq!(classify("koofr"), ProtocolClass::Api);
        assert_eq!(classify("github"), ProtocolClass::Api);
    }
}
