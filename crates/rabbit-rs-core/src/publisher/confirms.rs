use std::collections::BTreeMap;

pub struct ConfirmLedger<T> {
    pending: BTreeMap<u64, T>,
}

impl<T> Default for ConfirmLedger<T> {
    fn default() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }
}

impl<T> ConfirmLedger<T> {
    pub fn insert(&mut self, sequence: u64, pending: T) {
        self.pending.insert(sequence, pending);
    }

    pub fn remove(&mut self, sequence: u64) -> Option<T> {
        self.pending.remove(&sequence)
    }

    pub fn drain(&mut self) -> impl Iterator<Item = T> {
        std::mem::take(&mut self.pending).into_values()
    }
}
