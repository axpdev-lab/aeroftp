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
    let without_scheme = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(endpoint);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    // Drop any userinfo (`user:pass@host`).
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 literals are bracketed: `[::1]:1800`.
    let host = if let Some(rest) = hostport.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

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
