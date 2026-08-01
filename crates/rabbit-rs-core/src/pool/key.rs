use crate::config::ValidatedConfig;
use std::fmt::Write;

/// Stable identity of a normalized connection-pool configuration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionKey([u8; 32]);

impl ConnectionKey {
    /// Builds a pool key from a validated, canonical configuration.
    #[must_use]
    pub fn from_config(config: &ValidatedConfig) -> Self {
        Self(config.fingerprint().into_bytes())
    }

    /// Returns the non-reversible pool identity as lowercase hexadecimal.
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut encoded = String::with_capacity(64);
        for byte in self.0 {
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        encoded
    }

    #[cfg(test)]
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionKey;

    #[test]
    fn connection_keys_are_stable_and_distinct() {
        assert_eq!(
            ConnectionKey::from_bytes([1; 32]),
            ConnectionKey::from_bytes([1; 32])
        );
        assert_ne!(
            ConnectionKey::from_bytes([1; 32]),
            ConnectionKey::from_bytes([2; 32])
        );
    }

    #[test]
    fn connection_key_has_a_stable_hex_representation() {
        assert_eq!(
            ConnectionKey::from_bytes([0xab; 32]).to_hex(),
            "ab".repeat(32)
        );
    }
}
