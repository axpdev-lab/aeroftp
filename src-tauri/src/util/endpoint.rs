// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Endpoint host predicates shared by more than one caller.

/// Whether an endpoint URL points at this machine's loopback interface
/// (127.0.0.0/8, `::1`, or `localhost`), so traffic to it never reaches a
/// network.
///
/// The name states the guarantee rather than the syntax, because the guarantee
/// is what both callers actually depend on and because a similarly named
/// predicate nearby promises something else. `providers::s3::is_local_s3_endpoint`
/// answers a *different* question: it also accepts `*.local` and `*.localhost`,
/// which are mDNS names on a LAN. That is right for deciding whether to accept
/// a self-signed certificate from a local bridge, and wrong for deciding whether
/// bytes stay off the wire, because a `.local` host is reached across a network
/// segment that other machines share. Keep the two apart: they look
/// interchangeable and are not.
///
/// Used to decide whether a self-signed certificate is acceptable on export to
/// rclone (which rejects the Filen Desktop gateway's IP-SAN-less certificate
/// unless `no_check_certificate` is set), and whether an `http://` S3 endpoint
/// needs the profile's explicit cleartext consent.
pub fn endpoint_stays_on_this_machine(endpoint: &str) -> bool {
    // The host the request reaches, from the URL parser (#1506 Requested 188):
    // splitting the text took `http://evil.com#@127.0.0.1` and
    // `http://evil.com\@127.0.0.1` for loopback, so an `http://` S3 endpoint
    // on another machine skipped its cleartext consent.
    let Some(host) = endpoint_host(endpoint) else {
        return false;
    };
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The host of an endpoint as the HTTP stack will connect to it, lowercase and
/// without brackets, or `None` when there is no host. A schemeless endpoint
/// (`host:port`, `host/path`) is read as `http://` first.
///
/// Every "is this host local or private" predicate must start here instead of
/// splitting strings: split-based extraction kept `?` and `#` in the host
/// (`evil.com?.local` read as a `.local` name), and prefix tests on the text
/// took `10.example.com` or `192.168.1.1.attacker.com` for private addresses
/// (#1506 Requested 188). The URL parser applies the same WHATWG rules reqwest
/// does, so the host judged here is the host the request reaches.
pub fn endpoint_host(endpoint: &str) -> Option<String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let url = url::Url::parse(&with_scheme).ok()?;
    let host = match url.host()? {
        url::Host::Domain(domain) => domain.to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    Some(host)
}

/// Whether `host` (as returned by [`endpoint_host`]) is a loopback, private
/// (RFC 1918) or link-local IP address, judged on the parsed address, never
/// on the text.
pub fn host_is_private_ip(host: &str) -> bool {
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1506 Requested 188: the host is what the URL parser says, so query,
    /// fragment, userinfo and port tricks cannot make a public host look local.
    #[test]
    fn endpoint_host_is_the_host_the_request_reaches() {
        for (endpoint, host) in [
            ("https://Example.COM:8443/path", "example.com"),
            ("example.com:9000", "example.com"),
            ("evil.com?.local", "evil.com"),
            ("evil.com#.local", "evil.com"),
            ("http://user:pw@evil.com/x.local", "evil.com"),
            ("[::1]:1800", "::1"),
            (
                "http://192.168.1.1.attacker.com:8080",
                "192.168.1.1.attacker.com",
            ),
            ("10.0.0.5:1900", "10.0.0.5"),
        ] {
            assert_eq!(endpoint_host(endpoint).as_deref(), Some(host), "{endpoint}");
        }
        assert_eq!(endpoint_host(""), None);
    }

    #[test]
    fn private_ip_is_judged_on_the_address_not_the_text() {
        for private in [
            "10.0.0.5",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.50",
            "169.254.1.1",
            "127.0.0.1",
            "127.5.6.7",
            "::1",
        ] {
            assert!(host_is_private_ip(private), "{private}");
        }
        for public in [
            "10.example.com",
            "192.168.1.1.attacker.com",
            "172.20.0.1.x.com",
            "127.evil.com",
            "172.32.0.1",
            "8.8.8.8",
            "example.com",
        ] {
            assert!(!host_is_private_ip(public), "{public}");
        }
    }

    #[test]
    fn loopback_endpoints_stay_on_this_machine() {
        assert!(endpoint_stays_on_this_machine("https://127.0.0.1:1800"));
        assert!(endpoint_stays_on_this_machine("http://localhost:9000"));
        assert!(endpoint_stays_on_this_machine("https://127.0.0.1"));
        assert!(endpoint_stays_on_this_machine("https://[::1]:443"));
        assert!(endpoint_stays_on_this_machine(
            "https://127.5.6.7:8000/path"
        ));
    }

    #[test]
    fn routed_endpoints_do_not() {
        assert!(!endpoint_stays_on_this_machine(
            "https://storage.googleapis.com"
        ));
        assert!(!endpoint_stays_on_this_machine(
            "https://s3.eu-central-003.backblazeb2.com"
        ));
        // A bucket literally named "localhost" must not be misread: the host
        // here is the real endpoint, not the path segment.
        assert!(!endpoint_stays_on_this_machine(
            "https://s3.example.com/localhost"
        ));
    }

    /// The host is the one the URL parser reaches, so text after `#`, `?` or a
    /// backslash cannot pose as the userinfo of a loopback address.
    #[test]
    fn a_loopback_address_after_a_fragment_or_backslash_is_not_the_host() {
        for endpoint in [
            "http://evil.com#@127.0.0.1",
            "http://evil.com?@127.0.0.1",
            "http://evil.com\\@127.0.0.1:9000",
            "http://localhost.evil.com",
            "evil.com#@localhost",
        ] {
            assert!(!endpoint_stays_on_this_machine(endpoint), "{endpoint}");
        }
        assert!(endpoint_stays_on_this_machine("user:pw@127.0.0.1:9000"));
        assert!(endpoint_stays_on_this_machine("http://LOCALHOST:9000"));
    }

    /// The boundary against the other "local" predicate. An mDNS name is on a
    /// network, so it must answer false here even though it answers true to
    /// `is_local_s3_endpoint`. This test is the one that fails if the two are
    /// ever merged.
    #[test]
    fn mdns_and_lan_names_are_not_this_machine() {
        assert!(!endpoint_stays_on_this_machine("http://minio.local:9000"));
        assert!(!endpoint_stays_on_this_machine("http://nas.local"));
        assert!(!endpoint_stays_on_this_machine("http://minio.lan:9000"));
        assert!(!endpoint_stays_on_this_machine("http://192.168.1.10:9000"));
        assert!(!endpoint_stays_on_this_machine("http://10.0.0.5:9000"));
    }
}
