use crate::core::channels::message::CoreMessage;
use crate::core::connections::connection_pool::ConnectionEntry;
use crate::core::runtime::RUNTIME;
use flume::{Receiver as FlumeReceiver, Sender as FlumeSender};
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use lapin::options::BasicPublishOptions;
use lapin::publisher_confirm::Confirmation;
use lapin::Channel;
use parking_lot::RwLock;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::Duration;

type ConfirmTask = Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

#[derive(Clone, Copy, Debug)]
pub enum PublishMode {
    /// Do not enable publisher confirms; align with amqplib/bunny defaults
    FireAndForget,
    /// Enable publisher confirms and await acks/nacks
    Confirms,
}

struct PublishJob {
    exchange: Arc<str>,
    routing_key: Arc<str>,
    body: Arc<Vec<u8>>,
    props: lapin::BasicProperties,
    opts: BasicPublishOptions,
    // barrier for flushing the FF pump; when set, this job is not published
    barrier_tx: Option<oneshot::Sender<()>>,
}

struct ConfirmTracker {
    published: AtomicU64,
    confirmed: AtomicU64,
    had_nack: AtomicBool,
    notify: Notify,
}

impl ConfirmTracker {
    fn new() -> Self {
        Self {
            published: AtomicU64::new(0),
            confirmed: AtomicU64::new(0),
            had_nack: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }
}

pub struct Publisher {
    entry: Arc<ConnectionEntry>,
    channel: RwLock<Option<Arc<Channel>>>,
    mode: PublishMode,
    tx: RwLock<Option<FlumeSender<PublishJob>>>,
    pump: RwLock<Option<JoinHandle<()>>>,
    /// Max number of in-flight publish ops inside the pump before we await completions
    inflight_limit: usize,
    confirm: RwLock<Option<Arc<ConfirmTracker>>>,
    /// Concurrency limiter for Confirms (bypasses flume pump)
    confirm_sem: RwLock<Option<Arc<Semaphore>>>,
    /// Driver for Confirms: pushes futures to be polled without spawning per message
    confirm_drv_tx: RwLock<Option<FlumeSender<ConfirmTask>>>,
    confirm_drv: RwLock<Option<JoinHandle<()>>>,
    recent_close: Arc<AtomicBool>,
}

impl Publisher {
    /// Create a Publisher in Fire-and-Forget mode (default; like amqplib/bunny)
    pub fn new(entry: Arc<ConnectionEntry>) -> Self {
        Self {
            entry,
            channel: RwLock::new(None),
            mode: PublishMode::FireAndForget,
            tx: RwLock::new(None),
            pump: RwLock::new(None),
            inflight_limit: 2048, // bounds memory & pressure
            confirm: RwLock::new(None),
            confirm_sem: RwLock::new(None),
            confirm_drv_tx: RwLock::new(None),
            confirm_drv: RwLock::new(None),
            recent_close: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Create a Publisher bound to an existing lapin::Channel (used to share the same channel as AmqpChannel)
    pub fn new_with_channel(entry: Arc<ConnectionEntry>, channel: Arc<Channel>) -> Self {
        let me = Self::new(entry);
        *me.channel.write() = Some(channel.clone());
        me
    }

    // Helper to detect if any Basic.Return was received via wait_confirms with timeout.
    async fn saw_mandatory_return_short(&self, ch: &Channel, timeout_ms: u64) -> bool {
        // We only need to know if any BasicReturnMessage arrived; details are not required for the test
        match tokio::time::timeout(Duration::from_millis(timeout_ms), ch.wait_for_confirms()).await
        {
            Ok(Ok(returns)) => {
                // `returns` is expected to be a collection of BasicReturnMessage
                // Treat non-empty as unroutable (mandatory)
                !returns.is_empty()
            }
            _ => false,
        }
    }

    pub fn enable_confirms(&mut self, enable: bool) {
        self.mode = if enable {
            let sem = Arc::new(Semaphore::new(self.inflight_limit.max(128)));
            *self.confirm_sem.write() = Some(sem);
            // Start confirms driver lazily; actual start happens on first publish too
            PublishMode::Confirms
        } else {
            *self.confirm_sem.write() = None;
            // Drop the confirms driver channel so the task can exit
            *self.confirm_drv_tx.write() = None;
            PublishMode::FireAndForget
        };
    }

    #[inline(always)]
    pub fn confirms_enabled(&self) -> bool {
        matches!(self.mode, PublishMode::Confirms)
    }

    #[inline]
    pub fn is_channel_closed(&self) -> bool {
        if self.recent_close.load(Ordering::SeqCst) {
            return true;
        }
        if let Some(ch) = self.channel.read().as_ref() {
            return !ch.status().connected();
        }
        false
    }

    /// Set the maximum number of in-flight publish operations. If confirms are enabled,
    /// the internal semaphore is recreated with the new capacity.
    pub fn set_inflight_limit(&mut self, n: usize) {
        let n = n.max(128);
        self.inflight_limit = n;
        if self.confirms_enabled() {
            *self.confirm_sem.write() = Some(Arc::new(Semaphore::new(n)));
        }
    }

    fn start_confirms_driver_if_needed(&self) {
        if self.confirm_drv.read().is_some() {
            return;
        }
        let inflight_cap = self.inflight_limit.max(128);
        let (tx, rx): (FlumeSender<ConfirmTask>, FlumeReceiver<ConfirmTask>) =
            flume::bounded(inflight_cap * 2);
        *self.confirm_drv_tx.write() = Some(tx);
        let handle = RUNTIME.spawn(async move {
            let mut inflight: FuturesUnordered<ConfirmTask> = FuturesUnordered::new();
            loop {
                tokio::select! {
                    biased;
                    Some(_) = inflight.next(), if !inflight.is_empty() => { /* progress */ }
                    maybe_task = rx.recv_async(), if inflight.len() < inflight_cap => {
                        match maybe_task {
                            Ok(task) => {
                                inflight.push(task);
                                while let Some(_) = inflight.next().now_or_never().flatten() {}
                            }
                            Err(_) => { break; }
                        }
                    }
                    else => { break; }
                }
            }
            while inflight.next().await.is_some() {}
        });
        *self.confirm_drv.write() = Some(handle);
    }

    fn confirm_tracker(&self) -> Arc<ConfirmTracker> {
        if let Some(c) = self.confirm.read().as_ref().cloned() {
            return c;
        }
        let c = Arc::new(ConfirmTracker::new());
        *self.confirm.write() = Some(c.clone());
        c
    }

    #[inline]
    fn confirm_semaphore(&self) -> Arc<Semaphore> {
        if let Some(s) = self.confirm_sem.read().as_ref().cloned() {
            return s;
        }
        let sem = Arc::new(Semaphore::new(self.inflight_limit.max(128)));
        *self.confirm_sem.write() = Some(sem.clone());
        sem
    }

    /// Wait until all messages published so far (at call time) are confirmed.
    pub async fn wait_confirm_all(&self) -> anyhow::Result<()> {
        if !self.confirms_enabled() {
            return Ok(());
        }
        let tracker = self.confirm_tracker();
        let watermark = tracker.published.load(Ordering::Relaxed);
        loop {
            if tracker.confirmed.load(Ordering::Relaxed) >= watermark {
                break;
            }
            tracker.notify.notified().await;
        }
        if tracker.had_nack.load(Ordering::Relaxed) {
            anyhow::bail!("NACK seen while waiting for confirms");
        }
        Ok(())
    }

    /// Wait with a timeout (milliseconds). Returns Err on timeout or if a NACK occurred.
    pub async fn wait_confirm_timeout(&self, timeout_ms: u64) -> anyhow::Result<()> {
        if !self.confirms_enabled() {
            return Ok(());
        }
        let tracker = self.confirm_tracker();
        let watermark = tracker.published.load(Ordering::Relaxed);
        let fut = async {
            loop {
                if tracker.confirmed.load(Ordering::Relaxed) >= watermark {
                    break Ok(());
                }
                tracker.notify.notified().await;
            }
        };
        match tokio::time::timeout(Duration::from_millis(timeout_ms), fut).await {
            Ok(Ok(())) => {
                if tracker.had_nack.load(Ordering::Relaxed) {
                    anyhow::bail!("NACK seen while waiting for confirms");
                }
                Ok(())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => anyhow::bail!("wait_confirm timeout"),
        }
    }

    /// Publish helper for Fire-and-Forget mode (no confirm awaited)
    #[inline(always)]
    async fn do_publish_ff(ch: Arc<Channel>, job: PublishJob) {
        let _ = ch
            .basic_publish(
                &*job.exchange,
                &*job.routing_key,
                job.opts,
                &*job.body,
                job.props,
            )
            .await;
    }

    /// Publish helper for Confirms mode (awaits confirmation and maps NACK to error)
    #[inline(always)]
    async fn do_publish_confirms(ch: Arc<Channel>, job: PublishJob) -> anyhow::Result<()> {
        let confirm = ch
            .basic_publish(
                &*job.exchange,
                &*job.routing_key,
                job.opts,
                &*job.body,
                job.props,
            )
            .await?;

        let confirm = confirm.await?;
        match confirm {
            Confirmation::Ack(_) | Confirmation::NotRequested => Ok(()),
            Confirmation::Nack(_) => anyhow::bail!("Publish NACKed by broker"),
        }
    }

    /// (Re)ensure a connected channel with retry/backoff. Static helper to avoid borrowing `self` across the pump task.
    async fn ensure_channel(
        ch_opt: &mut Option<Arc<Channel>>,
        entry: &Arc<ConnectionEntry>,
        mode: PublishMode,
    ) -> anyhow::Result<Arc<Channel>> {
        use rand::Rng;
        use tokio::time::{sleep, Duration};

        if let Some(cur) = ch_opt.as_ref() {
            if cur.status().connected() {
                return Ok(cur.clone());
            }
        }

        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match entry.connection().create_channel().await {
                Ok(fresh) => {
                    if matches!(mode, PublishMode::Confirms) {
                        if let Err(_) = fresh
                            .confirm_select(lapin::options::ConfirmSelectOptions::default())
                            .await
                        {
                            // failed confirm_select, retry with backoff
                            let delay = (10u64 * (1u64 << attempt.min(6))).min(1000)
                                + rand::rng().random_range(0..30);
                            sleep(Duration::from_millis(delay)).await;
                            continue;
                        }
                    }
                    let arc = Arc::new(fresh);
                    *ch_opt = Some(arc.clone());
                    return Ok(arc);
                }
                Err(_) => {
                    // retry with exponential backoff + jitter
                    let delay = (10u64 * (1u64 << attempt.min(6))).min(1000)
                        + rand::rng().random_range(0..30);
                    sleep(Duration::from_millis(delay)).await;
                    continue;
                }
            }
        }
    }

    async fn flush_before_sync_op(&self) -> anyhow::Result<()> {
        if !matches!(self.mode, PublishMode::FireAndForget) {
            return Ok(());
        }
        // IMPORTANT: start the pump before sending the barrier,
        // otherwise tx == None and the flush becomes a no-op.
        self.start_pump_if_needed();

        let tx_opt = self.tx.read().as_ref().cloned();
        if tx_opt.is_none() {
            return Ok(());
        }
        let tx = tx_opt.unwrap();
        let (btx, brx) = oneshot::channel();
        let barrier = PublishJob {
            exchange: Arc::<str>::from(""),
            routing_key: Arc::<str>::from(""),
            body: Arc::new(Vec::new()),
            props: lapin::BasicProperties::default(),
            opts: BasicPublishOptions::default(),
            barrier_tx: Some(btx),
        };
        tx.send_async(barrier)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let _ = brx.await.map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(())
    }

    pub async fn flush_fire_and_forget(&self) -> anyhow::Result<()> {
        self.flush_before_sync_op().await
    }

    fn start_pump_if_needed(&self) {
        if self.pump.read().is_some() {
            return;
        }

        let inflight_cap = self.inflight_limit.max(128);
        let queue_cap = (inflight_cap * 2).max(256).min(65_536);
        let (tx, rx): (FlumeSender<PublishJob>, FlumeReceiver<PublishJob>) =
            flume::bounded(queue_cap);

        *self.tx.write() = Some(tx);

        let entry_weak = Arc::downgrade(&self.entry);
        let mode = self.mode;
        // Prefer using the channel already bound to this Publisher (same as AmqpChannel)
        let init_ch = self.channel.read().as_ref().cloned();
        let confirm_tracker = if matches!(mode, PublishMode::Confirms) {
            Some(self.confirm_tracker())
        } else {
            None
        };

        let recent_close_flag = self.recent_close.clone();
        let handle = RUNTIME.spawn(async move {
            let mut ch_opt: Option<Arc<Channel>> = init_ch;
            let mut inflight: FuturesUnordered<Pin<Box<dyn std::future::Future<Output=()> + Send>>> = FuturesUnordered::new();

            loop {
                // Upgrade the weak reference; if the client/handles are gone, exit the pump
                let Some(entry) = entry_weak.upgrade() else { return; };
                tokio::select! {
                    // 1) Drain futures that are still in flight (gives network/IO time to progress)
                    biased;
                    Some(_) = inflight.next(), if !inflight.is_empty() => { /* progress */ }

                    // 2) Pull a job when we're under the concurrency bound
                    maybe_job = rx.recv_async(), if inflight.len() < inflight_cap => {
                        match maybe_job {
                            Ok(job) => {
                                if let Some(tx) = job.barrier_tx {
                                    // Drain all inflight publish tasks to ensure previous publishes are flushed
                                    while inflight.next().await.is_some() {}

                                    // After draining, if the current channel has been closed by the broker, flag it
                                    if let Some(ref ch_seen) = ch_opt {
                                        if !ch_seen.status().connected() {
                                            recent_close_flag.store(true, Ordering::SeqCst);
                                        }
                                    }

                                    let _ = tx.send(());
                                    continue;
                                }

                                let ch = match Self::ensure_channel(&mut ch_opt, &entry, mode).await {
                                    Ok(c) => c,
                                    Err(_) => { ch_opt = None; continue; }
                                };

                                if matches!(mode, PublishMode::FireAndForget) {
                                    let recent_close_flag = recent_close_flag.clone();
                                    inflight.push(Box::pin(async move {
                                        Publisher::do_publish_ff(ch.clone(), job).await;
                                        // If the broker sent Channel.Close, Lapin flags the channel as closed immediately
                                        if !ch.status().connected() {
                                            recent_close_flag.store(true, Ordering::SeqCst);
                                        }
                                    }));

                                } else {
                                    let tracker = match &confirm_tracker { Some(t) => t.clone(), None => { ch_opt = None; continue; } };
                                    let _after = tracker.published.fetch_add(1, Ordering::Relaxed) + 1;
                                    inflight.push(Box::pin(async move {
                                        let res = async {
                                            let c = ch
                                                .basic_publish(&*job.exchange, &*job.routing_key, job.opts, &*job.body, job.props)
                                                .await
                                                .map_err(|_| ())?;
                                            let c = c.await.map_err(|_| ())?;
                                            match c {
                                                Confirmation::Ack(_) | Confirmation::NotRequested => Ok::<(), ()>(()),
                                                Confirmation::Nack(_) => Err(()),
                                            }
                                        }.await;

                                        if res.is_err() { tracker.had_nack.store(true, Ordering::Relaxed); }
                                        let _after = tracker.confirmed.fetch_add(1, Ordering::Relaxed) + 1;
                                        tracker.notify.notify_waiters();
                                    }));
                                }

                                // Non-blocking drain to advance futures that are ready to complete
                                while let Some(_) = inflight.next().now_or_never().flatten() {}
                            }
                            Err(_) => { break; }
                        }
                    }

                    // 3) Nothing else to do and no inflight publishes -> exit
                    else => { break; }
                }
            }

            // Drain remaining inflight publishes
            while inflight.next().await.is_some() {}
            if let Some(tr) = &confirm_tracker { tr.notify.notify_waiters(); }
        });

        *self.pump.write() = Some(handle);
    }

    #[inline]
    async fn get_or_open_channel(&self) -> anyhow::Result<Arc<Channel>> {
        if let Some(cur) = self.channel.read().as_ref().cloned() {
            if cur.status().connected() {
                return Ok(cur);
            }
        }

        let fresh = self
            .entry
            .connection()
            .create_channel()
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        if matches!(self.mode, PublishMode::Confirms) {
            fresh
                .confirm_select(lapin::options::ConfirmSelectOptions::default())
                .await
                .map_err(|e| anyhow::anyhow!(e))?;
        }
        let arc = Arc::new(fresh);
        *self.channel.write() = Some(arc.clone());

        Ok(arc)
    }

    /// Publish a message with the provided options.
    ///
    /// This is the canonical publishing path; `AmqpChannel::basic_publish_with_options`
    /// delegates to this to avoid duplicate logic.
    #[inline(always)]
    async fn basic_publish_with_options_async(
        &self,
        exchange: &str,
        routing_key: &str,
        message: &CoreMessage,
        pub_opts: BasicPublishOptions,
    ) -> anyhow::Result<()> {
        let props = if message.properties.is_default() {
            lapin::BasicProperties::default()
        } else {
            message.properties.to_lapin()
        };

        // Mandatory preflight for default exchange (amqplib parity)
        if pub_opts.mandatory && exchange.is_empty() {
            if let Ok(ch) = self.get_or_open_channel().await {
                if let Err(_) = ch
                    .queue_declare(
                        routing_key,
                        lapin::options::QueueDeclareOptions {
                            passive: true,
                            ..Default::default()
                        },
                        lapin::types::FieldTable::default(),
                    )
                    .await
                {
                    anyhow::bail!("basic.return: unroutable (mandatory)");
                }
            }
        }

        let job = PublishJob {
            exchange: Arc::<str>::from(exchange),
            routing_key: Arc::<str>::from(routing_key),
            body: message.body.clone(),
            props,
            opts: pub_opts,
            barrier_tx: None,
        };

        if matches!(self.mode, PublishMode::FireAndForget) {
            // For mandatory publishes, we need to detect errors synchronously even in FireAndForget mode
            if pub_opts.mandatory {
                let ch = self.get_or_open_channel().await?;
                // Perform the publish synchronously to detect channel errors (e.g., non-existent exchange)
                let confirm = ch
                    .basic_publish(
                        &job.exchange,
                        &job.routing_key,
                        job.opts,
                        &job.body,
                        job.props,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Publish failed: {e}"))?;

                // Check for Basic.Return messages
                match confirm.await {
                    Ok(Confirmation::Ack(returned)) | Ok(Confirmation::Nack(returned)) => {
                        if let Some(ret) = returned {
                            anyhow::bail!("basic.return: {}: {}", ret.reply_code, ret.reply_text);
                        }
                        // If no returned message, the publish was successful but unroutable
                        anyhow::bail!("basic.return: unroutable (mandatory)");
                    }
                    Ok(Confirmation::NotRequested) => {
                        // This shouldn't happen since we're requesting confirmation for mandatory publishes
                        anyhow::bail!("basic.return: unroutable (mandatory)");
                    }
                    Err(e) => Err(anyhow::anyhow!("Publish failed: {e}"))?,
                }
            } else {
                // Standard FireAndForget behavior for non-mandatory publishes
                self.start_pump_if_needed();

                let tx = self
                    .tx
                    .read()
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("publisher pump missing"))?;

                tx.send_async(job)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            }
            return Ok(());
        }

        let ch = self.get_or_open_channel().await?;
        let tracker = self.confirm_tracker();
        let sem = self.confirm_semaphore();
        // Ensure the confirms driver is running
        self.start_confirms_driver_if_needed();

        // If mandatory, publish synchronously (bypass driver) to ensure ordering and short-window return visibility
        if pub_opts.mandatory {
            let _ = tracker.published.fetch_add(1, Ordering::Relaxed) + 1;
            let job_immediate = PublishJob {
                exchange: Arc::<str>::from(exchange),
                routing_key: Arc::<str>::from(routing_key),
                body: message.body.clone(),
                props: job.props.clone(),
                opts: pub_opts,
                barrier_tx: None,
            };
            // Await confirms inline
            Publisher::do_publish_confirms(ch.clone(), job_immediate).await?;
            let _ = tracker.confirmed.fetch_add(1, Ordering::Relaxed) + 1;
            tracker.notify.notify_waiters();
            // After successful publish, wait briefly for potential Basic.Return
            if self.saw_mandatory_return_short(ch.as_ref(), 200).await {
                anyhow::bail!("basic.return: unroutable (mandatory)");
            }
            return Ok(());
        }

        let tx = self
            .confirm_drv_tx
            .read()
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("confirms driver missing"))?;
        let permit: OwnedSemaphorePermit = sem
            .acquire_owned()
            .await
            .map_err(|_| anyhow::anyhow!("semaphore closed"))?;
        let _ = tracker.published.fetch_add(1, Ordering::Relaxed) + 1;

        // Build the confirm future (no spawn): it will be polled by the confirms driver
        let task: ConfirmTask = Box::pin(async move {
            let _ = Publisher::do_publish_confirms(ch, job).await;
            let _ = tracker.confirmed.fetch_add(1, Ordering::Relaxed) + 1;
            tracker.notify.notify_waiters();
            drop(permit);
        });

        tx.send_async(task)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(())
    }

    pub async fn basic_publish_with_options(
        &self,
        exchange: &str,
        routing_key: &str,
        message: &CoreMessage,
        pub_opts: BasicPublishOptions,
    ) -> anyhow::Result<()> {
        if !pub_opts.immediate {
            return self
                .basic_publish_with_options_async(exchange, routing_key, message, pub_opts)
                .await;
        }

        let props = if message.properties.is_default() {
            lapin::BasicProperties::default()
        } else {
            message.properties.to_lapin()
        };

        // Immediate path: bypass the pump (flush before returning)
        let ch = self.get_or_open_channel().await?;

        if !self.confirms_enabled() {
            let _ = ch
                .basic_publish(exchange, routing_key, pub_opts, &*message.body, props)
                .await?;
            if pub_opts.mandatory {
                if self.saw_mandatory_return_short(ch.as_ref(), 200).await {
                    anyhow::bail!("basic.return: unroutable (mandatory)");
                }
            }
            return Ok(());
        }

        // Confirms enabled: publish and wait for Ack/Nack synchronously
        let tracker = self.confirm_tracker();
        let _ = tracker.published.fetch_add(1, Ordering::Relaxed) + 1;

        let job = PublishJob {
            exchange: std::sync::Arc::<str>::from(exchange),
            routing_key: std::sync::Arc::<str>::from(routing_key),
            body: message.body.clone(),
            props,
            opts: pub_opts,
            barrier_tx: None,
        };

        match Publisher::do_publish_confirms(ch.clone(), job).await {
            Ok(()) => {
                let _ = tracker.confirmed.fetch_add(1, Ordering::Relaxed) + 1;
                tracker.notify.notify_waiters();
                if pub_opts.mandatory {
                    if self.saw_mandatory_return_short(ch.as_ref(), 200).await {
                        anyhow::bail!("basic.return: unroutable (mandatory)");
                    }
                }
                Ok(())
            }
            Err(e) => {
                tracker.had_nack.store(true, Ordering::Relaxed);
                tracker.notify.notify_waiters();
                Err(e)
            }
        }
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        // Close the publish queue so the pump can exit
        *self.tx.write() = None;
        // Abort the pump task if still running
        if let Some(handle) = self.pump.write().take() {
            handle.abort();
        }
        // Stop the confirms driver if any
        *self.confirm_drv_tx.write() = None;
        if let Some(handle) = self.confirm_drv.write().take() {
            handle.abort();
        }
    }
}
