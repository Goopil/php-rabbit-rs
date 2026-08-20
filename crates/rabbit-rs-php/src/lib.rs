#![forbid(unsafe_code)]

//! PHP extension boundary for Rabbit RS.

mod callbacks;
mod classes;
mod conversion;
#[cfg(feature = "extension-tests")]
mod testing;

use classes::{
    consumer::{Consumer, ConsumerIterator},
    delivery::Delivery,
    exception::{BackpressureException, ConnectionException, RabbitRsException},
    pool::Pool,
};
use ext_php_rs::prelude::{ModuleBuilder, php_module};
use rabbit_rs_core::runtime::RuntimeRegistry;

extern "C" fn module_shutdown(_module_type: i32, _module_number: i32) -> i32 {
    RuntimeRegistry::global().close();
    0
}

#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    let module = module
        .name("rabbit_rs")
        .version(env!("CARGO_PKG_VERSION"))
        .shutdown_function(module_shutdown)
        .class::<RabbitRsException>()
        .class::<BackpressureException>()
        .class::<ConnectionException>()
        .class::<Pool>()
        .class::<Consumer>()
        .class::<ConsumerIterator>()
        .class::<Delivery>();

    #[cfg(feature = "extension-tests")]
    let module = testing::register(module);

    module
}
