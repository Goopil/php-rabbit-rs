use super::{consumer::Consumer, exception::unavailable};
use ext_php_rs::{
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::ZendHashTable,
};

/// Native `RabbitMQ` connection and operation pool.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Pool")]
#[php(flags = ClassFlags::Final)]
pub struct Pool;

#[php_impl]
impl Pool {
    /// Creates a native pool from its PHP configuration.
    pub fn __construct(config: &ZendHashTable) -> PhpResult<Self> {
        let _ = config;
        unavailable("Goopil\\RabbitRs\\Pool::__construct")
    }

    /// Publishes one message and returns its stable message identifier.
    pub fn publish(&self, message: &ZendHashTable) -> PhpResult<String> {
        let _ = self;
        let _ = message;
        unavailable("Goopil\\RabbitRs\\Pool::publish")
    }

    /// Publishes multiple messages in one boundary crossing.
    pub fn publish_batch(&self, messages: &ZendHashTable) -> PhpResult<Vec<String>> {
        let _ = self;
        let _ = messages;
        unavailable("Goopil\\RabbitRs\\Pool::publishBatch")
    }

    /// Opens a consumer for a configured profile.
    pub fn consumer(&self, profile: String) -> PhpResult<Consumer> {
        let _ = self;
        drop(profile);
        unavailable("Goopil\\RabbitRs\\Pool::consumer")
    }

    /// Returns the current native metrics snapshot.
    pub fn stats(&self) -> PhpResult<ZBox<ZendHashTable>> {
        let _ = self;
        unavailable("Goopil\\RabbitRs\\Pool::stats")
    }

    /// Closes this pool handle.
    pub fn close(&self) -> PhpResult<()> {
        let _ = self;
        unavailable("Goopil\\RabbitRs\\Pool::close")
    }
}
