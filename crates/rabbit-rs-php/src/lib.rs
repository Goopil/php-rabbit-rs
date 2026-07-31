#![forbid(unsafe_code)]

//! PHP extension boundary for Rabbit RS.

mod classes;

use classes::exception::{BackpressureException, ConnectionException, RabbitRsException};
use ext_php_rs::prelude::{ModuleBuilder, php_module};

#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .name("rabbit_rs")
        .version(env!("CARGO_PKG_VERSION"))
        .class::<RabbitRsException>()
        .class::<BackpressureException>()
        .class::<ConnectionException>()
}
