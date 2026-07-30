use std::collections::HashMap;

use tokio::sync::oneshot;

use super::{PublishError, PublishOutcome};

pub struct PendingConfirmation {
    pub message_id: String,
    pub completion: oneshot::Sender<Result<PublishOutcome, PublishError>>,
}

#[derive(Default)]
pub struct ConfirmLedger {
    pending: HashMap<u64, PendingConfirmation>,
}

impl ConfirmLedger {
    pub fn insert(&mut self, sequence: u64, pending: PendingConfirmation) {
        self.pending.insert(sequence, pending);
    }

    pub fn remove(&mut self, sequence: u64) -> Option<PendingConfirmation> {
        self.pending.remove(&sequence)
    }

    pub fn drain(&mut self) -> impl Iterator<Item = PendingConfirmation> + '_ {
        self.pending.drain().map(|(_, pending)| pending)
    }
}
