use std::collections::HashMap;

pub struct ConfirmLedger<T> {
    pending: HashMap<u64, T>,
}

impl<T> Default for ConfirmLedger<T> {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }
}

impl<T> ConfirmLedger<T> {
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            pending: HashMap::with_capacity(capacity),
        }
    }

    pub fn insert(&mut self, sequence: u64, pending: T) {
        self.pending.insert(sequence, pending);
    }

    pub fn remove(&mut self, sequence: u64) -> Option<T> {
        self.pending.remove(&sequence)
    }

    pub fn drain(&mut self) -> impl Iterator<Item = T> {
        std::mem::take(&mut self.pending).into_values()
    }

    #[cfg(test)]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.pending.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::ConfirmLedger;

    #[test]
    fn drain_returns_all_values_after_mixed_insert_remove() {
        let mut ledger = ConfirmLedger::<&'static str>::with_capacity(8);
        ledger.insert(1, "one");
        ledger.insert(2, "two");
        ledger.insert(3, "three");
        ledger.insert(4, "four");
        ledger.remove(2);
        ledger.remove(4);
        ledger.insert(5, "five");

        let mut drained: Vec<&'static str> = ledger.drain().collect();
        drained.sort_unstable();
        assert_eq!(drained, vec!["five", "one", "three"]);
    }

    #[test]
    fn insert_remove_roundtrip_preserves_value() {
        let mut ledger = ConfirmLedger::<u32>::with_capacity(4);
        ledger.insert(42, 100);
        assert_eq!(ledger.remove(42), Some(100));
        assert_eq!(ledger.remove(42), None);
    }

    #[test]
    fn with_capacity_preallocates() {
        let ledger = ConfirmLedger::<u32>::with_capacity(64);
        assert!(
            ledger.capacity() >= 64,
            "capacity {} should be >= 64",
            ledger.capacity()
        );
    }

    #[test]
    fn default_has_zero_capacity() {
        let ledger = ConfirmLedger::<u32>::default();
        assert_eq!(ledger.capacity(), 0);
    }
}
