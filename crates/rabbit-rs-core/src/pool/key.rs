use crate::config::ValidatedConfig;

/// Stable identity of a normalized connection-pool configuration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ConnectionKey([u8; 32]);

impl ConnectionKey {
    /// Builds a pool key from validated, canonical configuration.
    #[must_use]
    pub fn from_config(config: &ValidatedConfig) -> Self {
        Self(config.fingerprint().into_bytes())
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
}
