use std::{borrow::Cow, collections::HashSet};

use bytes::Bytes;
use ext_php_rs::types::{ArrayKey, ZendHashTable, Zval};
use rabbit_rs_core::{
    config::{Config, ValidatedConfig},
    publisher::{Destination, MessageProperties, PublishRequest},
    transport::{HeaderFloat, HeaderValue, PublishHeaders},
};
use serde_json::{Map, Number, Value};
use tokio::time::Instant;

const MAX_CONFIG_DEPTH: usize = 64;
pub(crate) const MAX_BATCH_MESSAGES: usize = 256;
pub(crate) const MAX_BATCH_PAYLOAD_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_HEADER_ENTRIES: usize = 128;
pub(crate) const MAX_HEADER_BYTES: usize = 64 * 1024;
pub(crate) const MAX_TIMEOUT_MS: u64 = 86_400_000;

#[derive(Default)]
struct ConversionBudget {
    payload_bytes: usize,
    header_entries: usize,
    header_bytes: usize,
}

impl ConversionBudget {
    fn add_payload(&mut self, path: &str, bytes: usize) -> Result<(), String> {
        self.payload_bytes = self
            .payload_bytes
            .checked_add(bytes)
            .ok_or_else(|| format!("{path}: payload size overflow"))?;
        if self.payload_bytes > MAX_BATCH_PAYLOAD_BYTES {
            return Err(format!(
                "{path}: cumulative payload exceeds the {MAX_BATCH_PAYLOAD_BYTES} byte limit"
            ));
        }
        Ok(())
    }

    fn add_headers(&mut self, parent_path: &str, entries: usize) -> Result<(), String> {
        self.header_entries = self
            .header_entries
            .checked_add(entries)
            .ok_or_else(|| format!("{parent_path}.headers: header count overflow"))?;
        if self.header_entries > MAX_HEADER_ENTRIES {
            return Err(format!(
                "{parent_path}.headers: exceeds the {MAX_HEADER_ENTRIES} header entry limit"
            ));
        }
        Ok(())
    }

    fn add_header_bytes(
        &mut self,
        parent_path: &str,
        key: &str,
        bytes: usize,
    ) -> Result<(), String> {
        self.header_bytes = self
            .header_bytes
            .checked_add(bytes)
            .ok_or_else(|| format!("{parent_path}.headers.{key}: header size overflow"))?;
        if self.header_bytes > MAX_HEADER_BYTES {
            return Err(format!(
                "{parent_path}.headers.{key}: cumulative headers exceed the {MAX_HEADER_BYTES} byte limit"
            ));
        }
        Ok(())
    }
}

pub(crate) struct NativePublish {
    pub broker: String,
    pub request: PublishRequest,
}

pub(crate) fn validated_config(table: &ZendHashTable) -> Result<ValidatedConfig, String> {
    let mut active_arrays = HashSet::new();
    let value = array_value(table, "config", 0, &mut active_arrays)?;
    let config: Config = serde_json::from_value(value)
        .map_err(|error| format!("config: invalid structure: {error}"))?;
    config.validate().map_err(|error| error.to_string())
}

pub(crate) fn publish(table: &ZendHashTable, path: &str) -> Result<NativePublish, String> {
    publish_with_budget(table, path, &mut ConversionBudget::default())
}

fn publish_with_budget(
    table: &ZendHashTable,
    path: &str,
    budget: &mut ConversionBudget,
) -> Result<NativePublish, String> {
    reject_unknown_keys(
        table,
        path,
        &[
            "broker",
            "exchange",
            "routing_key",
            "payload",
            "message_id",
            "content_type",
            "correlation_id",
            "headers",
            "delay_ms",
            "timeout_ms",
        ],
    )?;

    let broker = required_string(table, "broker", path)?;
    let exchange = required_string(table, "exchange", path)?;
    let routing_key = required_string(table, "routing_key", path)?;
    let message_id = required_string(table, "message_id", path)?;
    if message_id.is_empty() {
        return Err(format!("{path}.message_id: must not be empty"));
    }
    let payload = required_payload(table, path, budget)?;
    let timeout_ms = optional_non_negative_integer(table, "timeout_ms", path)?.unwrap_or(30_000);
    if timeout_ms == 0 {
        return Err(format!("{path}.timeout_ms: must be greater than zero"));
    }
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(format!(
            "{path}.timeout_ms: exceeds the {MAX_TIMEOUT_MS} millisecond limit"
        ));
    }
    let deadline = Instant::now()
        .checked_add(std::time::Duration::from_millis(timeout_ms))
        .ok_or_else(|| format!("{path}.timeout_ms: deadline overflow"))?;

    let mut properties = MessageProperties::new(message_id);
    properties.content_type = optional_string(table, "content_type", path)?;
    properties.correlation_id = optional_string(table, "correlation_id", path)?;
    properties.delay_ms = optional_non_negative_integer(table, "delay_ms", path)?;
    properties.headers = optional_headers(table, path, budget)?;

    Ok(NativePublish {
        broker,
        request: PublishRequest::new(
            Destination::new(exchange, routing_key),
            payload,
            properties,
            deadline,
        ),
    })
}

pub(crate) fn publish_batch(table: &ZendHashTable) -> Result<Vec<NativePublish>, String> {
    if !is_list(table) {
        return Err("messages: publishBatch expects a list".to_owned());
    }
    if table.len() > MAX_BATCH_MESSAGES {
        return Err(format!(
            "messages: exceeds the {MAX_BATCH_MESSAGES} message limit"
        ));
    }
    let mut budget = ConversionBudget::default();
    let mut publishes = Vec::with_capacity(table.len());
    for (index, (_, value)) in table.iter().enumerate() {
        let path = format!("messages[{index}]");
        let message = value
            .dereference()
            .array()
            .ok_or_else(|| format!("{path}: expected an array"))?;
        publishes.push(publish_with_budget(message, &path, &mut budget)?);
    }
    Ok(publishes)
}

fn value(
    input: &Zval,
    path: &str,
    depth: usize,
    active_arrays: &mut HashSet<usize>,
) -> Result<Value, String> {
    if depth > MAX_CONFIG_DEPTH {
        return Err(format!("{path}: maximum nesting depth exceeded"));
    }

    let input = input.dereference();
    if input.is_null() {
        return Ok(Value::Null);
    }
    if let Some(value) = input.bool() {
        return Ok(Value::Bool(value));
    }
    if let Some(value) = input.long() {
        return Ok(Value::Number(Number::from(value)));
    }
    if let Some(value) = input.double() {
        return Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| format!("{path}: non-finite floating-point value is unsupported"));
    }
    if input.is_string() {
        return input
            .str()
            .map(|value| Value::String(value.to_owned()))
            .ok_or_else(|| format!("{path}: configuration strings must be valid UTF-8"));
    }
    if let Some(array) = input.array() {
        return array_value(array, path, depth, active_arrays);
    }

    Err(format!(
        "{path}: resources, objects, and callable values are unsupported"
    ))
}

fn array_value(
    input: &ZendHashTable,
    path: &str,
    depth: usize,
    active_arrays: &mut HashSet<usize>,
) -> Result<Value, String> {
    let identity = std::ptr::from_ref(input).addr();
    if !active_arrays.insert(identity) {
        return Err(format!("{path}: recursive arrays are unsupported"));
    }

    let result = if is_list(input) {
        input
            .iter()
            .enumerate()
            .map(|(index, (_, item))| {
                value(
                    item,
                    &format!("{path}.{index}"),
                    depth.saturating_add(1),
                    active_arrays,
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array)
    } else {
        let mut object = Map::with_capacity(input.len());
        for (key, item) in input {
            let key = string_key(key, path)?;
            let item_path = format!("{path}.{key}");
            object.insert(
                key,
                value(item, &item_path, depth.saturating_add(1), active_arrays)?,
            );
        }
        Ok(Value::Object(object))
    };

    active_arrays.remove(&identity);
    result
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
            "{path}.{value}: associative configuration arrays require string keys"
        )),
    }
}

fn required<'a>(table: &'a ZendHashTable, key: &str, path: &str) -> Result<&'a Zval, String> {
    table
        .get(key)
        .map(Zval::dereference)
        .ok_or_else(|| format!("{path}.{key}: required field is missing"))
}

fn required_string(table: &ZendHashTable, key: &str, path: &str) -> Result<String, String> {
    required(table, key, path)?
        .str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{path}.{key}: expected a valid UTF-8 string"))
}

fn optional_string(table: &ZendHashTable, key: &str, path: &str) -> Result<Option<String>, String> {
    let Some(value) = table.get(key).map(Zval::dereference) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .str()
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| format!("{path}.{key}: expected a valid UTF-8 string or null"))
}

fn required_payload(
    table: &ZendHashTable,
    path: &str,
    budget: &mut ConversionBudget,
) -> Result<Bytes, String> {
    let value = required(table, "payload", path)?
        .zend_str()
        .ok_or_else(|| format!("{path}.payload: expected a binary PHP string"))?;
    budget.add_payload(&format!("{path}.payload"), value.as_bytes().len())?;
    Ok(Bytes::copy_from_slice(value.as_bytes()))
}

fn optional_non_negative_integer(
    table: &ZendHashTable,
    key: &str,
    path: &str,
) -> Result<Option<u64>, String> {
    let Some(value) = table.get(key).map(Zval::dereference) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .long()
        .ok_or_else(|| format!("{path}.{key}: expected a non-negative integer"))?;
    u64::try_from(value)
        .map(Some)
        .map_err(|_| format!("{path}.{key}: expected a non-negative integer"))
}

fn optional_headers(
    table: &ZendHashTable,
    path: &str,
    budget: &mut ConversionBudget,
) -> Result<PublishHeaders, String> {
    let Some(value) = table.get("headers").map(Zval::dereference) else {
        return Ok(PublishHeaders::new());
    };
    if value.is_null() {
        return Ok(PublishHeaders::new());
    }
    let headers = value
        .array()
        .ok_or_else(|| format!("{path}.headers: expected an associative array"))?;
    budget.add_headers(path, headers.len())?;
    let mut output = PublishHeaders::new();
    for (key, value) in headers {
        let key = match key {
            ArrayKey::Str(key) => Cow::Borrowed(key),
            ArrayKey::String(key) => Cow::Owned(key),
            ArrayKey::ZendString(key) => Cow::Borrowed(
                key.as_str()
                    .map_err(|_| format!("{path}.headers: key must be valid UTF-8"))?,
            ),
            ArrayKey::Long(key) => {
                return Err(format!(
                    "{path}.headers.{key}: integer keys are unsupported"
                ));
            }
        };
        budget.add_header_bytes(path, &key, key.len())?;
        let value = header_value(value, path, &key, budget)?;
        output.insert(key.into_owned(), value);
    }
    Ok(output)
}

fn header_value(
    input: &Zval,
    parent_path: &str,
    key: &str,
    budget: &mut ConversionBudget,
) -> Result<HeaderValue, String> {
    let input = input.dereference();
    if input.is_null() {
        return Ok(HeaderValue::Void);
    }
    if let Some(value) = input.bool() {
        budget.add_header_bytes(parent_path, key, 1)?;
        return Ok(HeaderValue::Boolean(value));
    }
    if let Some(value) = input.long() {
        budget.add_header_bytes(parent_path, key, size_of::<i64>())?;
        return Ok(HeaderValue::Integer(value));
    }
    if let Some(value) = input.double() {
        let value = HeaderFloat::new(value)
            .map(HeaderValue::Double)
            .ok_or_else(|| {
                format!(
                    "{parent_path}.headers.{key}: non-finite floating-point value is unsupported"
                )
            })?;
        budget.add_header_bytes(parent_path, key, size_of::<f64>())?;
        return Ok(value);
    }
    if let Some(value) = input.zend_str() {
        budget.add_header_bytes(parent_path, key, value.as_bytes().len())?;
        return Ok(HeaderValue::Binary(Bytes::copy_from_slice(
            value.as_bytes(),
        )));
    }
    if input.array().is_some() {
        return Err(format!(
            "{parent_path}.headers.{key}: nested arrays are unsupported; headers must be flat"
        ));
    }

    Err(format!(
        "{parent_path}.headers.{key}: expected null, bool, int, finite float, or string"
    ))
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
