use ext_php_rs::{
    flags::ClassFlags,
    prelude::{PhpException, PhpResult, php_class, php_impl},
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

#[php_impl]
impl ConnectionException {
    /// Throws a connection exception carrying the given message; never returns.
    ///
    /// A native exception's message can only be set when the exception is
    /// thrown (the base PHP exception message is written by the throw
    /// machinery), so PHP userland that must surface a connection-level
    /// failure itself — e.g. the Laravel queue draining an async settlement
    /// error — calls this factory instead of constructing the class.
    pub fn throw(message: String) -> PhpResult<()> {
        Err(PhpException::from_class::<ConnectionException>(message))
    }
}

/// Builds a base exception value without wrapping it in `PhpResult`.
pub(crate) fn rabbit_exception_message(message: String) -> PhpException {
    PhpException::from_class::<RabbitRsException>(message)
}

pub(crate) fn rabbit_exception<T>(message: impl Into<String>) -> PhpResult<T> {
    Err(PhpException::from_class::<RabbitRsException>(
        message.into(),
    ))
}

pub(crate) fn backpressure_exception<T>(message: &str) -> PhpResult<T> {
    Err(PhpException::from_class::<BackpressureException>(
        message.to_owned(),
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

/// Builds the PHP exception for a consumer error without wrapping it.
pub(crate) fn consumer_exception_message(error: &ConsumerError) -> PhpException {
    let message = error.to_string();
    match error.kind() {
        ConsumerErrorKind::Transport
        | ConsumerErrorKind::StaleGeneration
        | ConsumerErrorKind::SourceReplaced => {
            PhpException::from_class::<ConnectionException>(message)
        }
        ConsumerErrorKind::Closed
        | ConsumerErrorKind::AlreadySettled
        | ConsumerErrorKind::AlreadySettling
        | ConsumerErrorKind::SettlementInProgress
        | ConsumerErrorKind::Publish
        | ConsumerErrorKind::MissingPublisher
        | ConsumerErrorKind::InvalidSubscription
        | ConsumerErrorKind::MaxAttempts
        | ConsumerErrorKind::InvalidDelay => PhpException::from_class::<RabbitRsException>(message),
    }
}

pub(crate) fn consumer_exception<T>(error: &ConsumerError) -> PhpResult<T> {
    Err(consumer_exception_message(error))
}
