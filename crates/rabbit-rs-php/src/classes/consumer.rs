#![expect(
    non_snake_case,
    reason = "ext-php-rs preserves parameter identifiers for PHP named arguments"
)]

use super::{delivery::Delivery, exception::unavailable};
use ext_php_rs::{
    flags::ClassFlags,
    prelude::{PhpResult, php_class, php_impl},
};

/// Native consumer for an aggregated subscription profile.
#[php_class]
#[php(name = "Goopil\\RabbitRs\\Consumer")]
#[php(flags = ClassFlags::Final)]
pub struct Consumer;

#[php_impl]
impl Consumer {
    /// Returns the next delivery within the requested timeout.
    pub fn next(&self, timeoutMs: i64) -> PhpResult<Option<Delivery>> {
        let _ = self;
        let _ = timeoutMs;
        unavailable("Goopil\\RabbitRs\\Consumer::next")
    }

    /// Closes this consumer handle.
    pub fn close(&self) -> PhpResult<()> {
        let _ = self;
        unavailable("Goopil\\RabbitRs\\Consumer::close")
    }
}
