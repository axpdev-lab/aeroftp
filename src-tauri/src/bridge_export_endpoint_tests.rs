// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Cross-exporter tests for the address every profile-bridge exporter writes.
//!
//! The profiles below have the three shapes a saved profile really takes, and
//! that each exporter used to misread by treating `host` as a bare host name:
//! a MinIO profile whose `host` is empty and whose endpoint lives in
//! `options.endpoint`, a Cloudflare R2 profile whose `host` is a full URL, and
//! a WebDAV profile whose `host` is a URL with a path. The FTP profiles store
//! their TLS mode as `options.tlsMode`, the key the GUI writes. Each test
//! reads the file an exporter wrote and checks the address a tool would dial.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::bridge_shared::{resolve_export_endpoint, BridgeExportOutcome};

fn minio() -> Value {
    json!({
        "name": "lab minio", "host": "", "port": 443, "username": "AKLAB",
        "protocol": "s3", "providerId": "minio", "initialPath": "/",
        "options": { "endpoint": "https://s3.lab.example.com", "bucket": "tests", "region": "us-east-1" }
    })
}

fn r2() -> Value {
    json!({
        "name": "r2", "host": "https://acct.r2.cloudflarestorage.com/", "port": 443,
        "username": "AKR2", "protocol": "s3", "providerId": "cloudflare-r2",
        "options": { "bucket": "media", "region": "auto" }
    })
}

fn webdav() -> Value {
    json!({
        "name": "cloud dav", "host": "https://dav.example.com/remote.php/dav/files/u/",
        "port": 443, "username": "u", "protocol": "webdav", "initialPath": "/"
    })
}

fn ftp(name: &str, protocol: &str, port: u32, tls_mode: Option<&str>) -> Value {
    let mut v = json!({
        "name": name, "host": "ftp.example.com", "port": port, "username": "ftpuser",
        "protocol": protocol
    });
    if let Some(m) = tls_mode {
        v["options"] = json!({ "tlsMode": m });
    }
    v
}

fn typed<T: serde::de::DeserializeOwned>(profiles: &[Value]) -> Vec<T> {
    profiles
        .iter()
        .map(|p| serde_json::from_value(p.clone()).expect("export server shape"))
        .collect()
}

fn passwords(profiles: &[Value]) -> HashMap<String, String> {
    profiles
        .iter()
        .map(|p| (p["name"].as_str().unwrap().to_string(), "pw".to_string()))
        .collect()
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aeroftp-bridge-ep-{}-{name}",
        crate::bridge_shared::uuid_v4()
    ))
}

fn read(p: &PathBuf) -> String {
    let s = std::fs::read_to_string(p).expect("exported file");
    std::fs::remove_file(p).ok();
    s
}

fn no_skips(o: &BridgeExportOutcome) {
    assert!(o.skipped.is_empty(), "unexpected skips: {:?}", o.skipped);
}

// ---------------------------------------------------------------- resolver

#[test]
fn resolver_reads_the_endpoint_where_each_profile_keeps_it() {
    let ep = |v: Value| {
        resolve_export_endpoint(
            v["protocol"].as_str().unwrap(),
            v["host"].as_str().unwrap(),
            v["port"].as_u64().unwrap() as u32,
            v["username"].as_str().unwrap(),
            v.get("options"),
            v.get("providerId").and_then(|p| p.as_str()),
        )
        .expect("resolves")
        .expect("has an endpoint")
    };

    let m = ep(minio());
    assert_eq!(
        (m.scheme, m.host.as_str(), m.port),
        ("https", "s3.lab.example.com", None)
    );
    assert!(m.s3_path_style, "MinIO answers only path-style");

    let r = ep(r2());
    assert_eq!(r.host, "acct.r2.cloudflarestorage.com");
    assert_eq!(r.path, "");

    let d = ep(webdav());
    assert_eq!(d.url(), "https://dav.example.com/remote.php/dav/files/u");

    // A cleartext MinIO on 9000 keeps scheme and port.
    let mut plain = minio();
    plain["options"]["endpoint"] = json!("http://nas.local:9000");
    let p = ep(plain);
    assert_eq!(p.base_url(), "http://nas.local:9000");

    // AWS: no endpoint of its own.
    let aws = resolve_export_endpoint(
        "s3",
        "",
        443,
        "AK",
        Some(&json!({"bucket": "b"})),
        Some("amazon-s3"),
    )
    .expect("resolves");
    assert!(aws.is_none());
}

// ---------------------------------------------------------------- cyberduck

#[test]
fn cyberduck_bookmarks_carry_a_bare_host_and_path_style() {
    let profiles = [minio(), r2(), webdav()];
    let dir = tmp("duck");
    let out = crate::cyberduck_import::export_cyberduck(&typed(&profiles), &HashMap::new(), &dir)
        .expect("export");
    no_skips(&out);
    assert_eq!(out.exported, 3);

    let minio_duck = read(&dir.join("lab-minio.duck"));
    assert!(
        minio_duck.contains("<string>s3.lab.example.com</string>"),
        "{minio_duck}"
    );
    assert!(
        minio_duck.contains("<key>s3.bucket.virtualhost.disable</key>"),
        "{minio_duck}"
    );

    let r2_duck = read(&dir.join("r2.duck"));
    assert!(
        r2_duck.contains("<string>acct.r2.cloudflarestorage.com</string>"),
        "{r2_duck}"
    );
    assert!(!r2_duck.contains("https://"), "{r2_duck}");

    let dav_duck = read(&dir.join("cloud-dav.duck"));
    assert!(
        dav_duck.contains("<string>dav.example.com</string>"),
        "{dav_duck}"
    );
    assert!(
        dav_duck.contains("<string>/remote.php/dav/files/u/</string>"),
        "{dav_duck}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cyberduck_round_trip_keeps_port_and_path_style() {
    let mut p = minio();
    p["options"]["endpoint"] = json!("https://s3.lab.example.com:8443");
    let dir = tmp("duck-rt");
    crate::cyberduck_import::export_cyberduck(&typed(&[p]), &HashMap::new(), &dir).expect("export");
    let back = crate::cyberduck_import::import_cyberduck(&dir).expect("import");
    std::fs::remove_dir_all(&dir).ok();
    let s = &back.servers[0];
    assert_eq!(s.host, "s3.lab.example.com");
    assert_eq!(s.port, 8443);
    assert_eq!(s.options.as_ref().unwrap()["pathStyle"], json!(true));
}

// ---------------------------------------------------------------- mc

#[test]
fn mc_alias_url_is_one_scheme_and_the_real_endpoint() {
    let profiles = [minio(), r2()];
    let f = tmp("mc.json");
    let out =
        crate::mc_import::export_mc(&typed(&profiles), &passwords(&profiles), &f).expect("export");
    no_skips(&out);
    let doc: Value = serde_json::from_str(&read(&f)).unwrap();
    assert_eq!(
        doc["aliases"]["lab minio"]["url"],
        "https://s3.lab.example.com"
    );
    assert_eq!(doc["aliases"]["lab minio"]["path"], "on");
    assert_eq!(
        doc["aliases"]["r2"]["url"],
        "https://acct.r2.cloudflarestorage.com"
    );
}

// ---------------------------------------------------------------- aws

#[test]
fn aws_config_has_the_minio_endpoint_and_path_addressing() {
    let profiles = [minio()];
    let dir = tmp("aws");
    std::fs::create_dir_all(&dir).unwrap();
    let cred = dir.join("credentials");
    crate::aws_credentials_import::export_aws_credentials(
        &typed(&profiles),
        &passwords(&profiles),
        &cred,
    )
    .expect("export");
    let config = read(&dir.join("config"));
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        config.contains("endpoint_url = https://s3.lab.example.com\n"),
        "{config}"
    );
    assert!(config.contains("addressing_style = path"), "{config}");
}

// ---------------------------------------------------------------- s3cmd

#[test]
fn s3cmd_host_base_is_bare_and_other_profiles_are_reported() {
    let profiles = [minio(), r2()];
    let f = tmp("s3cfg");
    let out = crate::s3cmd_import::export_s3cmd(&typed(&profiles), &passwords(&profiles), &f)
        .expect("export");
    let body = read(&f);
    assert!(body.contains("host_base = s3.lab.example.com\n"), "{body}");
    assert!(
        body.contains("host_bucket = s3.lab.example.com\n"),
        "{body}"
    );
    assert_eq!(out.exported, 1);
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].name, "r2");
}

// ---------------------------------------------------------------- kopia

#[test]
fn kopia_endpoint_has_no_scheme_and_webdav_keeps_its_path() {
    let f = tmp("kopia-s3");
    let out = crate::kopia_import::export_kopia(&typed(&[r2(), minio()]), &HashMap::new(), &f)
        .expect("export");
    let doc: Value = serde_json::from_str(&read(&f)).unwrap();
    assert_eq!(
        doc["storage"]["config"]["endpoint"],
        "acct.r2.cloudflarestorage.com"
    );
    assert_eq!(
        out.skipped.len(),
        1,
        "the second profile must be reported, not dropped"
    );

    let f = tmp("kopia-dav");
    crate::kopia_import::export_kopia(&typed(&[webdav()]), &HashMap::new(), &f).expect("export");
    let doc: Value = serde_json::from_str(&read(&f)).unwrap();
    assert_eq!(
        doc["storage"]["config"]["url"],
        "https://dav.example.com/remote.php/dav/files/u"
    );
}

// ---------------------------------------------------------------- duplicacy

#[test]
fn duplicacy_storage_urls_are_duplicacy_forms() {
    let profiles = [minio(), r2(), webdav()];
    let f = tmp("duplicacy");
    let out =
        crate::duplicacy_import::export_duplicacy(&typed(&profiles), &passwords(&profiles), &f)
            .expect("export");
    no_skips(&out);
    let doc: Vec<Value> = serde_json::from_str(&read(&f)).unwrap();
    let storage: Vec<&str> = doc.iter().map(|e| e["storage"].as_str().unwrap()).collect();
    assert_eq!(storage[0], "minios://us-east-1@s3.lab.example.com/tests");
    // The R2 preset connects path-style, so Duplicacy gets its path-style form.
    assert_eq!(
        storage[1],
        "minios://auto@acct.r2.cloudflarestorage.com/media"
    );
    assert_eq!(
        storage[2],
        "webdav://u@dav.example.com/remote.php/dav/files/u"
    );
    let names: Vec<&str> = doc.iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(names[0], "default");
    assert_eq!(
        names.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "{names:?}"
    );
}

#[test]
fn duplicacy_import_reads_region_scheme_and_webdav_path() {
    let fixture = r#"[
      { "name":"a", "id":"x", "repository":"", "encrypted":true,
        "storage":"minio://us-east-1@nas.local:9000/tests/sub", "keys":{ "s3_id":"A", "s3_secret":"S" } },
      { "name":"b", "id":"y", "repository":"", "encrypted":true,
        "storage":"webdav-http://u@nas.local:8080/dav/backup", "keys":{ "password":"p" } }
    ]"#;
    let f = tmp("dup-in");
    std::fs::write(&f, fixture).unwrap();
    let r = crate::duplicacy_import::import_duplicacy_with_env(&f, &|_| None).expect("import");
    std::fs::remove_file(&f).ok();
    let s3 = &r.servers[0];
    assert_eq!(s3.host, "nas.local:9000");
    let o = s3.options.as_ref().unwrap();
    assert_eq!(o["endpoint"], "http://nas.local:9000");
    assert_eq!(o["pathStyle"], "true");
    assert_eq!(o["region"], "us-east-1");
    assert_eq!(o["bucket"], "tests");
    let dav = &r.servers[1];
    assert_eq!(dav.host, "http://nas.local:8080/dav/backup");
    assert_eq!(dav.username, "u");
}

// ---------------------------------------------------------------- restic

#[test]
fn restic_s3_repository_uses_the_endpoint_and_webdav_is_refused() {
    let f = tmp("restic");
    let out = crate::restic_import::export_restic(
        &typed(&[webdav(), minio(), r2()]),
        &passwords(&[minio()]),
        &f,
    )
    .expect("export");
    let body = read(&f);
    assert!(
        body.contains("RESTIC_REPOSITORY='s3:https://s3.lab.example.com/tests'"),
        "{body}"
    );
    assert_eq!(out.exported, 1);
    let reasons: Vec<&str> = out.skipped.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(reasons, ["cloud dav", "r2"]);
    assert!(out.skipped[0].reason.contains("no WebDAV backend"));
}

// ---------------------------------------------------------------- lftp

#[test]
fn lftp_webdav_url_is_not_nested_and_ftps_follows_the_tls_mode() {
    let profiles = [
        webdav(),
        ftp("implicit", "ftps", 990, Some("implicit")),
        ftp("explicit", "ftps", 21, Some("explicit")),
    ];
    let f = tmp("lftp");
    let out = crate::lftp_import::export_lftp(&typed(&profiles), &passwords(&profiles), &f)
        .expect("export");
    no_skips(&out);
    let body = read(&f);
    assert!(
        body.contains("cloud_dav https://u:pw@dav.example.com/remote.php/dav/files/u/\n"),
        "{body}"
    );
    assert!(
        body.contains("implicit ftps://ftpuser:pw@ftp.example.com\n"),
        "{body}"
    );
    // lftp's ftps:// is implicit TLS: an explicit-TLS server is an ftp:// entry.
    assert!(
        body.contains("explicit ftp://ftpuser:pw@ftp.example.com\n"),
        "{body}"
    );
}

// ---------------------------------------------------------------- FTP TLS mode

#[test]
fn winscp_and_filezilla_read_the_tls_mode_the_gui_stores() {
    let profiles = [
        ftp("implicit", "ftps", 990, Some("implicit")),
        ftp("gui-explicit", "ftp", 21, Some("explicit")),
        ftp("ftps-default", "ftps", 990, None),
    ];

    let f = tmp("winscp.ini");
    crate::winscp_import::export_winscp(&typed(&profiles), &HashMap::new(), &f).expect("export");
    let ini = read(&f);
    let ftps_of = |session: &str| {
        let i = ini.find(&format!("[Sessions\\{session}]")).expect(session);
        let tail = &ini[i..];
        let line = tail.lines().find(|l| l.starts_with("Ftps=")).unwrap();
        line.trim_start_matches("Ftps=").to_string()
    };
    assert_eq!(ftps_of("implicit"), "1");
    // An FTP profile the GUI connects with explicit TLS must not become cleartext.
    assert_eq!(ftps_of("gui-explicit"), "3");
    // Same default as the connection: an ftps profile without a mode is implicit.
    assert_eq!(ftps_of("ftps-default"), "1");

    let f = tmp("sitemanager.xml");
    crate::filezilla_import::export_filezilla(&typed(&profiles), &HashMap::new(), &f)
        .expect("export");
    let xml = read(&f);
    let protos: Vec<&str> = xml
        .lines()
        .filter_map(|l| l.trim().strip_prefix("<Protocol>"))
        .map(|l| l.trim_end_matches("</Protocol>"))
        .collect();
    assert_eq!(protos, ["3", "4", "3"]);
}

#[test]
fn mobaxterm_refuses_ftps_instead_of_writing_a_plain_ftp_bookmark() {
    let profiles = [
        ftp("secure", "ftps", 21, Some("explicit")),
        ftp("gui-explicit", "ftp", 21, Some("explicit")),
        ftp("plain", "ftp", 21, Some("none")),
    ];
    let f = tmp("moba.ini");
    let out = crate::mobaxterm_import::export_mobaxterm(&typed(&profiles), &HashMap::new(), &f)
        .expect("export");
    let ini = read(&f);
    assert_eq!(out.exported, 1);
    let skipped: Vec<&str> = out.skipped.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(skipped, ["secure", "gui-explicit"]);
    assert!(!ini.contains("secure="), "{ini}");
}

#[test]
fn dreamweaver_reports_the_sites_it_had_no_room_for() {
    let profiles = [webdav(), ftp("plain", "ftp", 21, None)];
    let f = tmp("site.ste");
    let out =
        crate::dreamweaver_import::export_dreamweaver(&typed(&profiles), &passwords(&profiles), &f)
            .expect("export");
    let xml = read(&f);
    assert!(
        xml.contains("host=\"https://dav.example.com/remote.php/dav/files/u\""),
        "{xml}"
    );
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].name, "plain");
}

#[test]
fn cyberduck_ftp_protocol_follows_the_tls_mode() {
    let profiles = [
        ftp("gui-explicit", "ftp", 21, Some("explicit")),
        ftp("implicit", "ftps", 990, Some("implicit")),
        ftp("plain", "ftp", 21, Some("none")),
    ];
    let dir = tmp("duck-ftp");
    let out = crate::cyberduck_import::export_cyberduck(&typed(&profiles), &HashMap::new(), &dir)
        .expect("export");
    assert_eq!(out.exported, 2);
    assert_eq!(out.skipped.len(), 1);
    assert_eq!(out.skipped[0].name, "implicit");
    let explicit = read(&dir.join("gui-explicit.duck"));
    assert!(explicit.contains("<string>ftps</string>"), "{explicit}");
    let plain = read(&dir.join("plain.duck"));
    assert!(plain.contains("<string>ftp</string>"), "{plain}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dreamweaver_marks_an_ftp_profile_with_tls_as_ssl() {
    let profiles = [ftp("gui-explicit", "ftp", 21, Some("explicit"))];
    let f = tmp("tls.ste");
    crate::dreamweaver_import::export_dreamweaver(&typed(&profiles), &passwords(&profiles), &f)
        .expect("export");
    let xml = read(&f);
    assert!(xml.contains("useSSL=\"TRUE\""), "{xml}");
}

#[test]
fn cyberduck_ftps_bookmark_imports_as_explicit_tls() {
    let dir = tmp("duck-ftps-in");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("f.duck"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>Protocol</key><string>ftps</string>
<key>Nickname</key><string>f</string>
<key>Hostname</key><string>ftp.example.com</string>
<key>Username</key><string>u</string>
</dict></plist>"#,
    )
    .unwrap();
    let r = crate::cyberduck_import::import_cyberduck(&dir).expect("import");
    std::fs::remove_dir_all(&dir).ok();
    let s = &r.servers[0];
    // Cyberduck's `ftps` is explicit AUTH TLS on 21, not AeroFTP's implicit default.
    assert_eq!(s.port, 21);
    assert_eq!(s.options.as_ref().unwrap()["tlsMode"], "explicit");
}

#[test]
fn legacy_ftps_mode_key_reaches_the_connection() {
    let mut extra = HashMap::new();
    crate::profile_loader::apply_profile_options(
        &mut extra,
        &json!({ "options": { "ftpsMode": "explicit" } }),
    );
    assert_eq!(extra.get("tls_mode").map(String::as_str), Some("explicit"));
}

#[test]
fn importers_leave_aws_without_an_explicit_endpoint() {
    // An explicit endpoint turns path-style on in the S3 connection; AWS
    // refuses path-style for newer buckets, so an AWS alias must not get one.
    let f = tmp("mc-aws.json");
    std::fs::write(
        &f,
        r#"{"version":"10","aliases":{"s3":{"url":"https://s3.amazonaws.com","accessKey":"A","secretKey":"S","api":"S3v4","path":"auto"},
            "nas":{"url":"http://nas.local:8080","accessKey":"A","secretKey":"S","api":"S3v4","path":"on"}}}"#,
    )
    .unwrap();
    let r = crate::mc_import::import_mc(&f).expect("import");
    std::fs::remove_file(&f).ok();
    let by = |n: &str| {
        r.servers
            .iter()
            .find(|s| s.name == n)
            .unwrap()
            .options
            .clone()
            .unwrap()
    };
    assert!(by("s3").get("endpoint").is_none());
    assert_eq!(by("nas")["endpoint"], "http://nas.local:8080");
    assert_eq!(by("nas")["pathStyle"], true);
}

#[test]
fn kopia_and_duplicacy_imports_leave_aws_implicit() {
    let f = tmp("kopia-aws");
    std::fs::write(
        &f,
        r#"{"storage":{"type":"s3","config":{"bucket":"b","endpoint":"s3.amazonaws.com","accessKeyID":"A","secretAccessKey":"S"}}}"#,
    )
    .unwrap();
    let r = crate::kopia_import::import_kopia(&f).expect("import");
    std::fs::remove_file(&f).ok();
    assert!(r.servers[0]
        .options
        .as_ref()
        .unwrap()
        .get("endpoint")
        .is_none());

    let f = tmp("dup-aws");
    std::fs::write(
        &f,
        r#"[{"name":"a","id":"x","repository":"","encrypted":true,"storage":"s3://us-east-1@s3.amazonaws.com/b","keys":{"s3_id":"A","s3_secret":"S"}}]"#,
    )
    .unwrap();
    let r = crate::duplicacy_import::import_duplicacy_with_env(&f, &|_| None).expect("import");
    std::fs::remove_file(&f).ok();
    assert!(r.servers[0]
        .options
        .as_ref()
        .unwrap()
        .get("endpoint")
        .is_none());
}
