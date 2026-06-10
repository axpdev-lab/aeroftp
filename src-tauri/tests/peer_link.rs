//! WI-3b: first physical convergence increment. The main app crate links the
//! peer-l0 crate (dev-dependency) and exercises its crypto surface end to end.

use aeroftp_peer_l0::{decode_secret, decrypt_blob, derive_drive_key, encrypt_blob};

const TEST_SECRET_B64: &str = "KlchUcAY5dwVZrNECsbd3w";

#[test]
fn drive_key_is_deterministic_and_namespace_separated() {
    let secret = decode_secret(TEST_SECRET_B64).expect("decode test secret");
    let ns_a = "8ce68153cc3b80d778b594b7e3787e3511745ca28b384ebdb4fab5ec41be0832";
    let ns_b = "1111111111111111111111111111111111111111111111111111111111111111";
    let k_a1 = derive_drive_key(&secret, ns_a);
    let k_a2 = derive_drive_key(&secret, ns_a);
    let k_b = derive_drive_key(&secret, ns_b);
    assert_eq!(k_a1, k_a2, "same secret+namespace must derive the same key");
    assert_ne!(k_a1, k_b, "different namespaces must derive different keys");
}

#[test]
fn encrypted_blob_round_trips_through_the_linked_crate() {
    let secret = decode_secret(TEST_SECRET_B64).expect("decode test secret");
    let key = derive_drive_key(&secret, "wi3b-link-test-namespace");
    let plaintext = b"aeroftp peer link smoke payload";
    let (nonce, ciphertext) = encrypt_blob(&key, plaintext).expect("encrypt");
    assert_eq!(nonce.len(), 12, "AES-GCM nonce is 12 bytes");
    let decrypted = decrypt_blob(&key, &nonce, &ciphertext).expect("decrypt");
    assert_eq!(
        decrypted.as_slice(),
        &plaintext[..],
        "round trip must restore the plaintext"
    );
}
