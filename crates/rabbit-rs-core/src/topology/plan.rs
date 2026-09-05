use std::{collections::BTreeMap, error::Error, fmt};

use crate::{
    config::{TopologyMode, ValidatedConfig},
    transport::{BindingSpec, ExchangeKind, ExchangeSpec, Headers, QueueKind, QueueSpec},
};

use super::delay::{DELAYED_EXCHANGE_NAME, DelayStrategy, delayed_exchange_spec};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueDefinition {
    name: String,
    kind: QueueKind,
    durable: bool,
    exclusive: bool,
    auto_delete: bool,
    delivery_limit: Option<u32>,
    arguments: BTreeMap<String, crate::transport::HeaderValue>,
}

impl QueueDefinition {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: QueueKind::Quorum,
            durable: true,
            exclusive: false,
            auto_delete: false,
            delivery_limit: None,
            arguments: BTreeMap::new(),
        }
    }

    #[must_use]
    pub const fn classic(mut self) -> Self {
        self.kind = QueueKind::Classic;
        self
    }

    #[must_use]
    pub const fn kind(mut self, kind: QueueKind) -> Self {
        self.kind = kind;
        self
    }

    #[must_use]
    pub const fn durable(mut self, durable: bool) -> Self {
        self.durable = durable;
        self
    }

    #[must_use]
    pub const fn exclusive(mut self, exclusive: bool) -> Self {
        self.exclusive = exclusive;
        self
    }

    #[must_use]
    pub const fn auto_delete(mut self, auto_delete: bool) -> Self {
        self.auto_delete = auto_delete;
        self
    }

    #[must_use]
    pub const fn delivery_limit(mut self, limit: u32) -> Self {
        self.delivery_limit = Some(limit);
        self
    }

    fn compile(self) -> Result<QueueSpec, TopologyPlanError> {
        if self.kind == QueueKind::Quorum && (self.exclusive || self.auto_delete) {
            return Err(TopologyPlanError::new(format!(
                "quorum queue '{}' cannot be exclusive or auto-delete",
                self.name
            )));
        }

        Ok(QueueSpec {
            name: self.name,
            durable: self.durable,
            exclusive: self.exclusive,
            auto_delete: self.auto_delete,
            kind: self.kind,
            dead_letter_exchange: None,
            dead_letter_routing_key: None,
            message_ttl: None,
            expires: None,
            delivery_limit: self.delivery_limit,
            arguments: self.arguments,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeadLetterDefinition {
    source_queue: String,
    exchange: String,
    queue: String,
    routing_key: String,
}

impl DeadLetterDefinition {
    #[must_use]
    pub fn new(
        source_queue: impl Into<String>,
        exchange: impl Into<String>,
        queue: impl Into<String>,
        routing_key: impl Into<String>,
    ) -> Self {
        Self {
            source_queue: source_queue.into(),
            exchange: exchange.into(),
            queue: queue.into(),
            routing_key: routing_key.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyDefinition {
    exchanges: Vec<ExchangeSpec>,
    queues: Vec<QueueDefinition>,
    bindings: Vec<BindingSpec>,
    dead_letters: Vec<DeadLetterDefinition>,
}

impl TopologyDefinition {
    #[must_use]
    pub const fn new(
        exchanges: Vec<ExchangeSpec>,
        queues: Vec<QueueDefinition>,
        bindings: Vec<BindingSpec>,
    ) -> Self {
        Self {
            exchanges,
            queues,
            bindings,
            dead_letters: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_dead_letter(mut self, dead_letter: DeadLetterDefinition) -> Self {
        self.dead_letters.push(dead_letter);
        self
    }
}

/// Immutable, validated sequence of topology operations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyPlan {
    mode: TopologyMode,
    exchanges: Vec<ExchangeSpec>,
    queues: Vec<QueueSpec>,
    bindings: Vec<BindingSpec>,
}

impl TopologyPlan {
    /// Compiles topology configuration before any broker I/O.
    ///
    /// # Errors
    ///
    /// Returns a permanent error for invalid queue combinations or references.
    pub fn compile(
        mode: TopologyMode,
        definition: TopologyDefinition,
    ) -> Result<Self, TopologyPlanError> {
        let mut exchanges = definition.exchanges;
        let mut queues = definition
            .queues
            .into_iter()
            .map(QueueDefinition::compile)
            .collect::<Result<Vec<_>, _>>()?;
        let mut bindings = definition.bindings;

        let mut seen_exchanges: Vec<String> = Vec::new();
        let mut seen_dlqs: Vec<String> = Vec::new();
        for dead_letter in definition.dead_letters {
            let source = queues
                .iter_mut()
                .find(|queue| queue.name == dead_letter.source_queue)
                .ok_or_else(|| {
                    TopologyPlanError::new(format!(
                        "dead-letter source queue '{}' is not defined",
                        dead_letter.source_queue
                    ))
                })?;
            source.dead_letter_exchange = Some(dead_letter.exchange.clone());
            source.dead_letter_routing_key = Some(dead_letter.routing_key.clone());
            if !seen_exchanges.contains(&dead_letter.exchange) {
                exchanges.push(ExchangeSpec {
                    name: dead_letter.exchange.clone(),
                    kind: ExchangeKind::Direct,
                    durable: true,
                    auto_delete: false,
                    internal: false,
                    arguments: Headers::new(),
                });
                seen_exchanges.push(dead_letter.exchange.clone());
            }
            // The DLQ queue is declared once per queue name, but every
            // (queue, exchange, routing_key) triple gets its own binding:
            // per-source routing keys differ between subscriptions sharing a
            // DLQ, and the DLX republish is not mandatory — a missing binding
            // silently drops dead-lettered messages.
            if !seen_dlqs.contains(&dead_letter.queue) {
                queues.push(QueueDefinition::new(dead_letter.queue.clone()).compile()?);
                seen_dlqs.push(dead_letter.queue.clone());
            }
            let binding = BindingSpec {
                queue: dead_letter.queue.clone(),
                exchange: dead_letter.exchange.clone(),
                routing_key: dead_letter.routing_key.clone(),
            };
            if !bindings.contains(&binding) {
                bindings.push(binding);
            }
        }

        Ok(Self {
            mode,
            exchanges,
            queues,
            bindings,
        })
    }

    #[must_use]
    pub const fn mode(&self) -> TopologyMode {
        self.mode
    }

    #[must_use]
    pub fn exchanges(&self) -> &[ExchangeSpec] {
        &self.exchanges
    }

    #[must_use]
    pub fn queues(&self) -> &[QueueSpec] {
        &self.queues
    }

    #[must_use]
    pub fn bindings(&self) -> &[BindingSpec] {
        &self.bindings
    }

    /// Compiles the topology plan a pool applies for a validated
    /// configuration.
    ///
    /// Declares one queue per subscription plus the dead-letter topology when
    /// enabled. In declare mode with a plugin-routed delay strategy (plugin
    /// or auto mode), the `rabbit-rs.delayed` exchange and a queue-name-keyed
    /// binding for every subscription queue join the plan so delayed delivery
    /// works without operator-provisioned bindings (issue #97). Custom route
    /// exchanges publish delayed messages through `{exchange}.delayed`, whose
    /// bindings stay an infrastructure contract.
    ///
    /// A definition that fails compilation falls back to an empty
    /// external-mode plan; the pool stays connectable and the invalid
    /// combination surfaces as a configuration error at pool creation.
    ///
    /// # Panics
    ///
    /// Never panics: the external-mode fallback compiles by construction.
    #[must_use]
    pub fn from_config(config: &ValidatedConfig) -> Self {
        let queue_type = config.queue_type();
        let queue_durable = config.queue_durable();
        let subscriptions: Vec<_> = config
            .worker_profiles()
            .iter()
            .flat_map(|worker| &worker.subscriptions)
            .collect();
        let queues = subscriptions
            .iter()
            .map(|sub| {
                let mut qd = QueueDefinition::new(&sub.queue)
                    .kind(queue_type)
                    .durable(queue_durable);
                if let Some(limit) = config.delivery_limit() {
                    qd = qd.delivery_limit(limit);
                }
                qd
            })
            .collect();

        let mut exchanges = Vec::new();
        let mut bindings = Vec::new();
        if matches!(config.topology_mode(), TopologyMode::Declare)
            && matches!(DelayStrategy::compile(config), DelayStrategy::Plugin)
        {
            exchanges.push(delayed_exchange_spec(DELAYED_EXCHANGE_NAME));
            bindings.extend(subscriptions.iter().map(|sub| BindingSpec {
                queue: sub.queue.clone(),
                exchange: DELAYED_EXCHANGE_NAME.to_owned(),
                routing_key: sub.queue.clone(),
            }));
        }

        let mut topology = TopologyDefinition::new(exchanges, queues, bindings);
        if let Some(dl) = config.dead_letter()
            && dl.enabled
        {
            for sub in subscriptions {
                let routing_key = dl.routing_key.clone().unwrap_or_else(|| sub.queue.clone());
                topology = topology.with_dead_letter(DeadLetterDefinition::new(
                    sub.queue.clone(),
                    dl.exchange.clone(),
                    dl.queue.clone(),
                    routing_key,
                ));
            }
        }

        Self::compile(config.topology_mode(), topology).unwrap_or_else(|_error| {
            Self::compile(
                TopologyMode::External,
                TopologyDefinition::new(vec![], vec![], vec![]),
            )
            .expect("external mode always compiles")
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyPlanError {
    message: String,
}

impl TopologyPlanError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for TopologyPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for TopologyPlanError {}
