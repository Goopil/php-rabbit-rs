pub struct Batcher<T> {
    items: Vec<T>,
    bytes: usize,
    max_messages: usize,
    max_bytes: usize,
}

impl<T> Batcher<T> {
    #[must_use]
    pub fn new(max_messages: usize, max_bytes: usize) -> Self {
        Self {
            items: Vec::with_capacity(max_messages.max(1)),
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
        std::mem::take(&mut self.items)
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
