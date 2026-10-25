// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Nextcloud trash purge. The fixture answers a trashbin PROPFIND with the
//! properties the request actually asked for, plus a 404 propstat that carries
//! a sentinel file id and original location. A parser that ignores `d:status`
//! purges the wrong item. A plain WebDAV server is a different origin shape
//! and must receive no request.

use std::sync::{Arc, Mutex};

use super::*;

#[derive(Clone, Debug)]
struct DavHit {
    method: String,
    path: String,
    depth: String,
    body: String,
}

fn asked(body: &str, needle: &str) -> bool {
    body.to_ascii_lowercase().contains(needle)
}

fn xml_prop(request: &str, needle: &str, element: &str, value: &str) -> String {
    if !asked(request, needle) {
        return String::new();
    }
    format!("<{element}>{value}</{element}>")
}

fn trash_item(
    body: &str,
    href: &str,
    filename: &str,
    location: &str,
    deleted: &str,
    file_id: &str,
    dir: bool,
) -> String {
    let kind = if dir {
        "<d:resourcetype><d:collection/></d:resourcetype>"
    } else if asked(body, "resourcetype") {
        "<d:resourcetype/>"
    } else {
        ""
    };
    let found = format!(
        "{}{}{}{}{}{}",
        xml_prop(body, "trashbin-filename", "nc:trashbin-filename", filename),
        xml_prop(
            body,
            "trashbin-original-location",
            "nc:trashbin-original-location",
            location
        ),
        xml_prop(
            body,
            "trashbin-deletion-time",
            "nc:trashbin-deletion-time",
            deleted
        ),
        xml_prop(body, "fileid", "oc:fileid", file_id),
        xml_prop(body, "getcontentlength", "d:getcontentlength", "4"),
        kind,
    );
    // Sentinel values live only in the 404 propstat. Production leaves that
    // propstat empty; the sentinel makes a status-blind parser fail the test.
    let missing = if asked(body, "fileid") || asked(body, "trashbin-original-location") {
        format!(
            "<d:propstat><d:prop>{}{}</d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>",
            xml_prop(body, "fileid", "oc:fileid", "999"),
            xml_prop(body, "trashbin-original-location", "nc:trashbin-original-location", "docs/not-this"),
        )
    } else {
        String::new()
    };
    format!(
        "<d:response><d:href>{href}</d:href>\
         {missing}\
         <d:propstat><d:prop>{found}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>\
         </d:response>"
    )
}

fn trash_xml(body: &str) -> String {
    let root = "<d:response><d:href>/remote.php/dav/trashbin/user/trash/</d:href>\
        <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>\
        <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>";
    let items = [
        trash_item(body, "/remote.php/dav/trashbin/user/trash/a.txt.d1700000001", "a.txt", "docs/a.txt", "1700000001", "42", false),
        trash_item(body, "/remote.php/dav/trashbin/user/trash/a.txt.d1700000002", "a.txt", "docs/a.txt", "1700000002", "99", false),
        trash_item(
            body,
            "/remote.php/dav/trashbin/user/trash/%CF%84%CE%B5%CE%BB%CE%B9%CE%BA%CF%8C%CF%82.txt.d1700000003",
            "τελικός.txt",
            "docs/τελικός.txt",
            "1700000003",
            "7",
            false,
        ),
        trash_item(body, "/remote.php/dav/trashbin/user/trash/File.txt.d1700000004", "File.txt", "docs/File.txt", "1700000004", "8", false),
        trash_item(body, "/remote.php/dav/trashbin/user/trash/run.d1700000005", "run", "docs/run", "1700000005", "11", true),
        trash_item(
            body,
            "/remote.php/dav/trashbin/user/trash/Stra%C3%9Fe.txt.d1700000006",
            "Straße.txt",
            "docs/Straße.txt",
            "1700000006",
            "15",
            false,
        ),
        trash_item(body, "/remote.php/dav/trashbin/user/trash/other.txt.d1700000007", "other.txt", "docs/other.txt", "1700000007", "12", false),
    ];
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <d:multistatus xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\" xmlns:nc=\"http://nextcloud.org/ns\">\
         {root}{}</d:multistatus>",
        items.join("")
    )
}

fn live_file(body: &str, href: &str, file_id: &str) -> String {
    let id = xml_prop(body, "fileid", "oc:fileid", file_id);
    let decoy = if asked(body, "fileid") {
        "<d:propstat><d:prop><oc:fileid>999</oc:fileid></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>"
    } else {
        ""
    };
    format!(
        "<d:response><d:href>{href}</d:href>\
         {decoy}\
         <d:propstat><d:prop>{id}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>\
         </d:response>"
    )
}

fn live_xml(body: &str, path: &str) -> String {
    let one = |href: &str, id: &str| live_file(body, href, id);
    let inner = if path.contains("a.txt") {
        one("/remote.php/dav/files/user/docs/a.txt", "42")
    } else if path.contains("%CF%84") || path.contains("τελικός") {
        one(
            "/remote.php/dav/files/user/docs/%CF%84%CE%B5%CE%BB%CE%B9%CE%BA%CF%8C%CF%82.txt",
            "7",
        )
    } else {
        format!(
            "<d:response><d:href>/remote.php/dav/files/user/docs/</d:href>\
             <d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop>\
             <d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>{}{}{}",
            one("/remote.php/dav/files/user/docs/a.txt", "42"),
            one(
                "/remote.php/dav/files/user/docs/%CF%84%CE%B5%CE%BB%CE%B9%CE%BA%CF%8C%CF%82.txt",
                "7",
            ),
            one("/remote.php/dav/files/user/docs/run", "11"),
        )
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <d:multistatus xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\">{inner}</d:multistatus>"
    )
}

async fn serve(fail_propfind: bool, stall_propfind: bool) -> (String, Arc<Mutex<Vec<DavHit>>>) {
    let hits = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&hits);
    let app =
        axum::Router::new().fallback(axum::routing::any(move |req: axum::extract::Request| {
            let seen = Arc::clone(&seen);
            async move {
                let method = req.method().as_str().to_string();
                let path = req.uri().path().to_string();
                let depth = req
                    .headers()
                    .get("depth")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024)
                    .await
                    .unwrap_or_default();
                let body = String::from_utf8_lossy(&bytes).to_string();
                seen.lock().unwrap().push(DavHit {
                    method: method.clone(),
                    path: path.clone(),
                    depth,
                    body: body.clone(),
                });
                if method == "PROPFIND" && stall_propfind {
                    std::future::pending::<()>().await;
                }
                if method == "PROPFIND" && fail_propfind {
                    return axum::response::Response::builder()
                        .status(500)
                        .body(axum::body::Body::from("propfind failed"))
                        .unwrap();
                }
                if method == "DELETE" {
                    return axum::response::Response::builder()
                        .status(204)
                        .body(axum::body::Body::empty())
                        .unwrap();
                }
                if method == "PROPFIND" {
                    let xml = if path.contains("/trashbin/") {
                        trash_xml(&body)
                    } else {
                        live_xml(&body, &path)
                    };
                    return axum::response::Response::builder()
                        .status(207)
                        .header("content-type", "application/xml; charset=utf-8")
                        .body(axum::body::Body::from(xml))
                        .unwrap();
                }
                axum::response::Response::builder()
                    .status(405)
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    (format!("http://{addr}"), hits)
}

fn webdav_config(url: &str, provider_id: Option<&str>) -> WebDavConfig {
    WebDavConfig {
        url: url.to_string(),
        username: "user".to_string(),
        password: secrecy::SecretString::from("pass".to_string()),
        initial_path: None,
        provider_id: provider_id.map(str::to_string),
        verify_cert: true,
        anonymous: false,
    }
}

fn nextcloud_at(base: &str) -> WebDavProvider {
    let mut provider = WebDavProvider::new(webdav_config(
        &format!("{base}/remote.php/dav/files/user/"),
        Some("nextcloud"),
    ))
    .unwrap();
    provider.connected = true;
    provider
}

fn plain_at(base: &str) -> WebDavProvider {
    let mut provider = WebDavProvider::new(webdav_config(&format!("{base}/dav/"), None)).unwrap();
    provider.connected = true;
    provider
}

fn koofr_at(base: &str) -> WebDavProvider {
    let mut provider =
        WebDavProvider::new(webdav_config(&format!("{base}/dav/Koofr"), Some("koofr"))).unwrap();
    provider.connected = true;
    provider
}

fn deletes(hits: &[DavHit]) -> Vec<String> {
    hits.iter()
        .filter(|hit| hit.method == "DELETE")
        .map(|hit| hit.path.clone())
        .collect()
}

fn decoded_delete_ids(paths: &[String]) -> Vec<String> {
    paths
        .iter()
        .map(|path| {
            let leaf = path.rsplit('/').find(|part| !part.is_empty()).unwrap_or("");
            percent_encoding::percent_decode_str(leaf)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect()
}

fn assert_not_the_trash_root(paths: &[String]) {
    for path in paths {
        let trimmed = path.trim_end_matches('/');
        assert_ne!(
            trimmed, "/remote.php/dav/trashbin/user/trash",
            "purge must not empty the trashbin: {path}"
        );
    }
}

#[tokio::test]
async fn nextcloud_purge_deletes_the_one_trash_item_for_that_path() {
    let (base, hits) = serve(false, false).await;
    let mut provider = nextcloud_at(&base);
    let purged = provider.delete_permanent("/docs/run/").await;
    let seen = hits.lock().unwrap().clone();
    let gone = deletes(&seen);
    assert!(matches!(purged, Ok(true)), "{purged:?}");
    assert_eq!(
        decoded_delete_ids(&gone),
        vec!["run.d1700000005".to_string()]
    );
    assert_not_the_trash_root(&gone);
    let listing = seen
        .iter()
        .find(|hit| hit.method == "PROPFIND" && hit.path.contains("/trashbin/"))
        .expect("trash PROPFIND");
    assert_eq!(listing.depth, "1");
    assert!(asked(&listing.body, "fileid"));
    assert!(asked(&listing.body, "trashbin-original-location"));
    assert!(asked(&listing.body, "trashbin-filename"));
    assert!(asked(&listing.body, "trashbin-deletion-time"));
}

#[tokio::test]
async fn nextcloud_purge_refuses_two_entries_with_the_same_location() {
    let (base, hits) = serve(false, false).await;
    let mut provider = nextcloud_at(&base);
    let purged = provider.delete_permanent("/docs/a.txt").await;
    let message = purged.expect_err("two generations of one path").to_string();
    assert!(message.contains("Not purged"), "{message}");
    assert!(message.contains('2'), "{message}");
    let gone = deletes(&hits.lock().unwrap());
    assert!(gone.is_empty(), "{gone:?}");
}

#[tokio::test]
async fn nextcloud_purge_with_captured_file_id_deletes_only_that_generation() {
    let (base, hits) = serve(false, false).await;
    let mut provider = nextcloud_at(&base);
    let captured = provider
        .capture_trash_identities(&["/docs/a.txt".to_string()])
        .await;
    assert_eq!(captured.len(), 1);
    let file_id = captured[0].as_ref().expect("capture").as_deref();
    assert_eq!(file_id, Some("42"));
    let purged = provider
        .delete_permanent_with_identity("/docs/a.txt", file_id)
        .await;
    let seen = hits.lock().unwrap().clone();
    let gone = deletes(&seen);
    assert!(matches!(purged, Ok(true)), "{purged:?}");
    assert_eq!(
        decoded_delete_ids(&gone),
        vec!["a.txt.d1700000001".to_string()]
    );
    assert_not_the_trash_root(&gone);
    let live = seen
        .iter()
        .find(|hit| hit.method == "PROPFIND" && hit.path.contains("/files/"))
        .expect("live PROPFIND");
    assert!(
        asked(&live.body, "fileid"),
        "the live request must ask for oc:fileid"
    );
    assert!(!live.path.contains("/trashbin/"));
}

#[tokio::test]
async fn nextcloud_purge_keeps_exact_unicode_and_does_not_casefold() {
    let (base, hits) = serve(false, false).await;
    let mut provider = nextcloud_at(&base);
    assert_eq!(
        provider.delete_permanent("/docs/τελικός.txt").await.ok(),
        Some(true)
    );
    assert_eq!(
        provider.delete_permanent("/docs/file.txt").await.ok(),
        Some(false),
        "File.txt is a different name"
    );
    assert_eq!(
        provider.delete_permanent("/docs/Strasse.txt").await.ok(),
        Some(false),
        "Strasse is not Straße"
    );
    assert_eq!(
        provider.delete_permanent("/docs/Straße.txt").await.ok(),
        Some(true)
    );
    let gone = deletes(&hits.lock().unwrap());
    assert_eq!(
        decoded_delete_ids(&gone),
        vec![
            "τελικός.txt.d1700000003".to_string(),
            "Straße.txt.d1700000006".to_string(),
        ]
    );
    assert_not_the_trash_root(&gone);
}

#[tokio::test]
async fn nextcloud_purge_of_the_root_sends_nothing() {
    let (base, hits) = serve(false, false).await;
    let mut provider = nextcloud_at(&base);
    assert_eq!(provider.delete_permanent("/").await.ok(), Some(false));
    assert_eq!(provider.delete_permanent("").await.ok(), Some(false));
    assert!(deletes(&hits.lock().unwrap()).is_empty());
}

#[tokio::test]
async fn nextcloud_purge_does_not_delete_when_the_listing_fails() {
    let (base, hits) = serve(true, false).await;
    let mut provider = nextcloud_at(&base);
    let purged = provider.delete_permanent("/docs/run").await;
    assert!(purged.is_err(), "{purged:?}");
    assert!(deletes(&hits.lock().unwrap()).is_empty());
}

#[tokio::test]
async fn nextcloud_identity_for_another_location_is_not_deleted() {
    let (base, hits) = serve(false, false).await;
    let mut provider = nextcloud_at(&base);
    let purged = provider
        .delete_permanent_with_identity("/docs/a.txt", Some("12"))
        .await;
    assert!(matches!(purged, Ok(false)), "{purged:?}");
    assert!(deletes(&hits.lock().unwrap()).is_empty());
}

#[tokio::test]
async fn plain_webdav_purge_sends_nothing() {
    let (base, hits) = serve(false, false).await;
    let mut provider = plain_at(&base);
    assert_eq!(
        provider.delete_permanent("/docs/a.txt").await.ok(),
        Some(false)
    );
    assert_eq!(
        provider
            .delete_permanent_with_identity("/docs/a.txt", Some("42"))
            .await
            .ok(),
        Some(false)
    );
    let captured = provider
        .capture_trash_identities(&["/docs/a.txt".to_string()])
        .await;
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].as_ref().ok().and_then(|id| id.as_deref()), None);
    assert!(
        hits.lock().unwrap().is_empty(),
        "{:?}",
        hits.lock().unwrap()
    );
}

#[tokio::test]
async fn koofr_webdav_keeps_its_trash_note_and_sends_nothing() {
    let (base, hits) = serve(false, false).await;
    let mut provider = koofr_at(&base);
    assert!(provider.trash_left_unpurged().is_some());
    assert_eq!(
        provider.delete_permanent("/docs/a.txt").await.ok(),
        Some(false)
    );
    assert!(hits.lock().unwrap().is_empty());
}
