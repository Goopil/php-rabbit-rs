use std::{collections::BTreeMap, sync::Arc, time::Duration};

use bytes::Bytes;
use ext_php_rs::{
    prelude::{ModuleBuilder, PhpException, PhpResult, php_function},
    types::{ArrayKey, ZendHashTable, Zval},
    wrap_function,
};
use rabbit_rs_core::{
    client::ClientPool,
    consumer::APPLICATION_ATTEMPTS_HEADER,
    pool::ConnectionKey,
    publisher::PublisherConfig,
    runtime::RuntimeRegistry,
    transport::{Delivery, Headers, mock::MockTransport},
};

use crate::{
    classes::{exception::RabbitRsException, pool::Pool},
    conversion,
};

pub(crate) fn register(module: ModuleBuilder) -> ModuleBuilder {
    module.function(wrap_function!(testing_pool))
}

#[derive(Debug)]
struct Scenario {
    deliveries: Vec<DeliveryFixture>,
    publisher_capacity: usize,
    pending_confirmations: usize,
}

#[derive(Debug)]
struct DeliveryFixture {
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
    for _ in 0..scenario.pending_confirmations {
        transport.push_pending_confirmation();
    }
    for (index, fixture) in scenario.deliveries.into_iter().enumerate() {
        transport.push_delivery(Ok(Delivery {
            delivery_tag: u64::try_from(index.saturating_add(1)).unwrap_or(u64::MAX),
            exchange: "testing".to_owned(),
            routing_key: "testing".to_owned(),
            redelivered: false,
            headers: fixture.headers,
            payload: fixture.payload,
        }));
    }

    let key = ConnectionKey::from_config(&config);
    let handle = RuntimeRegistry::global()
        .acquire(key)
        .map_err(|error| testing_exception(error.to_string()))?;
    let publisher_config = PublisherConfig::new(
        1,
        1024 * 1024,
        Duration::from_millis(1),
        scenario.publisher_capacity,
        Duration::from_secs(30),
    );
    let client = Arc::new(ClientPool::new_for_tests(
        config,
        Arc::new(transport),
        publisher_config,
    ));

    Ok(Pool::for_testing(handle, client))
}

impl Scenario {
    fn parse(table: &ZendHashTable) -> Result<Self, String> {
        reject_unknown_keys(
            table,
            "scenario",
            &["deliveries", "publisher_capacity", "pending_confirmations"],
        )?;
        let publisher_capacity =
            optional_usize(table, "publisher_capacity", "scenario")?.unwrap_or(8192);
        if publisher_capacity == 0 {
            return Err("scenario.publisher_capacity: must be greater than zero".to_owned());
        }
        let pending_confirmations =
            optional_usize(table, "pending_confirmations", "scenario")?.unwrap_or(0);
        let deliveries = optional_deliveries(table)?;

        Ok(Self {
            deliveries,
            publisher_capacity,
            pending_confirmations,
        })
    }
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
                &["message_id", "payload", "headers", "attempts"],
            )?;
            let payload = required_binary(delivery, "payload", &path)?;
            let attempts = optional_u32(delivery, "attempts", &path)?.unwrap_or(1);
            let mut headers = optional_binary_headers(delivery, &path)?;
            headers.insert(
                APPLICATION_ATTEMPTS_HEADER.to_owned(),
                Bytes::from(attempts.to_string()),
            );
            Ok(DeliveryFixture { payload, headers })
        })
        .collect()
}

fn optional_binary_headers(table: &ZendHashTable, path: &str) -> Result<Headers, String> {
    let Some(value) = table.get("headers").map(Zval::dereference) else {
        return Ok(BTreeMap::new());
    };
    let headers = value
        .array()
        .ok_or_else(|| format!("{path}.headers: expected an associative array"))?;
    let mut output = BTreeMap::new();
    for (key, value) in headers {
        let key = string_key(key, &format!("{path}.headers"))?;
        let value = value
            .dereference()
            .zend_str()
            .map(|value| Bytes::copy_from_slice(value.as_bytes()))
            .ok_or_else(|| format!("{path}.headers.{key}: expected a binary PHP string"))?;
        output.insert(key, value);
    }
    Ok(output)
}

fn required_binary(table: &ZendHashTable, key: &str, path: &str) -> Result<Bytes, String> {
    table
        .get(key)
        .map(Zval::dereference)
        .and_then(Zval::zend_str)
        .map(|value| Bytes::copy_from_slice(value.as_bytes()))
        .ok_or_else(|| format!("{path}.{key}: expected a binary PHP string"))
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

fn optional_non_negative_integer(
    table: &ZendHashTable,
    key: &str,
    path: &str,
) -> Result<Option<u64>, String> {
    let Some(value) = table.get(key).map(Zval::dereference) else {
        return Ok(None);
    };
    let value = value
        .long()
        .ok_or_else(|| format!("{path}.{key}: expected a non-negative integer"))?;
    u64::try_from(value)
        .map(Some)
        .map_err(|_| format!("{path}.{key}: expected a non-negative integer"))
}

fn reject_unknown_keys(table: &ZendHashTable, path: &str, allowed: &[&str]) -> Result<(), String> {
    for (key, _) in table {
        let key = string_key(key, path)?;
        if !allowed.contains(&key.as_str()) {
            return Err(format!("{path}.{key}: unknown field"));
        }
    }
    Ok(())
}

fn is_list(input: &ZendHashTable) -> bool {
    input.iter().enumerate().all(|(index, (key, _))| {
        matches!(key, ArrayKey::Long(value) if value == i64::try_from(index).unwrap_or(i64::MAX))
    })
}

fn string_key(key: ArrayKey<'_>, path: &str) -> Result<String, String> {
    match key {
        ArrayKey::String(value) => Ok(value),
        ArrayKey::Str(value) => Ok(value.to_owned()),
        ArrayKey::ZendString(value) => value
            .as_str()
            .map(ToOwned::to_owned)
            .map_err(|_| format!("{path}: array keys must be valid UTF-8 strings")),
        ArrayKey::Long(value) => Err(format!(
            "{path}.{value}: associative arrays require string keys"
        )),
    }
}

fn testing_exception(message: String) -> PhpException {
    PhpException::from_class::<RabbitRsException>(message)
}
