//! Weighted scheduling across ready subscriptions.

use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

/// Stable identity of a configured subscription.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
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
    priority_class: i16,
    starvation_after: Duration,
}

impl SubscriptionPolicy {
    /// Creates a validated subscription policy.
    ///
    /// # Panics
    ///
    /// Panics when `weight` is zero or `starvation_after` is zero. Runtime
    /// configuration validation prevents both cases before registration.
    #[must_use]
    pub fn new(weight: u16, priority_class: i16, starvation_after: Duration) -> Self {
        assert!(weight > 0, "subscription weight must be greater than zero");
        assert!(
            !starvation_after.is_zero(),
            "starvation interval must be greater than zero"
        );

        Self {
            weight,
            priority_class,
            starvation_after,
        }
    }
}

/// Selects the next ready subscription.
pub trait Scheduler {
    /// Registers a subscription or replaces its scheduling policy.
    fn register(&mut self, id: SubscriptionId, policy: SubscriptionPolicy);

    /// Marks a subscription as having at least one buffered delivery.
    fn mark_ready(&mut self, id: &SubscriptionId);

    /// Marks a subscription as having no buffered delivery.
    fn mark_empty(&mut self, id: &SubscriptionId);

    /// Returns the next subscription according to priority, aging, and weight.
    fn next(&mut self, now: Instant) -> Option<SubscriptionId>;
}

/// A deterministic smooth weighted scheduler with starvation protection.
///
/// `eligible_indices` is used only for O(1) membership testing during
/// cursor-based selection; iteration order for selection is provided by
/// `eligible_list`, which preserves the same order as an enumerate-based
/// scan of `entries`. `index` maps each `SubscriptionId` to its position in
/// `entries` so `register`/`mark_ready`/`mark_empty` are O(1) lookups
/// instead of linear scans.
#[derive(Debug, Default)]
pub struct WeightedFairScheduler {
    entries: Vec<Entry>,
    cursor: usize,
    index: HashMap<SubscriptionId, usize>,
    eligible_indices: HashSet<usize>,
    eligible_list: Vec<usize>,
}

#[derive(Debug)]
struct Entry {
    id: SubscriptionId,
    policy: SubscriptionPolicy,
    ready: bool,
    ready_since: Option<Instant>,
    credit: i64,
}

impl Scheduler for WeightedFairScheduler {
    fn register(&mut self, id: SubscriptionId, policy: SubscriptionPolicy) {
        if let Some(&position) = self.index.get(&id) {
            self.entries[position].policy = policy;
            return;
        }

        let position = self.entries.len();
        self.entries.push(Entry {
            id: id.clone(),
            policy,
            ready: false,
            ready_since: None,
            credit: 0,
        });
        self.index.insert(id, position);
    }

    fn mark_ready(&mut self, id: &SubscriptionId) {
        if let Some(&position) = self.index.get(id)
            && !self.entries[position].ready
        {
            let entry = &mut self.entries[position];
            entry.ready = true;
            entry.ready_since = None;
            entry.credit = 0;
        }
    }

    fn mark_empty(&mut self, id: &SubscriptionId) {
        if let Some(&position) = self.index.get(id) {
            let entry = &mut self.entries[position];
            entry.ready = false;
            entry.ready_since = None;
            entry.credit = 0;
        }
    }

    fn next(&mut self, now: Instant) -> Option<SubscriptionId> {
        for entry in self.entries.iter_mut().filter(|entry| entry.ready) {
            entry.ready_since.get_or_insert(now);
        }

        let highest_priority = self
            .entries
            .iter()
            .filter(|entry| entry.ready)
            .map(|entry| effective_priority(entry, now))
            .max()?;

        self.eligible_indices.clear();
        self.eligible_list.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.ready && effective_priority(entry, now) == highest_priority {
                self.eligible_indices.insert(index);
                self.eligible_list.push(index);
            }
        }

        let total_weight = self
            .eligible_list
            .iter()
            .map(|&index| i64::from(self.entries[index].policy.weight))
            .sum::<i64>();

        for &index in &self.eligible_list {
            self.entries[index].credit += i64::from(self.entries[index].policy.weight);
        }

        let chosen = (0..self.entries.len())
            .map(|offset| (self.cursor + offset) % self.entries.len())
            .filter(|index| self.eligible_indices.contains(index))
            .max_by_key(|&index| self.entries[index].credit)?;

        let entry = &mut self.entries[chosen];
        entry.credit -= total_weight;
        entry.ready_since = Some(now);
        let selected = entry.id.clone();
        self.cursor = (chosen + 1) % self.entries.len();

        Some(selected)
    }
}

fn effective_priority(entry: &Entry, now: Instant) -> i64 {
    let waiting = entry
        .ready_since
        .map_or(Duration::ZERO, |since| now.saturating_duration_since(since));
    let aging_steps = waiting.as_nanos() / entry.policy.starvation_after.as_nanos();
    let aging_steps = i64::try_from(aging_steps).unwrap_or(i64::MAX);

    i64::from(entry.policy.priority_class).saturating_add(aging_steps)
}
