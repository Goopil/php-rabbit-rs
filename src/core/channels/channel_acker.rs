use crate::core::channels::channel::AmqpChannel;
use crate::core::runtime::RUNTIME;
use anyhow::Error;
use lapin::options::{BasicAckOptions, BasicNackOptions, BasicRejectOptions};

impl AmqpChannel {
    pub fn simple_consume_ack_by_tag_flags(
        &self,
        delivery_tag: u64,
        multiple: bool,
    ) -> anyhow::Result<()> {
        self.ensure_open()?;
        {
            let unacked = self.pending_unacked.read();
            if multiple {
                let has_any = unacked.iter().any(|t| *t <= delivery_tag);
                if !has_any {
                    anyhow::bail!("unknown delivery tag");
                }
            } else if !unacked.contains(&delivery_tag) {
                anyhow::bail!("unknown delivery tag");
            }
        }

        let ch = self.clone_channel();
        RUNTIME.block_on(async move {
            ch.basic_ack(delivery_tag, BasicAckOptions { multiple })
                .await?;
            Ok::<(), Error>(())
        })?;
        // Remove from pending_unacked
        {
            let mut set = self.pending_unacked.write();
            if multiple {
                // remove all tags <= delivery_tag
                let to_remove: Vec<u64> =
                    set.iter().copied().filter(|t| *t <= delivery_tag).collect();
                for t in to_remove {
                    set.remove(&t);
                }
            } else {
                set.remove(&delivery_tag);
            }
        }
        Ok(())
    }

    pub fn simple_consume_nack_by_tag_flags(
        &self,
        delivery_tag: u64,
        requeue: bool,
        multiple: bool,
    ) -> anyhow::Result<()> {
        self.ensure_open()?;

        {
            let unacked = self.pending_unacked.read();
            if multiple {
                let has_any = unacked.iter().any(|t| *t <= delivery_tag);
                if !has_any {
                    anyhow::bail!("unknown delivery tag");
                }
            } else if !unacked.contains(&delivery_tag) {
                anyhow::bail!("unknown delivery tag");
            }
        }

        let ch = self.clone_channel();
        RUNTIME.block_on(async move {
            ch.basic_nack(delivery_tag, BasicNackOptions { requeue, multiple })
                .await?;
            Ok::<(), Error>(())
        })?;
        // Remove from pending_unacked
        {
            let mut set = self.pending_unacked.write();
            if multiple {
                let to_remove: Vec<u64> =
                    set.iter().copied().filter(|t| *t <= delivery_tag).collect();
                for t in to_remove {
                    set.remove(&t);
                }
            } else {
                set.remove(&delivery_tag);
            }
        }
        Ok(())
    }

    pub fn simple_consume_reject_by_tag(
        &self,
        delivery_tag: u64,
        requeue: bool,
    ) -> anyhow::Result<()> {
        self.ensure_open()?;

        {
            let unacked = self.pending_unacked.read();
            if !unacked.contains(&delivery_tag) {
                anyhow::bail!("unknown delivery tag");
            }
        }

        let ch = self.clone_channel();
        RUNTIME.block_on(async move {
            ch.basic_reject(delivery_tag, BasicRejectOptions { requeue })
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
        // Remove from pending_unacked
        {
            let mut set = self.pending_unacked.write();
            set.remove(&delivery_tag);
        }
        Ok(())
    }

    pub fn simple_consume_ack_by_tag(&self, tag: u64) -> anyhow::Result<()> {
        let ch = self.clone_channel();
        RUNTIME.block_on(async move {
            ch.basic_ack(tag, lapin::options::BasicAckOptions { multiple: false })
                .await?;
            Ok::<(), anyhow::Error>(())
        })?;
        {
            self.pending_unacked.write().remove(&tag);
        }
        Ok(())
    }
    pub fn simple_consume_nack_by_tag(&self, tag: u64, requeue: bool) -> anyhow::Result<()> {
        let ch = self.clone_channel();
        RUNTIME.block_on(async move {
            ch.basic_nack(
                tag,
                BasicNackOptions {
                    multiple: false,
                    requeue,
                },
            )
            .await?;
            Ok::<(), anyhow::Error>(())
        })?;
        {
            self.pending_unacked.write().remove(&tag);
        }
        Ok(())
    }
}
