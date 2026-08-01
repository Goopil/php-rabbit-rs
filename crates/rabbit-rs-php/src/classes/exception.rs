use ext_php_rs::{
    flags::ClassFlags,
    prelude::{PhpException, PhpResult, php_class},
    zend::ce,
};
use rabbit_rs_core::client::{ClientError, ClientErrorKind};
use rabbit_rs_core::consumer::{ConsumerError, ConsumerErrorKind};

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

pub(crate) fn rabbit_exception<T>(message: impl Into<String>) -> PhpResult<T> {
    Err(PhpException::from_class::<RabbitRsException>(
        message.into(),
    ))
}

pub(crate) fn client_exception<T>(error: &ClientError) -> PhpResult<T> {
    let message = error.to_string();
    match error.kind() {
        ClientErrorKind::Backpressure => {
            Err(PhpException::from_class::<BackpressureException>(message))
        }
        ClientErrorKind::Transport => Err(PhpException::from_class::<ConnectionException>(message)),
        ClientErrorKind::Configuration
        | ClientErrorKind::Publish
        | ClientErrorKind::Consumer
        | ClientErrorKind::Closed => rabbit_exception(message),
    }
}

pub(crate) fn consumer_exception<T>(error: &ConsumerError) -> PhpResult<T> {
    match error.kind() {
        ConsumerErrorKind::Transport | ConsumerErrorKind::StaleGeneration => Err(
            PhpException::from_class::<ConnectionException>(error.to_string()),
        ),
        ConsumerErrorKind::Closed
        | ConsumerErrorKind::AlreadySettled
        | ConsumerErrorKind::Publish
        | ConsumerErrorKind::MissingPublisher
        | ConsumerErrorKind::InvalidSubscription
        | ConsumerErrorKind::MaxAttempts => rabbit_exception(error.to_string()),
    }
}
