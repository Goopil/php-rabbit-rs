use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use ext_php_rs::{
    prelude::{ModuleBuilder, PhpException, PhpResult, php_function},
    types::{ZendHashTable, Zval},
    wrap_function,
};
use rabbit_rs_core::{
    client::ClientPool,
    config::SafetyMode,
    consumer::APPLICATION_ATTEMPTS_HEADER,
    pool::ConnectionKey,
    publisher::PublisherConfig,
    runtime::RuntimeRegistry,
    transport::{
        Delivery, HeaderFloat, HeaderValue, Headers, PublishConfirmation, ReturnedMessage,
        TransportError, mock::MockTransport,
    },
};

use crate::{
    classes::pool::Pool,
    conversion::{self, is_list, optional_non_negative_integer, reject_unknown_keys, string_key},
};

pub(crate) fn register(module: ModuleBuilder) -> ModuleBuilder {
    module.function(wrap_function!(testing_pool))
}

#[derive(Debug)]
struct Scenario {
    deliveries: Vec<DeliveryFixture>,
    publisher_capacity: usize,
    publisher_safety: SafetyMode,
    pending_confirmations: usize,
    confirmed_publications: usize,
    publication_outcomes: Vec<PublicationFixture>,
    buffer_flush_interval_ms: Option<u64>,
    buffer_flush_threshold: Option<usize>,
}

#[derive(Debug)]
enum PublicationFixture {
    Ack,
    Returned,
    Pending,
    TransportError,
}

#[derive(Debug)]
struct DeliveryFixture {
    message_id: Option<String>,
    correlation_id: Option<String>,
    payload: Bytes,
    headers: Headers,
}

#[php_function]
#[php(name = "Goopil\\RabbitRs\\testing_pool")]
pub(crate) fn testing_pool(config: &ZendHashTable, scenario: &ZendHashTable) -> PhpResult<Pool> {
    let config = Arc::new(conversion::validated_config(config).map_err(testing_exception)?);
    let scenario = Scenario::parse(scenario).map_err(testing_exception)?;
    let transport = MockTransport::default();
    transport.keep_delivery_stream_open();
    for outcome in scenario.publication_outcomes {
        match outcome {
            PublicationFixture::Ack => {
                transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
            }
            PublicationFixture::Returned => {
                transport.push_confirmation(Ok(PublishConfirmation::Ack(Some(ReturnedMessage {
                    reply_code: 312,
                    reply_text: "NO_ROUTE".to_owned(),
                    exchange: "jobs".to_owned(),
                    routing_key: "default".to_owned(),
                    payload: Bytes::from_static(b"payload"),
                }))));
            }
            PublicationFixture::Pending => transport.push_pending_confirmation(),
            PublicationFixture::TransportError => transport.push_confirmation(Err(
                TransportError::protocol("transport failed during confirmation"),
            )),
        }
    }
    for _ in 0..scenario.pending_confirmations {
        transport.push_pending_confirmation();
    }
    for _ in 0..scenario.confirmed_publications {
        transport.push_confirmation(Ok(PublishConfirmation::Ack(None)));
    }
    for (index, fixture) in scenario.deliveries.into_iter().enumerate() {
        transport.push_delivery(Ok(Delivery {
            delivery_tag: u64::try_from(index.saturating_add(1)).unwrap_or(u64::MAX),
            exchange: "testing".to_owned(),
            routing_key: "testing".to_owned(),
            redelivered: false,
            message_id: fixture.message_id,
            correlation_id: fixture.correlation_id,
            headers: Arc::new(fixture.headers),
            payload: fixture.payload,
        }));
    }

    let key = ConnectionKey::from_config(&config);
    let handle = RuntimeRegistry::global()
        .acquire(key)
        .map_err(|error| testing_exception(error.to_string()))?;
    let publisher_config = PublisherConfig::with_safety(
        scenario.publisher_capacity,
        Duration::from_secs(30),
        scenario.publisher_safety,
    );
    let delay_strategy = rabbit_rs_core::topology::delay::DelayStrategy::compile(&config);
    let client = Arc::new(ClientPool::new_for_tests(
        config,
        Arc::new(transport),
        publisher_config,
    ));

    Ok(Pool::for_testing(
        handle,
        client,
        delay_strategy,
        scenario.buffer_flush_interval_ms.map(Duration::from_millis),
        scenario.buffer_flush_threshold,
    ))
}

impl Scenario {
    fn parse(table: &ZendHashTable) -> Result<Self, String> {
        reject_unknown_keys(
            table,
            "scenario",
            &[
                "deliveries",
                "publisher_capacity",
                "publisher_safety",
                "pending_confirmations",
                "confirmed_publications",
                "publication_outcomes",
                "buffer_flush_interval_ms",
                "buffer_flush_threshold",
            ],
        )?;
        let publisher_capacity =
            optional_usize(table, "publisher_capacity", "scenario")?.unwrap_or(1024);
        if publisher_capacity == 0 {
            return Err("scenario.publisher_capacity: must be greater than zero".to_owned());
        }
        let publisher_safety = optional_publisher_safety(table)?;
        let pending_confirmations =
            optional_usize(table, "pending_confirmations", "scenario")?.unwrap_or(0);
        let confirmed_publications =
            optional_usize(table, "confirmed_publications", "scenario")?.unwrap_or(0);
        let publication_outcomes = optional_publication_outcomes(table)?;
        let deliveries = optional_deliveries(table)?;
        let buffer_flush_interval_ms = optional_u64(table, "buffer_flush_interval_ms", "scenario")?;
        let buffer_flush_threshold = optional_usize(table, "buffer_flush_threshold", "scenario")?;

        Ok(Self {
            deliveries,
            publisher_capacity,
            publisher_safety,
            pending_confirmations,
            confirmed_publications,
            publication_outcomes,
            buffer_flush_interval_ms,
            buffer_flush_threshold,
        })
    }
}

fn optional_publisher_safety(table: &ZendHashTable) -> Result<SafetyMode, String> {
    let Some(value) = table.get("publisher_safety").map(Zval::dereference) else {
        return Ok(SafetyMode::Safe);
    };
    let value = value
        .dereference()
        .str()
        .ok_or_else(|| "scenario.publisher_safety: expected a string".to_owned())?;
    match value {
        "safe" => Ok(SafetyMode::Safe),
        "unsafe" => Ok(SafetyMode::Unsafe),
        "blind" => Ok(SafetyMode::Blind),
        other => Err(format!(
            "scenario.publisher_safety: unsupported safety mode '{other}'"
        )),
    }
}

fn optional_publication_outcomes(table: &ZendHashTable) -> Result<Vec<PublicationFixture>, String> {
    let Some(value) = table.get("publication_outcomes").map(Zval::dereference) else {
        return Ok(Vec::new());
    };
    let outcomes = value
        .array()
        .ok_or_else(|| "scenario.publication_outcomes: expected a list".to_owned())?;
    if !is_list(outcomes) {
        return Err("scenario.publication_outcomes: expected a list".to_owned());
    }

    outcomes
        .iter()
        .enumerate()
        .map(|(index, (_, value))| {
            let path = format!("scenario.publication_outcomes.{index}");
            let value = value
                .dereference()
                .str()
                .ok_or_else(|| format!("{path}: expected a UTF-8 string"))?;
            match value {
                "ack" => Ok(PublicationFixture::Ack),
                "returned" => Ok(PublicationFixture::Returned),
                "pending" => Ok(PublicationFixture::Pending),
                "transport_error" => Ok(PublicationFixture::TransportError),
                _ => Err(format!("{path}: unsupported publication outcome '{value}'")),
            }
        })
        .collect()
}

fn optional_deliveries(table: &ZendHashTable) -> Result<Vec<DeliveryFixture>, String> {
    let Some(value) = table.get("deliveries").map(Zval::dereference) else {
        return Ok(Vec::new());
    };
    let deliveries = value
        .array()
        .ok_or_else(|| "scenario.deliveries: expected a list".to_owned())?;
    if !is_list(deliveries) {
        return Err("scenario.deliveries: expected a list".to_owned());
    }

    deliveries
        .iter()
        .enumerate()
        .map(|(index, (_, value))| {
            let path = format!("scenario.deliveries.{index}");
            let delivery = value
                .dereference()
                .array()
                .ok_or_else(|| format!("{path}: expected an array"))?;
            reject_unknown_keys(
                delivery,
                &path,
                &[
                    "message_id",
                    "correlation_id",
                    "payload",
                    "headers",
                    "attempts",
                ],
            )?;
            let message_id = optional_text(delivery, "message_id", &path)?;
            let correlation_id = optional_text(delivery, "correlation_id", &path)?;
            let payload = required_binary(delivery, "payload", &path)?;
            let attempts = optional_u32(delivery, "attempts", &path)?.unwrap_or(1);
            let mut headers = optional_headers(delivery, &path)?;
            headers.insert(
                APPLICATION_ATTEMPTS_HEADER.to_owned(),
                HeaderValue::Integer(i64::from(attempts)),
            );
            Ok(DeliveryFixture {
                message_id,
                correlation_id,
                payload,
                headers,
            })
        })
        .collect()
}

fn optional_headers(table: &ZendHashTable, path: &str) -> Result<Headers, String> {
    let Some(value) = table.get("headers").map(Zval::dereference) else {
        return Ok(Headers::new());
    };
    let headers = value
        .array()
        .ok_or_else(|| format!("{path}.headers: expected an associative array"))?;
    let mut output = Headers::new();
    for (key, value) in headers {
        let key = string_key(key, &format!("{path}.headers"))?;
        let value = fixture_header_value(value, &format!("{path}.headers.{key}"), 0)?;
        output.insert(key, value);
    }
    Ok(output)
}

fn fixture_header_value(value: &Zval, path: &str, depth: usize) -> Result<HeaderValue, String> {
    if depth > 16 {
        return Err(format!("{path}: fixture header nesting is too deep"));
    }
    let value = value.dereference();
    if value.is_null() {
        return Ok(HeaderValue::Void);
    }
    if let Some(value) = value.bool() {
        return Ok(HeaderValue::Boolean(value));
    }
    if let Some(value) = value.long() {
        return Ok(HeaderValue::Integer(value));
    }
    if let Some(value) = value.double() {
        return HeaderFloat::new(value)
            .map(HeaderValue::Double)
            .ok_or_else(|| format!("{path}: finite float expected"));
    }
    if let Some(value) = value.zend_str() {
        return Ok(HeaderValue::Binary(Bytes::copy_from_slice(
            value.as_bytes(),
        )));
    }
    if let Some(values) = value.array() {
        if is_list(values) {
            return values
                .iter()
                .enumerate()
                .map(|(index, (_, value))| {
                    fixture_header_value(value, &format!("{path}.{index}"), depth.saturating_add(1))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(HeaderValue::Array);
        }
        let mut table = Headers::new();
        for (key, value) in values {
            let key = string_key(key, path)?;
            let value =
                fixture_header_value(value, &format!("{path}.{key}"), depth.saturating_add(1))?;
            table.insert(key, value);
        }
        return Ok(HeaderValue::Table(table));
    }
    Err(format!("{path}: AMQP-compatible header expected"))
}

fn required_binary(table: &ZendHashTable, key: &str, path: &str) -> Result<Bytes, String> {
    table
        .get(key)
        .map(Zval::dereference)
        .and_then(Zval::zend_str)
        .map(|value| Bytes::copy_from_slice(value.as_bytes()))
        .ok_or_else(|| format!("{path}.{key}: expected a binary PHP string"))
}

fn optional_text(table: &ZendHashTable, key: &str, path: &str) -> Result<Option<String>, String> {
    let Some(value) = table.get(key).map(Zval::dereference) else {
        return Ok(None);
    };
    let value = value
        .zend_str()
        .ok_or_else(|| format!("{path}.{key}: expected a string"))?;
    std::str::from_utf8(value.as_bytes())
        .map(str::to_owned)
        .map(Some)
        .map_err(|_| format!("{path}.{key}: expected a UTF-8 string"))
}

fn optional_usize(table: &ZendHashTable, key: &str, path: &str) -> Result<Option<usize>, String> {
    let value = optional_non_negative_integer(table, key, path)?;
    value
        .map(usize::try_from)
        .transpose()
        .map_err(|_| format!("{path}.{key}: integer is too large"))
}

fn optional_u32(table: &ZendHashTable, key: &str, path: &str) -> Result<Option<u32>, String> {
    let value = optional_non_negative_integer(table, key, path)?;
    value
        .map(u32::try_from)
        .transpose()
        .map_err(|_| format!("{path}.{key}: integer is too large"))
}

fn optional_u64(table: &ZendHashTable, key: &str, path: &str) -> Result<Option<u64>, String> {
    let value = optional_non_negative_integer(table, key, path)?;
    value
        .map(u64::try_from)
        .transpose()
        .map_err(|_| format!("{path}.{key}: integer is too large"))
}

fn testing_exception(message: String) -> PhpException {
    crate::classes::exception::rabbit_exception_message(message)
}
