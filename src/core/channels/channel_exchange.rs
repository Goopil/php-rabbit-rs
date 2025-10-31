use crate::core::channels::channel::AmqpChannel;
use crate::core::runtime::RUNTIME;
use anyhow::{Error, Result};
use lapin::options::{
    ExchangeBindOptions, ExchangeDeclareOptions, ExchangeDeleteOptions, ExchangeUnbindOptions,
};
use lapin::types::FieldTable;
use lapin::ExchangeKind;

impl AmqpChannel {
    pub fn exchange_declare(
        &self,
        name: &str,
        kind: ExchangeKind,
        opts: ExchangeDeclareOptions,
        args: FieldTable,
    ) -> Result<()> {
        self.flush_ff_publishes()?;
        self.ensure_open()?;
        let ch = self.clone_channel();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            ch.exchange_declare(name, kind, opts, args).await?;
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("exchange.declare failed: {e}"));
        }
        res
    }

    pub fn exchange_delete(&self, name: &str, opts: ExchangeDeleteOptions) -> Result<()> {
        self.flush_ff_publishes()?;
        self.ensure_open()?;
        let ch = self.clone_channel();
        let name = name.to_string();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            ch.exchange_delete(&name, opts).await?;
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("exchange.delete failed: {e}"));
        }
        res
    }

    pub fn exchange_bind(
        &self,
        destination: &str,
        source: &str,
        routing_key: &str,
        opts: ExchangeBindOptions,
        args: FieldTable,
    ) -> Result<()> {
        self.flush_ff_publishes()?;
        self.ensure_open()?;
        let ch = self.clone_channel();
        let dest = destination.to_string();
        let src = source.to_string();
        let rk = routing_key.to_string();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            ch.exchange_bind(&dest, &src, &rk, opts, args).await?;
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("exchange.bind failed: {e}"));
        }
        res
    }

    pub fn exchange_unbind(
        &self,
        destination: &str,
        source: &str,
        routing_key: &str,
        opts: ExchangeUnbindOptions,
        args: FieldTable,
    ) -> Result<()> {
        self.flush_ff_publishes()?;
        self.ensure_open()?;
        let ch = self.clone_channel();
        let dest = destination.to_string();
        let src = source.to_string();
        let rk = routing_key.to_string();
        let res: Result<(), Error> = RUNTIME.block_on(async move {
            ch.exchange_unbind(&dest, &src, &rk, opts, args).await?;
            Ok(())
        });
        if let Err(ref e) = res {
            self.set_closed_with_error(format!("exchange.unbind failed: {e}"));
        }
        res
    }
}
