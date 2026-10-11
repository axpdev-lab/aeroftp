// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const HEAD: &str =
    r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns""#;
fn dav_error(namespace: &str, exception: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><z:error xmlns:z="DAV:" xmlns:x="{namespace}"><x:exception>{exception}</x:exception><x:message>Missing fixture resource</x:message></z:error>"#
    )
}
fn missing() -> String {
    format!(
        "{HEAD}{}",
        dav_error("http://sabredav.org/ns", r"Sabre\DAV\Exception\NotFound")
    )
}
fn provider() -> WebDavProvider {
    WebDavProvider::new(WebDavConfig {
        url: "http://127.0.0.1".into(),
        username: "fixture".into(),
        password: secrecy::SecretString::from("fixture-password".to_string()),
        initial_path: None,
        provider_id: None,
        verify_cert: true,
        anonymous: false,
    })
    .unwrap()
}
async fn wire(body: String) -> (WebDavProvider, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut p = provider();
    p.config.url = format!("http://{}", listener.local_addr().unwrap());
    p.connected = true;
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = Vec::new();
            let mut block = [0u8; 4096];
            let end = loop {
                let n = stream.read(&mut block).await.unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&block[..n]);
                if let Some(i) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..end]);
            assert!(head.starts_with("PROPFIND "));
            let length = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            while request.len() < end + length {
                let n = stream.read(&mut block).await.unwrap();
                if n == 0 {
                    return;
                }
                request.extend_from_slice(&block[..n]);
            }
            let reply = format!("HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
            stream.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (p, task)
}
#[tokio::test]
async fn owncloud_207_stat_is_not_found_instead_of_empty_metadata() {
    let (mut p, task) = wire(missing()).await;
    let result = p.stat("/missing").await;
    task.abort();
    assert!(
        matches!(result,Err(ProviderError::NotFound(ref path)) if path=="/missing"),
        "{result:?}"
    );
}
#[tokio::test]
async fn owncloud_207_list_is_not_found_instead_of_empty_listing() {
    let (mut p, task) = wire(missing()).await;
    let result = p.list("/missing").await;
    task.abort();
    assert!(
        matches!(result,Err(ProviderError::NotFound(ref path)) if path=="/missing"),
        "{result:?}"
    );
}
#[tokio::test]
async fn owncloud_207_cd_reports_missing_and_keeps_current_path() {
    let (mut p, task) = wire(missing()).await;
    let result = p.cd("/missing").await;
    task.abort();
    assert!(
        matches!(result,Err(ProviderError::NotFound(ref path)) if path=="/missing"),
        "{result:?}"
    );
    assert_eq!(p.current_path, "/");
}
#[test]
fn incomplete_listing_cannot_return_accumulated_entries() {
    let xml = r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/ok.txt</d:href><d:propstat><d:prop><d:getcontentlength>7</d:getcontentlength></d:prop></d:propstat></d:response><d:response>"#;
    assert!(matches!(
        provider().parse_propfind_response(xml, "/"),
        Err(ProviderError::ParseError(_))
    ));
}
#[test]
fn not_found_text_in_valid_filename_and_property_is_not_an_error() {
    let xml = r#"<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns"><d:response><d:href>/Sabre%5CDAV%5CException%5CNotFound.txt</d:href><d:propstat><d:prop><d:displayname>Sabre\DAV\Exception\NotFound.txt</d:displayname><s:exception>Sabre\DAV\Exception\NotFound</s:exception><d:getcontentlength>7</d:getcontentlength></d:prop></d:propstat></d:response></d:multistatus>"#;
    let entries = provider().parse_propfind_response(xml, "/").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].size, 7);
}
#[test]
fn wrong_namespace_and_other_exception_are_parse_errors() {
    for tail in [
        dav_error("urn:not-sabre", r"Sabre\DAV\Exception\NotFound"),
        dav_error("http://sabredav.org/ns", r"Sabre\DAV\Exception\Forbidden"),
    ] {
        let result = provider().parse_propfind_response(&format!("{HEAD}{tail}"), "/missing");
        assert!(
            matches!(result, Err(ProviderError::ParseError(_))),
            "{result:?}"
        );
    }
}

#[tokio::test]
async fn owncloud_207_recursive_list_also_reports_missing() {
    let (mut p, task) = wire(missing()).await;
    let result = p.list_recursive("/missing").await;
    task.abort();
    assert!(
        matches!(result, Err(ProviderError::NotFound(ref path)) if path == "/missing"),
        "{result:?}"
    );
}
#[tokio::test]
async fn truncated_stat_is_parse_error_instead_of_zero_byte_file() {
    let (mut p, task) = wire(r#"<d:multistatus xmlns:d="DAV:"><d:response>"#.into()).await;
    let result = p.stat("/missing").await;
    task.abort();
    assert!(
        matches!(result, Err(ProviderError::ParseError(_))),
        "{result:?}"
    );
}
#[test]
fn standalone_dav_error_resolves_namespace_instead_of_fixed_prefix() {
    for xml in [dav_error("http://sabredav.org/ns", r"Sabre\DAV\Exception\NotFound"),
        r#"<error xmlns="DAV:"><exception xmlns="http://sabredav.org/ns"><![CDATA[Sabre\DAV\Exception\NotFound]]></exception></error>"#.into()] {
        assert!(matches!(provider().parse_propfind_response(&xml, "/missing"), Err(ProviderError::NotFound(_))));
    }
}
#[test]
fn embedded_error_document_in_cdata_does_not_classify_a_file() {
    let xml = format!(
        r#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/normal.txt</d:href><d:propstat><d:prop><d:displayname><![CDATA[{}]]></d:displayname><d:getcontentlength>9</d:getcontentlength></d:prop></d:propstat></d:response></d:multistatus>"#,
        missing()
    );
    let entries = provider().parse_propfind_response(&xml, "/").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].size, 9);
}

#[test]
fn unknown_entity_cannot_turn_another_exception_into_not_found() {
    let xml = dav_error(
        "http://sabredav.org/ns",
        r"Sabre\DAV\Exception\NotFound&unknown;",
    );
    assert!(matches!(
        provider().parse_propfind_response(&xml, "/missing"),
        Err(ProviderError::ParseError(_))
    ));
}
