// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Redirect policy for HTTP clients that authenticate with a header reqwest
//! does not know is a secret.
//!
//! reqwest's default policy follows up to ten redirects, and when a redirect
//! changes host, port or scheme it removes only `Authorization`, `Cookie`,
//! `cookie2`, `Proxy-Authorization` and `WWW-Authenticate`
//! (`remove_sensitive_headers` in reqwest 0.13 `redirect.rs`). A key sent as
//! `x-api-key`, `PRIVATE-TOKEN` or `X-InfiniCLOUD-API-KEY` travels on to
//! wherever the redirect points, plaintext `http://` included. This is the
//! class rclone fixed in 1.75.1 (GHSA-486v-q2wf-fp2r).
//!
//! The S3 client answers it with `Policy::none()`. The clients here keep
//! following redirects that stay on the origin the request was addressed to,
//! because the servers behind them (self-hosted Immich and GitLab in
//! particular) commonly sit behind proxies that redirect within the same site,
//! and they stop at anything else: the caller then receives the 3xx response
//! as an error instead of the secret being replayed to another origin.

use reqwest::redirect::Policy;
use reqwest::Url;

/// Same ceiling as reqwest's default policy.
const MAX_REDIRECTS: usize = 10;

/// True when a redirect to `next` keeps the secret on the origin the request
/// was sent to: same scheme, host and port, or an upgrade from `http` to
/// `https` on the same host and port. A downgrade, another host or another
/// port is a different origin, an upgrade included.
fn keeps_origin(original: &Url, next: &Url) -> bool {
    let same_host = original.host_str() == next.host_str();
    let same_origin = same_host
        && original.scheme() == next.scheme()
        && original.port_or_known_default() == next.port_or_known_default();
    // `Url::port()` is `None` for the scheme's default port, so the canonical
    // `http://host` to `https://host` (80 to 443) qualifies, while an explicit
    // port has to stay the same: `http://host:8080` to `https://host:8443` is
    // another service on the same machine.
    let upgrade = same_host
        && original.scheme() == "http"
        && next.scheme() == "https"
        && original.port() == next.port();
    same_origin || upgrade
}

/// Follow redirects only while they stay on the origin of the original
/// request. Use it on every client that sends a credential in a header other
/// than `Authorization`.
pub fn same_origin_redirect_policy() -> Policy {
    Policy::custom(|attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let keeps = match attempt.previous().first() {
            Some(original) => keeps_origin(original, attempt.url()),
            None => true,
        };
        if keeps {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

/// A local origin that redirects to a second local origin, for pinning that a
/// client with a secret header never reaches the second one.
#[cfg(test)]
pub(crate) mod fixture {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    pub(crate) struct CrossOriginRedirect {
        /// On the origin, answers 302 to the other origin.
        pub cross: String,
        /// On the origin, answers 302 to a path on the same origin.
        pub same: String,
        /// On the origin, answers 302 to itself, forever.
        pub looping: String,
        /// On the origin, `{hops}/{n}` redirects `n` times before answering.
        pub hops: String,
        pub other_origin_hits: Arc<AtomicUsize>,
        pub other_origin_saw_secret: Arc<AtomicBool>,
        pub same_origin_saw_secret: Arc<AtomicBool>,
    }

    impl CrossOriginRedirect {
        pub(crate) fn other_origin_reached(&self) -> bool {
            self.other_origin_hits.load(Ordering::SeqCst) > 0
        }
        pub(crate) fn secret_leaked(&self) -> bool {
            self.other_origin_saw_secret.load(Ordering::SeqCst)
        }
        pub(crate) fn same_origin_got_secret(&self) -> bool {
            self.same_origin_saw_secret.load(Ordering::SeqCst)
        }
    }

    /// `secret_header` is the header whose arrival is recorded, lowercase.
    pub(crate) async fn spawn(secret_header: &'static str) -> CrossOriginRedirect {
        use axum::http::{header::LOCATION, HeaderMap, StatusCode};
        use axum::routing::get;

        let other_origin_hits = Arc::new(AtomicUsize::new(0));
        let other_origin_saw_secret = Arc::new(AtomicBool::new(false));
        let same_origin_saw_secret = Arc::new(AtomicBool::new(false));

        let other = {
            let hits = Arc::clone(&other_origin_hits);
            let saw = Arc::clone(&other_origin_saw_secret);
            axum::Router::new().route(
                "/sink",
                get(move |headers: HeaderMap| {
                    let hits = Arc::clone(&hits);
                    let saw = Arc::clone(&saw);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        if headers.contains_key(secret_header) {
                            saw.store(true, Ordering::SeqCst);
                        }
                        "other origin"
                    }
                }),
            )
        };
        let other_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind other origin");
        let other_addr = other_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(other_listener, other).await.ok();
        });

        let sink = format!("http://{other_addr}/sink");
        let origin = {
            let saw = Arc::clone(&same_origin_saw_secret);
            axum::Router::new()
                .route(
                    "/cross",
                    get(move || {
                        let sink = sink.clone();
                        async move { (StatusCode::FOUND, [(LOCATION, sink)]) }
                    }),
                )
                .route(
                    "/same",
                    get(|| async { (StatusCode::FOUND, [(LOCATION, "/landed")]) }),
                )
                .route(
                    "/loop",
                    get(|| async { (StatusCode::FOUND, [(LOCATION, "/loop")]) }),
                )
                .route(
                    "/hops/{n}",
                    get(|axum::extract::Path(n): axum::extract::Path<u32>| async move {
                        use axum::response::IntoResponse;
                        if n == 0 {
                            "landed".into_response()
                        } else {
                            (StatusCode::FOUND, [(LOCATION, format!("/hops/{}", n - 1))])
                                .into_response()
                        }
                    }),
                )
                .route(
                    "/landed",
                    get(move |headers: HeaderMap| {
                        let saw = Arc::clone(&saw);
                        async move {
                            if headers.contains_key(secret_header) {
                                saw.store(true, Ordering::SeqCst);
                            }
                            "landed"
                        }
                    }),
                )
        };
        let origin_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind origin");
        let origin_addr = origin_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(origin_listener, origin).await.ok();
        });

        CrossOriginRedirect {
            cross: format!("http://{origin_addr}/cross"),
            same: format!("http://{origin_addr}/same"),
            looping: format!("http://{origin_addr}/loop"),
            hops: format!("http://{origin_addr}/hops"),
            other_origin_hits,
            other_origin_saw_secret,
            same_origin_saw_secret,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn only_the_same_origin_or_an_https_upgrade_keeps_the_origin() {
        let base = url("https://photos.example.com/api/assets");
        assert!(keeps_origin(
            &base,
            &url("https://photos.example.com/api/other")
        ));
        assert!(keeps_origin(
            &base,
            &url("https://photos.example.com:443/x")
        ));
        assert!(keeps_origin(
            &url("http://photos.example.com/api"),
            &url("https://photos.example.com/api")
        ));

        assert!(!keeps_origin(
            &base,
            &url("http://photos.example.com/api/assets")
        ));
        assert!(!keeps_origin(
            &base,
            &url("https://photos.example.com:8443/x")
        ));
        assert!(!keeps_origin(&base, &url("https://cdn.example.net/blob")));
        assert!(!keeps_origin(&base, &url("https://example.com/api/assets")));
    }

    #[test]
    fn an_https_upgrade_must_keep_the_port() {
        // Canonical upgrade: the default port on both sides.
        assert!(keeps_origin(
            &url("http://photos.example.com/api"),
            &url("https://photos.example.com/api")
        ));
        // The same explicit port.
        assert!(keeps_origin(
            &url("http://photos.example.com:8080/api"),
            &url("https://photos.example.com:8080/api")
        ));
        // Another service on the same machine, whatever the scheme does.
        assert!(!keeps_origin(
            &url("http://photos.example.com:8080/api"),
            &url("https://photos.example.com:8443/api")
        ));
        assert!(!keeps_origin(
            &url("http://photos.example.com/api"),
            &url("https://photos.example.com:8443/api")
        ));
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(same_origin_redirect_policy())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn a_redirect_to_another_origin_is_returned_not_followed() {
        let fx = fixture::spawn("x-api-key").await;
        let response = client()
            .get(&fx.cross)
            .header("x-api-key", "SECRET")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert!(!fx.other_origin_reached(), "the other origin was contacted");
        assert!(!fx.secret_leaked());
    }

    #[tokio::test]
    async fn a_redirect_within_the_origin_is_still_followed_with_the_secret() {
        let fx = fixture::spawn("x-api-key").await;
        let response = client()
            .get(&fx.same)
            .header("x-api-key", "SECRET")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(fx.same_origin_got_secret());
    }

    #[tokio::test]
    async fn a_redirect_loop_still_ends_in_an_error() {
        let fx = fixture::spawn("x-api-key").await;
        let error = client().get(&fx.looping).send().await.unwrap_err();
        assert!(
            error.is_redirect(),
            "expected a redirect error, got {error:?}"
        );
    }

    /// The ceiling is pinned against reqwest's default policy itself, not
    /// against a reading of its source: whatever number of redirects the
    /// default client follows, this policy follows the same, one more fails.
    #[tokio::test]
    async fn the_redirect_ceiling_is_the_same_as_reqwests_default() {
        let fx = fixture::spawn("x-api-key").await;
        let reference = reqwest::Client::new();
        for hops in [10u32, 11] {
            let url = format!("{}/{hops}", fx.hops);
            let ours = client().get(&url).send().await;
            let default = reference.get(&url).send().await;
            assert_eq!(
                ours.is_ok(),
                default.is_ok(),
                "{hops} redirects: this policy {ours:?}, reqwest default {default:?}"
            );
        }
        assert!(client().get(format!("{}/10", fx.hops)).send().await.is_ok());
        let error = client()
            .get(format!("{}/11", fx.hops))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_redirect(), "expected a redirect error, got {error:?}");
    }

    /// The two shared AI clients carry `x-api-key` (Anthropic) and
    /// `x-goog-api-key` (Gemini) on every call; they are the only reqwest
    /// clients `ai.rs` and `ai_stream.rs` build.
    #[tokio::test]
    async fn the_ai_clients_never_carry_an_api_key_to_another_origin() {
        for (name, client) in [
            ("AI_HTTP_CLIENT", &*crate::ai::AI_HTTP_CLIENT),
            ("AI_STREAM_CLIENT", &*crate::ai::AI_STREAM_CLIENT),
        ] {
            let fx = fixture::spawn("x-goog-api-key").await;
            let response = client
                .get(&fx.cross)
                .header("x-goog-api-key", "SECRET")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::FOUND, "{name}");
            assert!(!fx.secret_leaked(), "{name} replayed the key");
            assert!(!fx.other_origin_reached(), "{name} followed the redirect");
        }
    }
}
