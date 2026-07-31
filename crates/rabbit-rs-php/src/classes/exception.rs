use ext_php_rs::{
    flags::ClassFlags,
    prelude::{PhpException, PhpResult, php_class},
    zend::ce,
};

#[php_class]
#[php(name = "Goopil\\RabbitRs\\Exception")]
#[php(extends(ce = ce::exception, stub = "\\Exception"))]
#[derive(Default)]
pub struct RabbitRsException;

#[php_class]
#[php(name = "Goopil\\RabbitRs\\BackpressureException")]
#[php(extends(RabbitRsException))]
#[php(flags = ClassFlags::Final)]
#[derive(Default)]
pub struct BackpressureException;

#[php_class]
#[php(name = "Goopil\\RabbitRs\\ConnectionException")]
#[php(extends(RabbitRsException))]
#[php(flags = ClassFlags::Final)]
#[derive(Default)]
pub struct ConnectionException;

#[expect(dead_code, reason = "reserved for the Task 2 native handle stubs")]
pub(crate) fn unavailable<T>(operation: &str) -> PhpResult<T> {
    Err(PhpException::from_class::<RabbitRsException>(format!(
        "{operation} is not available before native handle initialization"
    )))
}
