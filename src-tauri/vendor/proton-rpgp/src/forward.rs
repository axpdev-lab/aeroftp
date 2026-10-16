//! Support for `draft-wussler-openpgp-forwarding`.
//!
//! Forwarding lets a proxy re-target a message that was encrypted to a
//! forwarder key so that a forwardee key can decrypt it, without the proxy ever
//! learning the session key.
//!
//! - [`ForwardingKeyGenerator`] derives a forwardee key from a forwarder key,
//!   together with the [`ForwardingInstance`]s the proxy needs.
//! - [`ForwardingPkesk`] transforms a message's PKESK with those instances,
//!   yielding a [`ForwardedPkesk`].
//!
//! Reading forwarded messages is handled by the regular
//! [`Decryptor`](crate::Decryptor) via
//! [`allow_forwarding_decryption`](crate::Decryptor::allow_forwarding_decryption).

mod generation;
mod instance;
mod pkesk;

pub use generation::*;
pub use instance::*;
pub use pkesk::*;
