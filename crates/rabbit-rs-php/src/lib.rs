#![forbid(unsafe_code)]

//! PHP extension boundary for Rabbit RS.

mod callbacks;
mod classes;
mod conversion;
mod sink;
#[cfg(feature = "extension-tests")]
mod testing;

/// Benchmark entry surface: the publish buffer is the pure-Rust hot path
/// every PHP publish traverses, so CodSpeed benches it directly. Kept
/// `#[doc(hidden)]` — not part of the extension's public contract.
#[doc(hidden)]
pub mod bench_api {
    pub use crate::classes::publish_buffer::PublishBuffer;
    pub use crate::conversion::NativePublish;
}

use classes::{
    consumer::Consumer,
    delivery::Delivery,
    exception::{BackpressureException, ConnectionException, RabbitRsException},
    pool::Pool,
};
use ext_php_rs::prelude::{ModuleBuilder, php_module};
use rabbit_rs_core::runtime::RuntimeRegistry;

extern "C" fn module_shutdown(_module_type: i32, _module_number: i32) -> i32 {
    // ZTS (issue #369): MSHUTDOWN runs on every worker-thread teardown (e.g.
    // FrankenPHP worker recycling), and closing the process-global registry
    // there kills the shared Tokio runtime under the surviving threads —
    // parked timer futures panic. The registry is reclaimed at process exit,
    // the only moment the close has value under ZTS. NTS keeps the close.
    if !ext_php_rs::PHP_ZTS {
        RuntimeRegistry::global().close();
    }
    0
}

#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    // MINIT: install the diagnostics sink once, before any pool exists. The
    // sink stays silent unless RABBIT_RS_LOG selects a severity.
    sink::install_from_env();

    let module = module
        .name("rabbit_rs")
        .version(env!("CARGO_PKG_VERSION"))
        .shutdown_function(module_shutdown)
        .class::<RabbitRsException>()
        .class::<BackpressureException>()
        .class::<ConnectionException>()
        .class::<Pool>()
        .class::<Consumer>()
        .class::<Delivery>();

    #[cfg(feature = "extension-tests")]
    let module = testing::register(module);

    module
}
