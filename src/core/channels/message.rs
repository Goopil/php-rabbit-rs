use crate::core::runtime::RUNTIME;
use lapin::acker::Acker;
use lapin::options::{BasicAckOptions, BasicNackOptions, BasicRejectOptions};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum HeaderValue {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(String),
    Table(BTreeMap<String, HeaderValue>),
    Array(Vec<HeaderValue>),
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, Default)]
pub struct CoreProperties {
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub delivery_mode: Option<u8>,
    pub priority: Option<u8>,
    pub correlation_id: Option<String>,
    pub reply_to: Option<String>,
    pub expiration: Option<String>,
    pub message_id: Option<String>,
    pub timestamp: Option<u64>,
    pub kind: Option<String>,
    pub user_id: Option<String>,
    pub app_id: Option<String>,
    pub cluster_id: Option<String>,
    pub headers: Option<BTreeMap<String, HeaderValue>>,
}

#[derive(Clone, Debug)]
pub struct CoreMessage {
    pub body: Arc<Vec<u8>>,
    pub properties: Arc<CoreProperties>,
}

#[derive(Clone, Debug)]
pub struct CoreDelivery {
    pub body: Vec<u8>,
    pub properties: CoreProperties,
    pub exchange: String,
    pub routing_key: String,
    pub redelivered: bool,
    pub delivery_tag: u64,
    acker: Acker,
}

// Core -> lapin (publish)

impl CoreProperties {
    /// Fast-path: true if no property is set (all options None and headers empty)
    pub fn is_default(&self) -> bool {
        self.content_type.is_none()
            && self.content_encoding.is_none()
            && self.delivery_mode.is_none()
            && self.priority.is_none()
            && self.correlation_id.is_none()
            && self.reply_to.is_none()
            && self.expiration.is_none()
            && self.message_id.is_none()
            && self.timestamp.is_none()
            && self.kind.is_none()
            && self.user_id.is_none()
            && self.app_id.is_none()
            && self.cluster_id.is_none()
            && self.headers.as_ref().map(|m| m.is_empty()).unwrap_or(true)
    }

    pub fn to_lapin(&self) -> lapin::BasicProperties {
        use lapin::types::{AMQPValue, FieldTable, LongString, ShortString};
        use lapin::BasicProperties;

        // Fast-path: avoid building anything when everything is default
        if self.is_default() {
            return BasicProperties::default();
        }
        let mut props = BasicProperties::default();

        if let Some(v) = &self.content_type {
            props = props.with_content_type(ShortString::from(v.as_str()));
        }
        if let Some(v) = &self.content_encoding {
            props = props.with_content_encoding(ShortString::from(v.as_str()));
        }
        if let Some(v) = self.delivery_mode {
            props = props.with_delivery_mode(v);
        }
        if let Some(v) = self.priority {
            props = props.with_priority(v);
        }
        if let Some(v) = &self.correlation_id {
            props = props.with_correlation_id(ShortString::from(v.as_str()));
        }
        if let Some(v) = &self.reply_to {
            props = props.with_reply_to(ShortString::from(v.as_str()));
        }
        if let Some(v) = &self.expiration {
            props = props.with_expiration(ShortString::from(v.as_str()));
        }
        if let Some(v) = &self.message_id {
            props = props.with_message_id(ShortString::from(v.as_str()));
        }
        if let Some(v) = self.timestamp {
            props = props.with_timestamp(v as lapin::types::Timestamp);
        }
        if let Some(v) = &self.kind {
            // Setter “type” s’appelle with_type dans lapin
            props = props.with_type(ShortString::from(v.as_str()));
        }
        if let Some(v) = &self.user_id {
            props = props.with_user_id(ShortString::from(v.as_str()));
        }
        if let Some(v) = &self.app_id {
            props = props.with_app_id(ShortString::from(v.as_str()));
        }
        if let Some(v) = &self.cluster_id {
            props = props.with_cluster_id(ShortString::from(v.as_str()));
        }

        if let Some(hdrs) = &self.headers {
            fn to_amqp(v: &HeaderValue) -> Option<AMQPValue> {
                Some(match v {
                    HeaderValue::Null => AMQPValue::Void,
                    HeaderValue::Bool(b) => AMQPValue::Boolean(*b),
                    HeaderValue::Int(i) => AMQPValue::LongLongInt(*i),
                    HeaderValue::Double(f) => AMQPValue::Double(*f),
                    HeaderValue::String(s) => AMQPValue::LongString(LongString::from(s.as_str())),
                    HeaderValue::Table(map) => {
                        let mut sub = FieldTable::default();
                        for (k, vv) in map {
                            if let Some(amqp) = to_amqp(vv) {
                                sub.insert(ShortString::from(k.as_str()), amqp);
                            }
                        }
                        AMQPValue::FieldTable(sub)
                    }
                    HeaderValue::Array(items) => {
                        let mut arr = Vec::with_capacity(items.len());
                        for it in items {
                            if let Some(amqp) = to_amqp(it) {
                                arr.push(amqp);
                            }
                        }
                        AMQPValue::FieldArray(arr.into())
                    }
                    HeaderValue::Bytes(b) => AMQPValue::ByteArray(b.clone().into()),
                })
            }

            let mut tbl = FieldTable::default();
            for (k, v) in hdrs {
                if let Some(amqp) = to_amqp(v) {
                    tbl.insert(lapin::types::ShortString::from(k.as_str()), amqp);
                }
            }
            props = props.with_headers(tbl);
        }

        props
    }

    // lapin -> Core (consume)
    pub fn from_lapin(p: &lapin::BasicProperties) -> Self {
        use lapin::types::AMQPValue;

        // Conversion AMQPValue -> HeaderValue (support FieldArray + ByteArray)
        fn from_amqp(v: &AMQPValue) -> Option<HeaderValue> {
            Some(match v {
                AMQPValue::Void => HeaderValue::Null,
                AMQPValue::Boolean(b) => HeaderValue::Bool(*b),
                AMQPValue::ShortShortInt(i) => HeaderValue::Int(*i as i64),
                AMQPValue::ShortShortUInt(u) => HeaderValue::Int(*u as i64),
                AMQPValue::ShortInt(i) => HeaderValue::Int(*i as i64),
                AMQPValue::ShortUInt(u) => HeaderValue::Int(*u as i64),
                AMQPValue::LongInt(i) => HeaderValue::Int(*i as i64),
                AMQPValue::LongUInt(u) => HeaderValue::Int(*u as i64),
                AMQPValue::LongLongInt(i) => HeaderValue::Int(*i),
                AMQPValue::Float(f) => HeaderValue::Double(*f as f64),
                AMQPValue::Double(f) => HeaderValue::Double(*f),
                AMQPValue::DecimalValue(d) => {
                    HeaderValue::Double(d.value as f64 / 10f64.powi(d.scale as i32))
                }
                AMQPValue::LongString(s) => HeaderValue::String(s.to_string()),
                AMQPValue::ShortString(s) => HeaderValue::String(s.as_str().to_string()),
                AMQPValue::FieldArray(arr) => {
                    let vec_ref = arr.as_slice();
                    let mut out = Vec::with_capacity(vec_ref.len());
                    for el in vec_ref.iter() {
                        if let Some(h) = from_amqp(el) {
                            out.push(h);
                        }
                    }
                    HeaderValue::Array(out)
                }
                AMQPValue::FieldTable(t) => {
                    let mut map = BTreeMap::new();
                    for (k, v) in t {
                        if let Some(h) = from_amqp(v) {
                            map.insert(k.as_str().to_string(), h);
                        }
                    }
                    HeaderValue::Table(map)
                }
                AMQPValue::Timestamp(ts) => HeaderValue::Int(*ts as i64),
                AMQPValue::ByteArray(bytes) => HeaderValue::Bytes(bytes.as_slice().to_vec()),
            })
        }

        let mut out = CoreProperties::default();

        if let Some(v) = p.content_type() {
            out.content_type = Some(v.as_str().to_string());
        }
        if let Some(v) = p.content_encoding() {
            out.content_encoding = Some(v.as_str().to_string());
        }
        if let Some(v) = p.delivery_mode() {
            out.delivery_mode = Some(v.cast_signed() as u8);
        }
        if let Some(v) = p.priority() {
            out.priority = Some(v.cast_signed() as u8);
        }
        if let Some(v) = p.correlation_id() {
            out.correlation_id = Some(v.as_str().to_string());
        }
        if let Some(v) = p.reply_to() {
            out.reply_to = Some(v.as_str().to_string());
        }
        if let Some(v) = p.expiration() {
            out.expiration = Some(v.as_str().to_string());
        }
        if let Some(v) = p.message_id() {
            out.message_id = Some(v.as_str().to_string());
        }
        if let Some(v) = p.timestamp() {
            out.timestamp = Some(v.cast_signed() as u64);
        }
        if let Some(v) = p.kind() {
            out.kind = Some(v.as_str().to_string());
        }
        if let Some(v) = p.user_id() {
            out.user_id = Some(v.as_str().to_string());
        }
        if let Some(v) = p.app_id() {
            out.app_id = Some(v.as_str().to_string());
        }
        if let Some(v) = p.cluster_id() {
            out.cluster_id = Some(v.as_str().to_string());
        }
        if let Some(h) = p.headers() {
            let mut map = BTreeMap::new();
            for (k, v) in h {
                if let Some(hv) = from_amqp(v) {
                    map.insert(k.as_str().to_string(), hv);
                }
            }
            out.headers = Some(map);
        }

        out
    }
}

impl CoreDelivery {
    pub fn from_lapin(d: lapin::message::Delivery) -> Self {
        let props = CoreProperties::from_lapin(&d.properties);
        Self {
            body: d.data,
            properties: props,
            exchange: d.exchange.as_str().to_string(),
            routing_key: d.routing_key.as_str().to_string(),
            redelivered: d.redelivered,
            delivery_tag: d.delivery_tag,
            acker: d.acker,
        }
    }

    pub fn ack(&self) -> bool {
        RUNTIME.block_on(async move {
            let options = BasicAckOptions { multiple: false };
            self.acker.ack(options).await.unwrap()
        })
    }

    pub fn nack(&self, requeue: bool) -> bool {
        RUNTIME.block_on(async move {
            let options = BasicNackOptions {
                multiple: false,
                requeue,
            };
            self.acker.nack(options).await.unwrap()
        })
    }

    pub fn reject(&self, requeue: bool) -> bool {
        RUNTIME.block_on(async move {
            let options = BasicRejectOptions { requeue };
            self.acker.reject(options).await.unwrap()
        })
    }
}
