use crate::core::channels::channel_publisher::Publisher;
use crate::core::channels::message::{CoreDelivery, CoreMessage};
use crate::core::connections::connection_pool::ConnectionEntry;
use crate::core::runtime::RUNTIME;
use anyhow::{Error, Result};
use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, TryRecvError};
use futures::StreamExt;
use lapin::{
    options::{BasicConsumeOptions, BasicPublishOptions, BasicQosOptions, QueueDeclareOptions},
    types::FieldTable,
    Channel,
};
use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::task::JoinHandle;

#[derive(Copy, Clone, Debug, Default)]
pub struct ConsumePolicy {
    pub no_ack: bool,
    pub reject_on_exception: bool,
    pub requeue_on_reject: bool,
}

pub struct AmqpChannel {
    // Keep a handle to the owning connection entry so we can recreate channels after reconnecting
    entry: Arc<ConnectionEntry>,
    // Hot-swappable sender used by the forwarding task; never None once a consumer is started
    inbox_tx: Arc<RwLock<Option<FlumeSender<CoreDelivery>>>>,
    // Remember the last simple consumer queue to be able to resume it transparently
    consume_queue: RwLock<Option<String>>,
    // Capacity for the bounded inbox (flume) used by simple consumer; tied to QoS prefetch
    inbox_capacity: AtomicUsize,
    ch: RwLock<Arc<Channel>>,
    cached_id: AtomicU16,
    // Hot-swappable receiver seen by PHP wait/poll
    inbox_rx: RwLock<Option<Arc<FlumeReceiver<CoreDelivery>>>>,
    consumer_task: RwLock<Option<JoinHandle<()>>>,
    consumer_tag: RwLock<Option<String>>,
    consume_flags: RwLock<(bool, bool, bool)>, // (no_local, exclusive, nowait)
    consume_policy: RwLock<ConsumePolicy>,
    closed: AtomicBool,
    last_error: RwLock<Option<String>>,
    pub(crate) pending_unacked: Arc<RwLock<HashSet<u64>>>,
    publisher: RwLock<Option<Arc<Publisher>>>,
}

impl Drop for AmqpChannel {
    fn drop(&mut self) {
        // Stop consumer task if any
        if let Some(handle) = self.consumer_task.write().take() {
            handle.abort();
        }
        // Clear inbox endpoints so the forwarders no longer have a target
        *self.inbox_tx.write() = None;
        *self.inbox_rx.write() = None;
        // Drop cached publisher; dropping it tears down the pump/driver
        *self.publisher.write() = None;
    }
}

impl AmqpChannel {
    pub async fn open(entry: Arc<ConnectionEntry>) -> Result<Self> {
        let ch = entry.connection().create_channel().await?;

        Ok(Self {
            entry,
            ch: RwLock::new(Arc::new(ch)),
            cached_id: AtomicU16::new(0),
            inbox_tx: Arc::new(RwLock::new(None)),
            inbox_rx: RwLock::new(None),
            consumer_task: RwLock::new(None),
            consumer_tag: RwLock::new(None),
            consume_queue: RwLock::new(None),
            inbox_capacity: AtomicUsize::new(1024),
            consume_flags: RwLock::new((false, false, false)),
            consume_policy: RwLock::new(ConsumePolicy::default()),
            closed: AtomicBool::new(false),
            last_error: RwLock::new(None),
            pending_unacked: Arc::new(RwLock::new(HashSet::new())),
            publisher: RwLock::new(None),
        })
    }
    // --- New setters/getters for consumption error policy ---
    /// Returns Err if the channel is closed, using the last error if set.
    #[inline]
    pub(crate) fn ensure_open(&self) -> Result<()> {
        // If the broker has closed the underlying AMQP channel, behave like amqplib: next op throws
        if let Some(p) = self.publisher.read().as_ref().cloned() {
            if p.is_channel_closed() {
                // Mark locally as closed so later calls also fail deterministically
                self.closed.store(true, Ordering::SeqCst);
                let mut le = self.last_error.write();
                if le.is_none() {
                    *le = Some("AMQP channel closed by broker".to_string());
                }
                anyhow::bail!(le.clone().unwrap());
            }
        }
        // If the underlying lapin::Channel is already closed, surface the error now
        if !self.ch.read().status().connected() {
            self.closed.store(true, Ordering::SeqCst);
            let mut le = self.last_error.write();
            if le.is_none() {
                *le = Some("AMQP channel closed by broker".to_string());
            }
            anyhow::bail!(le.clone().unwrap());
        }
        if self.closed.load(Ordering::SeqCst) {
            if let Some(msg) = self.last_error.read().clone() {
                anyhow::bail!(msg);
            } else {
                anyhow::bail!("channel closed");
            }
        }
        Ok(())
    }

    #[inline]
    pub fn consume_policy(&self) -> ConsumePolicy {
        *self.consume_policy.read()
    }

    /// Get the per-channel Publisher (owned), lazily created and cached.
    #[inline]
    pub fn publisher(&self) -> Arc<Publisher> {
        if let Some(p) = self.publisher.read().as_ref().cloned() {
            return p;
        }
        let ch_arc = self.ch.read().clone();
        let p = Arc::new(Publisher::new_with_channel(self.entry.clone(), ch_arc));
        *self.publisher.write() = Some(p.clone());
        p
    }

    /// Enable or disable publisher confirms on this channel's Publisher.
    /// Replaces the cached Publisher to apply the new mode.
    pub fn enable_confirms(&self, enable: bool) -> Result<()> {
        self.ensure_open()?;
        // Build a fresh Publisher with the desired mode, then swap it into the cache
        let ch_arc = self.ch.read().clone();
        let mut new_pub = Publisher::new_with_channel(self.entry.clone(), ch_arc);
        new_pub.enable_confirms(enable);
        let arc = Arc::new(new_pub);
        *self.publisher.write() = Some(arc);
        Ok(())
    }

    /// Wait until all messages published so far are confirmed (blocking wrapper).
    pub fn wait_for_confirms(&self) -> Result<()> {
        self.ensure_open()?;
        let publisher = match self.publisher.read().as_ref().cloned() {
            Some(p) => p,
            None => {
                // No publisher yet: nothing to wait for
                return Ok(());
            }
        };

        RUNTIME.block_on(async move { publisher.wait_confirm_all().await })?;
        Ok(())
    }

    /// Wait with timeout (milliseconds) for all pending confirmations.
    pub fn wait_for_confirms_timeout(&self, timeout_ms: u64) -> Result<()> {
        self.ensure_open()?;
        let publisher = match self.publisher.read().as_ref().cloned() {
            Some(p) => p,
            None => {
                // No publisher yet: nothing to wait for
                return Ok(());
            }
        };
        RUNTIME.block_on(async move { publisher.wait_confirm_timeout(timeout_ms).await })?;
        Ok(())
    }

    #[inline]
    pub fn id(&self) -> u16 {
        let cached = self.cached_id.load(Ordering::Relaxed);
        if cached != 0 {
            return cached;
        }
        let id_now = self.ch.read().id();
        self.cached_id.store(id_now, Ordering::Relaxed);
        id_now
    }

    #[inline]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    #[inline]
    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().clone()
    }

    pub(crate) fn set_closed_with_error<S: Into<String>>(&self, msg: S) {
        self.closed.store(true, Ordering::SeqCst);
        *self.last_error.write() = Some(msg.into());
    }

    pub(crate) fn clone_channel(&self) -> Arc<Channel> {
        // If we've been closed (or the publisher observed a broker close), do not auto-recreate.
        if self.closed.load(Ordering::SeqCst) {
            return self.ch.read().clone();
        }
        if let Some(p) = self.publisher.read().as_ref().cloned() {
            if p.is_channel_closed() {
                return self.ch.read().clone();
            }
        }
        // Fast path
        let current = self.ch.read().clone();
        if current.status().connected() {
            return current;
        }

        // Slow path: try to recreate from (possibly) reconnected Connection
        let new_ch = RUNTIME.block_on(async {
            let conn = self.entry.connection();
            conn.create_channel().await
        });

        match new_ch {
            Ok(ch) => {
                let arc = Arc::new(ch);
                *self.ch.write() = arc.clone();
                // Invalidate cached publisher; it must bind to the new channel on next use
                *self.publisher.write() = None;
                // id changed, invalidate cache (re-read on next id())
                self.cached_id.store(0, Ordering::Relaxed);
                arc
            }
            Err(_) => {
                // Let the following operation report the error
                current
            }
        }
    }

    pub fn qos(&self, prefetch_count: u16, global: bool) -> Result<()> {
        self.ensure_open()?;
        // Tie the bounded inbox capacity to the QoS prefetch. Use at least 1.
        let cap = prefetch_count.max(1) as usize;
        self.inbox_capacity.store(cap, Ordering::SeqCst);
        // Hot-resize the current inbox if a consumer is active
        self.hot_resize_inbox(cap);
        let ch = self.clone_channel();

        RUNTIME.block_on(async move {
            ch.basic_qos(prefetch_count, BasicQosOptions { global })
                .await?;
            Ok(())
        })
    }

    /// Fetch one message synchronously from a queue (basic.get).
    /// Returns Ok (Some(CoreDelivery)) if a message was available, Ok (None) if empty, or Err on error.
    pub fn basic_get(&self, queue: &str, no_ack: bool) -> Result<Option<CoreDelivery>> {
        self.ensure_open()?;

        let queue = queue.to_string();
        let ch = self.clone_channel();
        let publisher = self.publisher();
        let pending = self.pending_unacked.clone();

        let res: Result<Option<CoreDelivery>, Error> = RUNTIME.block_on(async move {
            publisher
                .flush_fire_and_forget()
                .await
                .map_err(|e| anyhow::anyhow!(e))?;

            match ch
                .basic_get(&queue, lapin::options::BasicGetOptions { no_ack })
                .await?
            {
                Some(get) => {
                    // Extract the underlying delivery to build our CoreDelivery
                    let del = get.delivery;
                    let tag = del.delivery_tag;
                    let core = CoreDelivery::from_lapin(del);
                    // Track unacked deliveries only in manual-ack mode
                    if !no_ack {
                        let mut set = pending.write();
                        set.insert(tag);
                    }
                    Ok(Some(core))
                }
                None => Ok(None),
            }
        });

        if let Err(ref e) = res {
            // On NOT_FOUND or protocol error, the broker may close the channel
            self.set_closed_with_error(format!("basic.get failed: {e}"));
        }
        res
    }

    #[inline]
    pub fn consumer_tag(&self) -> Option<String> {
        self.consumer_tag.read().clone()
    }

    pub fn flush_ff_publishes(&self) -> Result<()> {
        let publisher = self.publisher();
        match RUNTIME.block_on(async move { publisher.flush_fire_and_forget().await }) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.set_closed_with_error(format!("fire-and-forget flush failed: {e}"));
                Err(e)
            }
        }
    }

    pub fn set_consume_policy(&self, policy: ConsumePolicy) {
        *self.consume_policy.write() = policy;
    }

    pub fn basic_publish_with_options(
        &self,
        exchange: &str,
        routing_key: &str,
        message: &CoreMessage,
        pub_opts: BasicPublishOptions,
    ) -> Result<()> {
        self.ensure_open()?;
        let publisher = self.publisher();

        // Match php-amqplib: when mandatory=true on the default exchange, ensure the queue exists.
        if pub_opts.mandatory && exchange.is_empty() {
            let q = routing_key.to_string();
            let ch = self.clone_channel();
            let exists = RUNTIME.block_on(async move {
                ch.queue_declare(
                    &q,
                    QueueDeclareOptions {
                        passive: true,
                        ..Default::default()
                    },
                    FieldTable::default(),
                )
                .await
            });
            if exists.is_err() {
                anyhow::bail!("basic.return: unroutable (mandatory)");
            }
        }

        let res: Result<(), Error> = RUNTIME.block_on(async move {
            publisher
                .basic_publish_with_options(exchange, routing_key, message, pub_opts)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        });

        if let Err(ref e) = res {
            self.set_closed_with_error(format!("basic.publish failed: {e}"));
        }
        res
    }

    pub fn close(&self) -> Result<()> {
        if self.closed.swap(true, Ordering::SeqCst) {
            // Already closed; be idempotent
            return Ok(());
        }
        self.pending_unacked.write().clear();

        let ch = self.clone_channel();

        // Best-effort: cancel active simple consumer and stop the task
        if let Some(tag) = self.consumer_tag.write().take() {
            let _ = RUNTIME.block_on(async {
                // Ignore errors during the shutdown path
                let _ = ch.basic_cancel(&tag, Default::default()).await;
                Ok::<(), Error>(())
            });
        }
        if let Some(handle) = self.consumer_task.write().take() {
            handle.abort();
        }

        RUNTIME.block_on(async move { ch.close(200, "Bye").await })?;
        Ok(())
    }

    pub fn simple_consume_start(
        &self,
        queue: &str,
        no_local: bool,
        exclusive: bool,
        nowait: bool,
        consumer_tag: Option<String>,
    ) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        if self.consumer_tag.read().is_some() {
            anyhow::bail!("simple consumer already active on this channel");
        }
        let queue = queue.to_string();

        let cap = self.inbox_capacity.load(Ordering::SeqCst);
        let (tx, rx): (FlumeSender<CoreDelivery>, FlumeReceiver<CoreDelivery>) =
            flume::bounded(cap);
        {
            *self.inbox_rx.write() = Some(Arc::new(rx));
            *self.inbox_tx.write() = Some(tx.clone());
            *self.consume_queue.write() = Some(queue.clone());
        }

        let no_ack_flag = self.consume_policy.read().no_ack;
        *self.consume_flags.write() = (no_local, exclusive, nowait);
        let inbox_tx = self.inbox_tx.clone();
        let pending = self.pending_unacked.clone();
        let handle = RUNTIME.block_on(async {
            let ctag = consumer_tag
                .unwrap_or_else(|| format!("simple-consumer-{}-{}", std::process::id(), ch.id()));
            match ch
                .basic_consume(
                    &queue,
                    &ctag,
                    BasicConsumeOptions {
                        no_local,
                        no_ack: no_ack_flag,
                        exclusive,
                        nowait,
                    },
                    FieldTable::default(),
                )
                .await
            {
                Ok(mut consumer) => {
                    let join: JoinHandle<()> = tokio::spawn(async move {
                        while let Some(delivery) = consumer.next().await {
                            match delivery {
                                Ok(del) => {
                                    let tag = del.delivery_tag;
                                    let core = CoreDelivery::from_lapin(del);
                                    // track unacked only in manual-ack mode
                                    if !no_ack_flag {
                                        {
                                            let mut set = pending.write();
                                            set.insert(tag);
                                        }
                                    }
                                    let tx_opt = { inbox_tx.read().as_ref().cloned() };
                                    if let Some(tx_cur) = tx_opt {
                                        let _ = tx_cur.send_async(core).await;
                                    } else {
                                        // no receiver registered; drop silently
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    });

                    *self.consumer_tag.write() = Some(ctag.clone());
                    Ok::<JoinHandle<()>, Error>(join)
                }
                Err(err) => {
                    self.closed.store(true, Ordering::SeqCst);
                    self.set_closed_with_error(format!("basic.consume failed: {err}"));
                    Err(err.into())
                }
            }
        })?;

        *self.consumer_task.write() = Some(handle);
        Ok(())
    }

    /// Explicitly cancel the simple consumer, if active.
    pub fn simple_consume_cancel(&self, tag: Option<String>) -> Result<()> {
        let ch = self.clone_channel();
        // If the caller gave a tag, use it; otherwise use our active one (without clearing yet)
        let cancel_tag = match tag {
            Some(t) => t,
            None => match self.consumer_tag.read().clone() {
                Some(t) => t,
                None => return Ok(()), // nothing to cancel
            },
        };

        // Send basic.cancel and wait for the broker to confirm/cut the stream
        RUNTIME.block_on(async move {
            ch.basic_cancel(&cancel_tag, Default::default()).await?;
            Ok::<(), Error>(())
        })?;

        // Abort forwarding task (it should naturally end after cancel but be safe)
        if let Some(handle) = self.consumer_task.write().take() {
            handle.abort();
        }

        let ch2 = self.clone_channel();
        let _ = RUNTIME.block_on(async move {
            // Best-effort: if the broker already cleaned up, this can be a no-op
            let _ = ch2
                .basic_recover(lapin::options::BasicRecoverOptions { requeue: true })
                .await;
            Ok::<(), Error>(())
        });

        // Now clear local consumer state
        *self.consumer_tag.write() = None;
        *self.consume_queue.write() = None;
        *self.inbox_tx.write() = None;
        *self.inbox_rx.write() = None;
        // Pending unacked deliveries were requeued by the broker on cancel; drop our local view
        self.pending_unacked.write().clear();

        Ok(())
    }

    fn ensure_consumer_resumed(&self) {
        // Only if we had started a consumer
        let queue = match self.consume_queue.read().clone() {
            Some(q) => q,
            None => return,
        };

        // If the task is still present, nothing to do
        if self.consumer_task.read().is_some() {
            return;
        }

        // Ensure we have a live channel
        let ch = self.clone_channel();
        let inbox_tx = self.inbox_tx.clone();
        let no_ack_flag = self.consume_policy.read().no_ack;
        let (no_local, exclusive, nowait) = *self.consume_flags.read();
        let existing_tag = self.consumer_tag.read().clone();

        let _ = RUNTIME.block_on(async move {
            let ctag = existing_tag
                .unwrap_or_else(|| format!("simple-consumer-{}-{}", std::process::id(), ch.id()));
            match ch
                .basic_consume(
                    &queue,
                    &ctag,
                    BasicConsumeOptions {
                        no_local,
                        no_ack: no_ack_flag,
                        exclusive,
                        nowait,
                    },
                    FieldTable::default(),
                )
                .await
            {
                Ok(mut consumer) => {
                    let join: JoinHandle<()> = tokio::spawn(async move {
                        while let Some(delivery) = consumer.next().await {
                            match delivery {
                                Ok(del) => {
                                    let core = CoreDelivery::from_lapin(del);
                                    // clone current sender outside awaits to avoid holding the read guard across .await
                                    let tx_opt = { inbox_tx.read().as_ref().cloned() };
                                    if let Some(tx_cur) = tx_opt {
                                        let _ = tx_cur.send_async(core).await;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    });
                    *self.consumer_tag.write() = Some(ctag);
                    *self.consumer_task.write() = Some(join);

                    Ok::<(), Error>(())
                }
                Err(_) => Ok::<(), Error>(()),
            }
        });
    }

    /// Swap the inbox buffer capacity at runtime without losing messages.
    /// If no consumer is active, it only updates the capacity for the next start.
    fn hot_resize_inbox(&self, new_cap: usize) {
        // If no consumer task is running, just store cap and return
        if self.consumer_task.read().is_none() {
            return;
        }
        // Create a new bounded channel
        let (new_tx, new_rx) = flume::bounded::<CoreDelivery>(new_cap.max(1));
        // Swap receiver seen by PHP
        let old_rx_opt = {
            let mut w = self.inbox_rx.write();
            let old = w.take();
            *w = Some(Arc::new(new_rx));
            old
        };
        // Point the forwarding task to the new sender
        *self.inbox_tx.write() = Some(new_tx.clone());

        // Migrate any items that were already queued in the old receiver
        if let Some(old_rx) = old_rx_opt {
            // Drain quickly without blocking
            while let Ok(item) = old_rx.try_recv() {
                let _ = new_tx.send(item);
            }
        }
    }

    pub fn simple_consume_poll_batch(
        &self,
        timeout_ms: u64,
        max: usize,
    ) -> Result<Vec<CoreDelivery>> {
        self.ensure_consumer_resumed();
        use std::time::{Duration, Instant};
        if max == 0 {
            return Ok(Vec::new());
        }

        self.flush_ff_publishes()?;

        let rx_arc = match self.inbox_rx.read().as_ref().cloned() {
            Some(rx) => rx,
            None => return Ok(Vec::new()),
        };

        let mut out = Vec::with_capacity(max.min(64));
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);

        // Initial non-blocking pre-drain
        while out.len() < max {
            match rx_arc.try_recv() {
                Ok(d) => out.push(d),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        if out.len() >= max || timeout_ms == 0 {
            return Ok(out);
        }

        // Loop until the deadline with short pauses so messages can arrive
        loop {
            // Fast drain
            while out.len() < max {
                match rx_arc.try_recv() {
                    Ok(d) => out.push(d),
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
                }
            }
            if out.len() >= max {
                break;
            }

            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;
            let remaining_ms = remaining.as_millis() as u64;
            if remaining_ms == 0 {
                break;
            }

            // Blocking wait with timeout
            match rx_arc.recv_timeout(Duration::from_millis(remaining_ms.max(1))) {
                Ok(d) => {
                    out.push(d);
                    if out.len() >= max {
                        break;
                    }
                }
                Err(flume::RecvTimeoutError::Timeout) => {
                    // Allow another fast-drain pass before respecting the timeout so any late
                    // arrivals queued locally are observed.
                    continue;
                }
                Err(flume::RecvTimeoutError::Disconnected) => break,
            }

            // Reduce sleep time for better responsiveness
            std::thread::sleep(Duration::from_millis(1));
        }

        // One last non-blocking sweep to catch messages that arrived between the timeout firing
        // and the loop exit above.
        while out.len() < max {
            match rx_arc.try_recv() {
                Ok(d) => out.push(d),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }

        Ok(out)
    }

    pub fn simple_consume_wait(&self, timeout_ms: i64, max: i64) -> Result<usize> {
        if timeout_ms < 0 || max <= 0 {
            return Ok(0);
        }
        self.ensure_consumer_resumed();
        let max = max as usize;
        let rx_arc = match self.inbox_rx.read().as_ref().cloned() {
            Some(rx) => rx,
            None => return Ok(0),
        };

        self.flush_ff_publishes()?;

        let mut count = 0usize;
        let start = std::time::Instant::now();

        // Non-blocking pre-drain
        while count < max {
            match rx_arc.try_recv() {
                Ok(_payload) => {
                    count += 1;
                    if count >= max {
                        return Ok(count);
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }

        while count < max {
            // Quick non-blocking attempt
            match rx_arc.try_recv() {
                Ok(_payload) => {
                    count += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }

            // Compute remaining time (floor to 1 ms when positive)
            let elapsed = start.elapsed();
            let remaining_ms = if (timeout_ms as u128) > elapsed.as_millis() {
                ((timeout_ms as u128 - elapsed.as_millis()) as u64).max(1)
            } else {
                0
            };
            if remaining_ms == 0 {
                break;
            }

            match rx_arc.recv_timeout(std::time::Duration::from_millis(remaining_ms)) {
                Ok(_payload) => {
                    count += 1;
                }
                Err(flume::RecvTimeoutError::Timeout)
                | Err(flume::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(count)
    }
}
