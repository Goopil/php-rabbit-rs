mod core;
mod php;

use crate::core::connections::connection_pool::dispose_pool;
use crate::php::channel::PhpChannel;
use crate::php::client::PhpClient;
use crate::php::delivery::AmqpDelivery;
use crate::php::message::AmqpMessage;
use ext_php_rs::prelude::{php_module, ModuleBuilder};

#[no_mangle]
extern "C" fn on_shutdown(_type: i32, _module_number: i32) -> i32 {
    dispose_pool();
    0
}

#[php_module]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .class::<PhpClient>()
        .class::<PhpChannel>()
        .class::<AmqpMessage>()
        .class::<AmqpDelivery>()
        .shutdown_function(on_shutdown)
}
