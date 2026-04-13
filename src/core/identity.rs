//! Peer identity — PGP fingerprints, keyring management.

/// A peer's identity, derived from their PGP public key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PeerIdentity {
    /// PGP key fingerprint (hex string).
    pub fingerprint: String,
    /// Display name, if known from the key's UID.
    pub display_name: Option<String>,
}
