use crate::core::channels::channel::AmqpChannel;
use crate::core::runtime::RUNTIME;
use anyhow::{Error, Result};
use lapin::options::{
    QueueBindOptions, QueueDeclareOptions, QueueDeleteOptions, QueuePurgeOptions,
};
use lapin::types::FieldTable;

impl AmqpChannel {
    pub fn queue_declare(
        &self,
        name: &str,
        opts: QueueDeclareOptions,
        args: FieldTable,
    ) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            ch.queue_declare(name, opts, args).await?;
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("queue.declare failed: {e}"));
        }
        res
    }

    pub fn queue_bind(
        &self,
        queue: &str,
        exchange: &str,
        routing_key: &str,
        opts: QueueBindOptions,
        args: FieldTable,
    ) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            ch.queue_bind(queue, exchange, routing_key, opts, args)
                .await?;
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("queue.bind failed: {e}"));
        }
        res
    }

    pub fn queue_unbind(
        &self,
        queue: &str,
        exchange: &str,
        routing_key: &str,
        args: FieldTable,
    ) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            ch.queue_unbind(queue, exchange, routing_key, args).await?;
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("queue.unbind failed: {e}"));
        }
        res
    }

    pub fn queue_purge(&self, name: &str, opts: QueuePurgeOptions) -> Result<()> {
        self.ensure_open()?;
        let publisher = self.publisher();
        let _ = RUNTIME.block_on(async move { publisher.flush_fire_and_forget().await });
        let ch = self.clone_channel();
        let name = name.to_string();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            let _ok = ch.queue_purge(&name, opts).await?; // ignore le count
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("queue.purge failed: {e}"));
        }
        res
    }

    pub fn queue_delete(&self, name: &str, opts: QueueDeleteOptions) -> Result<()> {
        self.ensure_open()?;
        let ch = self.clone_channel();
        let name = name.to_string();
        let precheck_if_empty = opts.if_empty;

        let res: Result<(), Error> = RUNTIME.block_on(async move {
            if precheck_if_empty {
                // passive declare pour lire message_count
                match ch.queue_declare(
                    &name,
                    QueueDeclareOptions { passive: true, ..Default::default() },
                    FieldTable::default(),
                ).await {
                    Ok(state) => {
                       if state.message_count() > 0 {
                            return Err(anyhow::anyhow!(
                                "PRECONDITION_FAILED - inequivalent arg 'if-empty' for queue.delete (queue not empty)"
                            ));
                        }
                    }
                    Err(e) => {
                        tracing::debug!("queue.delete precheck passive failed: {e}");
                    }
                }
            }

            let _ok = ch.queue_delete(&name, opts).await?;
            Ok(())
        });

        if let Err(ref e) = res {
            self.set_closed_with_error(format!("queue.delete failed: {e}"));
        }
        res
    }
}
