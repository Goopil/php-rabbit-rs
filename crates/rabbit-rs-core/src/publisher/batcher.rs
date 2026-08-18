pub struct Batcher<T> {
    items: Vec<T>,
    spare: Vec<T>,
    bytes: usize,
    max_messages: usize,
    max_bytes: usize,
}

impl<T> Batcher<T> {
    #[must_use]
    pub fn new(max_messages: usize, max_bytes: usize) -> Self {
        let cap = max_messages.max(1);
        Self {
            items: Vec::with_capacity(cap),
            spare: Vec::with_capacity(cap),
            bytes: 0,
            max_messages: max_messages.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    pub fn push(&mut self, item: T, bytes: usize) -> bool {
        self.items.push(item);
        self.bytes = self.bytes.saturating_add(bytes);
        self.items.len() >= self.max_messages || self.bytes >= self.max_bytes
    }

    pub fn take(&mut self) -> Vec<T> {
        self.bytes = 0;
        std::mem::swap(&mut self.items, &mut self.spare);
        std::mem::replace(&mut self.spare, Vec::with_capacity(self.max_messages))
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[cfg(test)]
    #[must_use]
    pub const fn items_capacity(&self) -> usize {
        self.items.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::Batcher;

    #[test]
    fn take_preserves_capacity_for_next_batch() {
        let mut batcher = Batcher::<u32>::new(256, 4096);
        for i in 0..256 {
            batcher.push(i, 1);
        }
        let taken = batcher.take();
        assert_eq!(taken.len(), 256);
        assert!(
            batcher.items_capacity() >= 256,
            "capacity {} should be >= 256",
            batcher.items_capacity()
        );
    }

    #[test]
    fn take_returns_all_items_and_empties() {
        let mut batcher = Batcher::<u32>::new(8, 1024);
        batcher.push(1, 10);
        batcher.push(2, 20);
        batcher.push(3, 30);
        let taken = batcher.take();
        assert_eq!(taken, vec![1, 2, 3]);
        assert!(batcher.is_empty());
    }

    #[test]
    fn repeated_push_take_cycles_do_not_reallocate() {
        let mut batcher = Batcher::<u32>::new(64, 8192);
        for cycle in 0..10 {
            for i in 0..64 {
                batcher.push(i, 1);
            }
            let taken = batcher.take();
            assert_eq!(taken.len(), 64, "cycle {cycle}");
            assert!(
                batcher.items_capacity() >= 64,
                "cycle {cycle}: capacity {} should be >= 64",
                batcher.items_capacity()
            );
        }
    }
}
