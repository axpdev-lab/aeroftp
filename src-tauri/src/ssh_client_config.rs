use std::borrow::Cow;

use russh::{kex, Preferred};

/// Shared SSH client policy. NIST ECDH gives standards-compliant servers an
/// efficient SHA-2 alternative to finite-field GEX without lowering its floor.
pub(crate) fn preferred() -> Preferred {
    let mut preferred = Preferred::default();
    let mut kex = preferred.kex.to_vec();
    let ecdh_at = kex
        .iter()
        .position(|name| *name == kex::DH_GEX_SHA256)
        .unwrap_or(kex.len());
    kex.splice(
        ecdh_at..ecdh_at,
        [
            kex::ECDH_SHA2_NISTP256,
            kex::ECDH_SHA2_NISTP384,
            kex::ECDH_SHA2_NISTP521,
        ],
    );
    preferred.kex = Cow::Owned(kex);
    preferred
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_policy_adds_nist_ecdh_without_changing_the_other_kex_choices() {
        let configured = preferred();
        let names: Vec<&str> = configured.kex.iter().map(|name| name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "mlkem768x25519-sha256",
                "curve25519-sha256",
                "curve25519-sha256@libssh.org",
                "ecdh-sha2-nistp256",
                "ecdh-sha2-nistp384",
                "ecdh-sha2-nistp521",
                "diffie-hellman-group-exchange-sha256",
                "diffie-hellman-group18-sha512",
                "diffie-hellman-group17-sha512",
                "diffie-hellman-group16-sha512",
                "diffie-hellman-group15-sha512",
                "diffie-hellman-group14-sha256",
                "ext-info-c",
                "ext-info-s",
                "kex-strict-c-v00@openssh.com",
                "kex-strict-s-v00@openssh.com",
            ]
        );
    }
}
