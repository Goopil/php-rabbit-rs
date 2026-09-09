//! Weighted scheduling across ready subscriptions.

use std::sync::Arc;

/// Stable identity of a configured subscription.
///
/// The inner `Arc<str>` makes per-delivery clones (one per incoming message
/// in the pump and one per dispatch) an atomic refcount bump instead of a
/// heap allocation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SubscriptionId(Arc<str>);

impl SubscriptionId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into().into_boxed_str().into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Scheduling parameters attached to one subscription.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionPolicy {
    weight: u16,
}

impl SubscriptionPolicy {
    /// Creates a validated subscription policy.
    ///
    /// # Panics
    ///
    /// Panics when `weight` is zero. Runtime configuration validation
    /// prevents this before registration.
    #[must_use]
    pub fn new(weight: u16) -> Self {
        assert!(weight > 0, "subscription weight must be greater than zero");

        Self { weight }
    }
}

/// Selects the next ready subscription.
///
/// A deterministic smooth weighted scheduler: proportional sharing by
/// weight, and weighted-fair cannot starve by construction.
#[derive(Debug, Default)]
pub struct WeightedFairScheduler {
    entries: Vec<Entry>,
    cursor: usize,
}

#[derive(Debug)]
struct Entry {
    id: SubscriptionId,
    policy: SubscriptionPolicy,
    ready: bool,
    credit: i64,
}

impl WeightedFairScheduler {
    /// Registers a subscription or replaces its scheduling policy.
    pub fn register(&mut self, id: SubscriptionId, policy: SubscriptionPolicy) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) {
            entry.policy = policy;
            return;
        }

        self.entries.push(Entry {
            id,
            policy,
            ready: false,
            credit: 0,
        });
    }

    pub fn mark_ready(&mut self, id: &SubscriptionId) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| &entry.id == id)
            && !entry.ready
        {
            entry.ready = true;
            entry.credit = 0;
        }
    }

    pub fn mark_empty(&mut self, id: &SubscriptionId) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| &entry.id == id) {
            entry.ready = false;
            entry.credit = 0;
        }
    }

    pub fn pick(&mut self) -> Option<SubscriptionId> {
        let eligible = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.ready.then_some(index))
            .collect::<Vec<_>>();

        let total_weight = eligible
            .iter()
            .map(|&index| i64::from(self.entries[index].policy.weight))
            .sum::<i64>();

        for &index in &eligible {
            self.entries[index].credit += i64::from(self.entries[index].policy.weight);
        }

        let chosen = (0..self.entries.len())
            .map(|offset| (self.cursor + offset) % self.entries.len())
            .filter(|index| eligible.contains(index))
            .max_by_key(|&index| self.entries[index].credit)?;

        let entry = &mut self.entries[chosen];
        entry.credit -= total_weight;
        let selected = entry.id.clone();
        self.cursor = (chosen + 1) % self.entries.len();

        Some(selected)
    }
}
