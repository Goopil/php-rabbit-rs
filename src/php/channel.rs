use crate::core::channels::channel::{AmqpChannel, ConsumePolicy};
use crate::php::auto_qos::{AutoQosConfig, AutoQosState};
use crate::php::delivery::AmqpDelivery;
use crate::php::message::AmqpMessage;
use crate::php::parsers::{
    parse_consume_flags, parse_consume_policy_options, parse_exchange_bind_options,
    parse_exchange_declare_kind, parse_exchange_declare_options, parse_exchange_delete_options,
    parse_exchange_unbind_options, parse_publish_options, parse_qos_options,
    parse_queue_bind_options, parse_queue_declare_options, parse_queue_delete_options,
    parse_queue_purge_options, parse_queue_unbind_args,
};
use ext_php_rs::convert::{IntoZval, IntoZvalDyn};
use ext_php_rs::prelude::{PhpException, PhpResult};
#[allow(unused_imports)]
use ext_php_rs::props::Prop;
use ext_php_rs::types::{Iterable, ZendCallable, Zval};
use ext_php_rs::{php_class, php_impl};
use parking_lot::RwLock;
use std::time::{Duration, Instant};

#[inline]
fn php_safe<S: AsRef<str>>(s: S) -> String {
    s.as_ref().replace('\0', "\\0")
}

#[php_class]
#[php(name = "Goopil\\RabbitRs\\PhpChannel")]
pub struct PhpChannel {
    pub inner: AmqpChannel,
    pub callback: RwLock<Option<ZendCallable<'static>>>,
    auto_qos: RwLock<Option<AutoQosState>>,
}

#[php_impl]
impl PhpChannel {
    pub fn get_id(&self) -> PhpResult<i64> {
        Ok(self.inner.id() as i64)
    }

    pub fn qos(&self, prefetch_count: i64, options: Option<Iterable>) -> PhpResult<bool> {
        if prefetch_count < 0 || prefetch_count > u16::MAX as i64 {
            return Err(PhpException::default("Invalid prefetch_count".into()));
        }

        let qos = parse_qos_options(options);

        self.inner
            .qos(prefetch_count as u16, qos.global)
            .map_err(|e| PhpException::default(php_safe(format!("QoS failed: {e}"))))?;

        Ok(true)
    }

    pub fn queue_declare(&self, name: String, options: Option<Iterable>) -> PhpResult<bool> {
        let (opts, args) = parse_queue_declare_options(options);

        self.inner
            .queue_declare(&name, opts, args)
            .map_err(|e| PhpException::default(php_safe(format!("Queue declare failed: {e}"))))?;

        Ok(true)
    }

    pub fn queue_bind(
        &self,
        queue: String,
        exchange: String,
        routing_key: Option<String>,
        options: Option<Iterable>,
    ) -> PhpResult<bool> {
        let (opts, args) = parse_queue_bind_options(options);
        let rk = routing_key.unwrap_or_default();

        self.inner
            .queue_bind(&queue, &exchange, &rk, opts, args)
            .map_err(|e| PhpException::default(php_safe(format!("Queue bind failed: {e}"))))?;

        Ok(true)
    }

    pub fn queue_unbind(
        &self,
        queue: String,
        exchange: String,
        routing_key: Option<String>,
        options: Option<Iterable>,
    ) -> PhpResult<bool> {
        let args = parse_queue_unbind_args(options);
        let rk = routing_key.unwrap_or_default();

        self.inner
            .queue_unbind(&queue, &exchange, &rk, args)
            .map_err(|e| PhpException::default(php_safe(format!("Queue unbind failed: {e}"))))?;

        Ok(true)
    }

    pub fn queue_purge(&self, name: String, options: Option<Iterable>) -> PhpResult<bool> {
        let opts = parse_queue_purge_options(options);

        self.inner
            .queue_purge(&name, opts)
            .map_err(|e| PhpException::default(php_safe(format!("Queue purge failed: {e}"))))?;

        Ok(true)
    }

    pub fn queue_delete(&self, name: String, options: Option<Iterable>) -> PhpResult<bool> {
        let opts = parse_queue_delete_options(options);

        self.inner
            .queue_delete(&name, opts)
            .map_err(|e| PhpException::default(php_safe(format!("Queue delete failed: {e}"))))?;

        Ok(true)
    }

    pub fn exchange_declare(
        &self,
        name: String,
        exchange_type: String,
        options: Option<Iterable>,
    ) -> PhpResult<bool> {
        let (opts, args) = parse_exchange_declare_options(options);
        let exchange_kind = parse_exchange_declare_kind(exchange_type)?;

        self.inner
            .exchange_declare(&name, exchange_kind, opts, args)
            .map_err(|e| {
                PhpException::default(php_safe(format!("Exchange declare failed: {e}")))
            })?;

        Ok(true)
    }

    pub fn exchange_delete(&self, name: String, options: Option<Iterable>) -> PhpResult<bool> {
        let opts = parse_exchange_delete_options(options);

        self.inner
            .exchange_delete(&name, opts)
            .map_err(|e| PhpException::default(php_safe(format!("Exchange delete failed: {e}"))))?;

        Ok(true)
    }

    /// Bind destination exchange to source exchange with an optional routing key.
    /// Options: nowait (bool), arguments (FieldTable)
    pub fn exchange_bind(
        &self,
        destination: String,
        source: String,
        routing_key: Option<String>,
        options: Option<Iterable>,
    ) -> PhpResult<bool> {
        let (opts, args) = parse_exchange_bind_options(options);
        let rk = routing_key.unwrap_or_default();

        self.inner
            .exchange_bind(&destination, &source, &rk, opts, args)
            .map_err(|e| PhpException::default(php_safe(format!("Exchange bind failed: {e}"))))?;

        Ok(true)
    }

    /// Unbind destination exchange from source exchange.
    /// Options: nowait (bool), arguments (FieldTable)
    pub fn exchange_unbind(
        &self,
        destination: String,
        source: String,
        routing_key: Option<String>,
        options: Option<Iterable>,
    ) -> PhpResult<bool> {
        let (opts, args) = parse_exchange_unbind_options(options);
        let rk = routing_key.unwrap_or_default();

        self.inner
            .exchange_unbind(&destination, &source, &rk, opts, args)
            .map_err(|e| PhpException::default(php_safe(format!("Exchange unbind failed: {e}"))))?;

        Ok(true)
    }

    /// Fetch one message synchronously from a queue (basic.get).
    /// Options:
    /// - no_ack: bool (default true). When false, the returned delivery must be acked/nacked.
    /// Returns: AmqpDelivery or null if the queue is empty.
    pub fn basic_get(
        &self,
        queue: String,
        options: Option<Iterable>,
    ) -> PhpResult<Option<AmqpDelivery>> {
        let no_ack = parse_consume_policy_options(&options).no_ack;

        let got = self
            .inner
            .basic_get(&queue, no_ack)
            .map_err(|e| PhpException::default(php_safe(format!("basicGet failed: {e}"))))?;

        match got {
            Some(core_delivery) => Ok(Some(AmqpDelivery::new(core_delivery, no_ack))),
            None => Ok(None),
        }
    }

    pub fn basic_publish(
        &self,
        exchange: String,
        routing_key: String,
        message: &AmqpMessage,
        options: Option<Iterable>,
    ) -> PhpResult<bool> {
        let opts = parse_publish_options(options);

        self.inner
            .basic_publish_with_options(&exchange, &routing_key, &message.inner, opts)
            .map_err(|e| PhpException::default(php_safe(format!("Publish failed: {e}"))))?;

        Ok(true)
    }

    /// Put this channel into Publisher Confirms mode (php-amqplib compatible API).
    /// The `nowait` parameter is accepted for API compatibility but ignored; enabling is lazy.
    #[allow(unused_variables)]
    pub fn confirm_select(&self, nowait: Option<bool>) -> PhpResult<bool> {
        self.inner
            .enable_confirms(true)
            .map_err(|e| PhpException::default(php_safe(format!("confirm_select failed: {e}"))))?;
        Ok(true)
    }

    /// Wait until all pending publishes at the moment of the call are confirmed.
    /// Returns true on success; throws on internal errors.
    pub fn wait_for_confirms(&self) -> PhpResult<bool> {
        self.inner.wait_for_confirms().map_err(|e| {
            PhpException::default(php_safe(format!("wait_for_confirms failed: {e}")))
        })?;
        Ok(true)
    }

    /// Wait until all pending publishes are confirmed, or throw if timeout / NACK occurs.
    /// Argument is a timeout in milliseconds (optional). If null, waits without timeout.
    pub fn wait_for_confirms_or_die(&self, timeout_ms: Option<i64>) -> PhpResult<bool> {
        match timeout_ms {
            Some(ms) if ms > 0 => {
                self.inner
                    .wait_for_confirms_timeout(ms as u64)
                    .map_err(|e| {
                        PhpException::default(php_safe(format!(
                            "wait_for_confirms_or_die timeout: {e}"
                        )))
                    })?;
            }
            _ => {
                self.inner.wait_for_confirms().map_err(|e| {
                    PhpException::default(php_safe(format!("wait_for_confirms_or_die failed: {e}")))
                })?;
            }
        }
        Ok(true)
    }

    pub fn close(&self) -> PhpResult<bool> {
        // If the broker already closed the channel with an error (e.g., NOT_FOUND),
        // decide whether to surface it or tolerate close() depending on the op.
        if self.inner.is_closed() {
            if let Some(msg) = self.inner.last_error() {
                let ml = msg.to_lowercase();
                // Tests expect: after a failed basic.consume on an auto-deleted queue,
                // calling close() should be tolerated (no throw).
                // But after passive is declared failures, close() should throw.
                let tolerate = ml.contains("basic.consume failed");
                if tolerate {
                    return Ok(true);
                }

                return Err(PhpException::default(php_safe(msg)));
            }

            // already closed cleanly -> idempotent OK
            return Ok(true);
        }

        self.inner
            .close()
            .map_err(|e| PhpException::default(php_safe(format!("Channel close failed: {e}"))))?;
        Ok(true)
    }

    /// Start a simple consumer on a queue.
    /// Options:
    /// - no_ack: bool (default true) -> auto-ack mode when true (messages acknowledged automatically)
    ///   When reject_on_exception=true and no_ack is not provided, no_ack is forced to false to allow NACKs (amqplib-compatible).
    /// - reject_on_exception: bool (default false) -> when true, NACK on callback exception
    /// - requeue_on_reject: bool (default false) -> controls NACK requeue behavior
    /// - auto_qos: { enabled, min, max, step_up, step_down, cooldown_ms, target }
    pub fn simple_consume(
        &self,
        queue: String,
        callback: &Zval,
        options: Option<Iterable>,
    ) -> PhpResult<String> {
        let callable = ZendCallable::new_owned(callback.as_zval(false).unwrap())
            .map_err(|e| PhpException::default(format!("Invalid callback: {e}")))?;
        {
            let mut slot = self.callback.write();
            *slot = Some(callable);
        }

        let policy = parse_consume_policy_options(&options);
        self.inner.set_consume_policy(ConsumePolicy {
            no_ack: policy.no_ack,
            reject_on_exception: policy.reject_on_exception,
            requeue_on_reject: policy.requeue_on_reject,
        });

        // Parse consume flags via options_parser
        let flags = parse_consume_flags(&options);

        let cos_config = AutoQosConfig::from_iterable(options);
        *self.auto_qos.write() = if cos_config.is_enabled() {
            Some(AutoQosState::new(cos_config, |start| {
                self.inner
                    .qos(start, false)
                    .map_err(|e| PhpException::default(php_safe(format!("QoS(auto) failed: {e}"))))
            })?)
        } else {
            None
        };

        self.inner
            .simple_consume_start(
                &queue,
                flags.no_local,
                flags.exclusive,
                flags.nowait,
                flags.consumer_tag,
            )
            .map_err(|e| PhpException::default(php_safe(format!("simpleConsume failed: {e}"))))?;

        Ok(self.inner.consumer_tag().unwrap())
    }

    pub fn basic_ack(&self, delivery_tag: i64, multiple: Option<bool>) -> PhpResult<bool> {
        let multi = multiple.unwrap_or(false);
        let tag = if delivery_tag < 0 {
            0
        } else {
            delivery_tag as u64
        };

        match self.inner.simple_consume_ack_by_tag_flags(tag, multi) {
            Ok(()) => Ok(true),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unknown delivery tag") {
                    return Ok(false);
                }
                Err(PhpException::default(php_safe(format!(
                    "basicAck failed: {msg}"
                ))))
            }
        }
    }

    pub fn basic_nack(
        &self,
        delivery_tag: i64,
        requeue: bool,
        multiple: Option<bool>,
    ) -> PhpResult<bool> {
        let multi = multiple.unwrap_or(false);
        let tag = if delivery_tag < 0 {
            0
        } else {
            delivery_tag as u64
        };

        match self
            .inner
            .simple_consume_nack_by_tag_flags(tag, requeue, multi)
        {
            Ok(()) => Ok(true),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unknown delivery tag") {
                    return Ok(false);
                }
                Err(PhpException::default(php_safe(format!(
                    "basicNack failed: {msg}"
                ))))
            }
        }
    }

    pub fn basic_reject(&self, delivery_tag: i64, requeue: bool) -> PhpResult<bool> {
        let tag = if delivery_tag < 0 {
            0
        } else {
            delivery_tag as u64
        };

        match self.inner.simple_consume_reject_by_tag(tag, requeue) {
            Ok(()) => Ok(true),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("unknown delivery tag") {
                    return Ok(false);
                }
                Err(PhpException::default(php_safe(format!(
                    "basicNack failed: {msg}"
                ))))
            }
        }
    }

    pub fn basic_cancel(&self, tag: Option<String>) -> PhpResult<bool> {
        self.inner
            .simple_consume_cancel(tag)
            .map_err(|e| PhpException::default(php_safe(format!("basicCancel failed: {e}"))))?;

        Ok(true)
    }

    pub fn wait(&self, timeout_ms: i64, max: i64) -> PhpResult<i64> {
        if self.inner.is_closed() {
            // Optional: to be extra thorough you could check for pending deliveries before returning.
            // For NOT_FOUND scenarios there is nothing waiting to be drained.
            let msg = self
                .inner
                .last_error()
                .unwrap_or_else(|| "Channel closed".to_string());
            return Err(PhpException::default(php_safe(msg)));
        }

        if timeout_ms < 0 || max <= 0 {
            return Ok(0);
        }

        // Ensure any recent fire-and-forget publishes are flushed so the consumer can see them
        self.inner
            .flush_ff_publishes()
            .map_err(|e| PhpException::default(php_safe(format!("wait flush failed: {e}"))))?;

        let cb_guard = self.callback.read();
        let cb = cb_guard.as_ref().ok_or_else(|| {
            PhpException::default(
                "No callback registered. Call simpleConsume(queue, callback) first.".into(),
            )
        })?;

        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        let mut count: i64 = 0;

        // Continue looping until we've received all expected messages or timeout
        while count < max {
            let now = Instant::now();
            if now >= deadline {
                break;
            }

            let remaining_ms = (deadline - now).as_millis() as u64;
            let want = (max - count) as usize;

            // Use a minimum timeout to ensure we don't miss messages
            let effective_timeout = remaining_ms.max(10);

            let batch = self
                .inner
                .simple_consume_poll_batch(effective_timeout, want)
                .map_err(|e| PhpException::default(php_safe(format!("wait failed: {e}"))))?;

            if batch.is_empty() {
                // Even if we got no messages, continue looping until timeout
                // This gives more time for messages to arrive
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }

            if let Some(ref mut st) = *self.auto_qos.write() {
                st.record_and_maybe_adjust(batch.len(), |next| {
                    self.inner.qos(next, false).map_err(|e| {
                        PhpException::default(php_safe(format!("Auto-QoS adjust failed: {e}")))
                    })
                })?;
            }

            for delivery in batch {
                let tag = delivery.delivery_tag;
                let policy = self.inner.consume_policy();
                let amqp_delivery = AmqpDelivery::new(delivery.clone(), policy.no_ack);

                match Self::call_php_callback(cb, amqp_delivery) {
                    Ok(()) => {}
                    Err(e) => {
                        if !policy.no_ack {
                            if policy.reject_on_exception {
                                let _ = self
                                    .inner
                                    .simple_consume_nack_by_tag(tag, policy.requeue_on_reject);
                            }

                            return Err(e);
                        }

                        // Auto-ack mode (no_ack=true):
                        // Swallow ONLY the specific misuse where the callback tries $d->ack() / $d->nack() / $d->ack_on() / $d->nack_on()
                        // despite auto-ack being active. All other exceptions still propagate.
                        let error_message = format!("{:?}", e).to_lowercase();
                        if error_message.contains("cannot ack in auto-ack mode")
                            || error_message.contains("cannot nack in auto-ack mode")
                            || error_message.contains("cannot reject in auto-ack mode")
                        {
                            continue;
                        };

                        return Err(e);
                    }
                }
                count += 1;
                if count >= max {
                    break;
                }
            }
        }

        Ok(count)
    }
}

impl PhpChannel {
    pub fn new(inner: AmqpChannel) -> Self {
        Self {
            inner,
            callback: RwLock::new(None),
            auto_qos: RwLock::new(None),
        }
    }

    #[inline]
    fn call_php_callback(cb: &ZendCallable<'static>, delivery: AmqpDelivery) -> PhpResult<()> {
        let arg = delivery.into_zval(false)?;

        cb.try_call(vec![&arg])
            .map_err(|e| PhpException::default(php_safe(format!("callback call failed: {e}"))))?;

        Ok(())
    }
}

impl Drop for PhpChannel {
    fn drop(&mut self) {
        let _ = self.inner.close(); // ignore error if already closed
        *self.callback.write() = None; // drop PHP callable
        *self.auto_qos.write() = None; // drop AutoQoS state
    }
}
