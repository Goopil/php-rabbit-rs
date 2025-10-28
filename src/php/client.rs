use crate::core::connections::connection_client::AmqpClient;
use crate::core::connections::connection_options::ConnectionOptions;
use crate::php::channel::PhpChannel;
use crate::php::parsers::parse_connection_options;
use ext_php_rs::exception::{PhpException, PhpResult};
use ext_php_rs::types::Iterable;
use ext_php_rs::{php_class, php_impl};

#[php_class]
#[php(name = "Goopil\\RabbitRs\\PhpClient")]
pub struct PhpClient {
    client: Option<AmqpClient>,
    opts: ConnectionOptions,
}

#[php_impl]
impl PhpClient {
    pub fn __construct(params: Option<Iterable>) -> PhpResult<Self> {
        let opts = parse_connection_options(params)?;

        Ok(Self { client: None, opts })
    }

    pub fn get_id(&self) -> PhpResult<String> {
        if let Some(client) = &self.client {
            return Ok(client.id());
        }

        Err(PhpException::default("Client is not connected".into()))
    }

    pub fn connect(&mut self) -> PhpResult<bool> {
        if self.client.is_none() {
            let client = AmqpClient::connect(self.opts.clone())
                .map_err(|e| PhpException::default(format!("{e}")));
            self.client = Some(client?);
        }

        Ok(true)
    }

    pub fn reconnect(&mut self) -> PhpResult<bool> {
        if let Some(client) = &self.client {
            client
                .reconnect_blocking()
                .map_err(|e| PhpException::default(format!("Reconnect failed: {e}")))?;

            return Ok(true);
        }

        Err(PhpException::default("Client is not connected".into()))
    }

    pub fn close(&mut self) -> PhpResult<bool> {
        if let Some(c) = self.client.take() {
            c.close_if_last()
                .map_err(|e| PhpException::default(format!("AMQP close error: {e}")))?;
        }

        Ok(true)
    }
    pub fn force_close(&mut self) -> PhpResult<bool> {
        if let Some(c) = self.client.take() {
            c.close()
                .map_err(|e| PhpException::default(format!("AMQP force close error: {e}")))?;
        }

        Ok(true)
    }

    pub fn ping(&self) -> PhpResult<bool> {
        if let Some(client) = &self.client {
            client
                .ping()
                .map_err(|e| PhpException::default(format!("Ping failed: {e}")))?;

            return Ok(true);
        }

        Err(PhpException::default("Client is not connected".into()))
    }

    pub fn open_channel(&self) -> PhpResult<PhpChannel> {
        if let Some(client) = &self.client {
            let ch = client
                .open_channel()
                .map_err(|e| PhpException::default(format!("Open channel failed: {e}")))?;

            return Ok(PhpChannel::new(ch));
        }

        Err(PhpException::default("Client is not connected".into()))
    }
}
