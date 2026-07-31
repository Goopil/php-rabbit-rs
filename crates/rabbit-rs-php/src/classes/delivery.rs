#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use super::exception::unavailable;
use ext_php_rs::{
    binary::Binary,
    boxed::ZBox,
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
    types::ZendHashTable,
};

/// Native delivery and its acknowledgement token.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Delivery")]
#[php(flags = ClassFlags::Final)]
pub struct Delivery;

#[php_impl]
impl Delivery {
    /// Returns the binary-safe delivery payload.
    pub fn payload(&self) -> PhpResult<Binary<u8>> {
        let _ = self;
        unavailable("Goopil\\RabbitRs\\Delivery::payload")
    }

    /// Returns delivery metadata as a PHP array.
    pub fn metadata(&self) -> PhpResult<ZBox<ZendHashTable>> {
        let _ = self;
        unavailable("Goopil\\RabbitRs\\Delivery::metadata")
    }

    /// Acknowledges the delivery.
    pub fn ack(&self) -> PhpResult<()> {
        let _ = self;
        unavailable("Goopil\\RabbitRs\\Delivery::ack")
    }

    /// Releases the delivery immediately or after a delay.
    #[php(defaults(delayMs = 0))]
    pub fn release(&self, delayMs: i64) -> PhpResult<()> {
        let _ = self;
        let _ = delayMs;
        unavailable("Goopil\\RabbitRs\\Delivery::release")
    }

    /// Rejects the delivery with optional requeueing.
    #[php(defaults(requeue = false))]
    pub fn reject(&self, requeue: bool) -> PhpResult<()> {
        let _ = self;
        let _ = requeue;
        unavailable("Goopil\\RabbitRs\\Delivery::reject")
    }
}
