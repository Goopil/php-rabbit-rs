#![forbid(unsafe_code)]

//! PHP extension boundary for Rabbit RS.

mod classes;

use classes::{
    consumer::Consumer,
    delivery::Delivery,
    exception::{BackpressureException, ConnectionException, RabbitRsException},
    pool::Pool,
};
use ext_php_rs::prelude::{ModuleBuilder, php_module};

#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .name("rabbit_rs")
        .version(env!("CARGO_PKG_VERSION"))
        .class::<RabbitRsException>()
        .class::<BackpressureException>()
        .class::<ConnectionException>()
        .class::<Pool>()
        .class::<Consumer>()
        .class::<Delivery>()
}
