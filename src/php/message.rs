use crate::core::channels::message::{CoreMessage, CoreProperties, HeaderValue};
use crate::php::parsers::extract_body;
#[allow(unused_imports)]
use ext_php_rs::props::Prop;
use ext_php_rs::types::ZendHashTable;
use ext_php_rs::types::{Iterable, Zval};
use ext_php_rs::{php_class, php_impl};
use std::collections::BTreeMap;
use std::sync::Arc;

#[php_class]
#[php(name = "Goopil\\RabbitRs\\AmqpMessage")]
pub struct AmqpMessage {
    pub inner: CoreMessage,
}

#[php_impl]
impl AmqpMessage {
    pub fn __construct(body: &Zval, properties: Option<Iterable>) -> Self {
        let bytes = extract_body(body).unwrap();
        let props = AmqpMessage::parse_properties(properties);

        Self {
            inner: CoreMessage {
                body: Arc::new(bytes),
                properties: Arc::new(props),
            },
        }
    }
}

impl AmqpMessage {
    fn zval_to_header_value(z: &Zval) -> Option<HeaderValue> {
        if z.is_null() {
            return Some(HeaderValue::Null);
        }
        if let Some(b) = z.bool() {
            return Some(HeaderValue::Bool(b));
        }
        if let Some(i) = z.long() {
            return Some(HeaderValue::Int(i));
        }
        if let Some(f) = z.double() {
            return Some(HeaderValue::Double(f));
        }
        if let Some(s) = z.str() {
            return Some(HeaderValue::String(s.to_string()));
        }
        // NEW: support binaire (PHP string binaire, non-UTF8)
        if let Some(bin) = z.binary() {
            return Some(HeaderValue::Bytes(bin.to_vec()));
        }
        if let Some(bs) = z.binary_slice() {
            return Some(HeaderValue::Bytes(bs.to_vec()));
        }
        if let Some(ht) = z.array() {
            return Some(AmqpMessage::ht_to_header_value(ht));
        }
        None
    }

    // Detect whether the PHP array is a list (0..N-1) -> Array, otherwise Table
    fn ht_to_header_value(ht: &ZendHashTable) -> HeaderValue {
        // Gather elements to decide between list and table
        let mut items: Vec<(Option<i64>, String, &Zval)> = Vec::new();
        let it = ht.iter();

        for (k, v) in it {
            let key_str = k.to_string();
            let key_num = key_str.parse::<i64>().ok();
            items.push((key_num, key_str, v));
        }

        let is_list = if items.is_empty() {
            true
        } else {
            let mut max = -1i64;
            let mut all_numeric = true;
            for (kn, _, _) in &items {
                if let Some(n) = kn {
                    if *n > max {
                        max = *n;
                    }
                } else {
                    all_numeric = false;
                    break;
                }
            }
            all_numeric && max as usize == items.len() - 1
        };

        if is_list {
            items.sort_by_key(|(kn, _, _)| *kn);
            let mut vec = Vec::with_capacity(items.len());
            for (_, _, v) in items {
                if let Some(hv) = AmqpMessage::zval_to_header_value(v) {
                    vec.push(hv);
                }
            }
            HeaderValue::Array(vec)
        } else {
            let mut map: BTreeMap<String, HeaderValue> = BTreeMap::new();
            for (_, ks, v) in items {
                if let Some(hv) = AmqpMessage::zval_to_header_value(v) {
                    map.insert(ks, hv);
                }
            }
            HeaderValue::Table(map)
        }
    }

    // ZendHashTable -> BTreeMap<String, HeaderValue> (pour top-level headers)
    fn ht_to_headers(ht: &ZendHashTable) -> BTreeMap<String, HeaderValue> {
        match AmqpMessage::ht_to_header_value(ht) {
            HeaderValue::Table(m) => m,
            HeaderValue::Array(vec) => {
                // Top-level headers must be a table: index as "0","1",...
                let mut m = BTreeMap::new();
                for (i, hv) in vec.into_iter().enumerate() {
                    m.insert(i.to_string(), hv);
                }
                m
            }
            _ => BTreeMap::new(),
        }
    }

    fn parse_properties(options: Option<Iterable>) -> CoreProperties {
        let mut p = CoreProperties::default();

        let Some(iter) = options else {
            return p;
        };

        match iter {
            Iterable::Array(opts) => {
                if let Some(v) = opts.get("content_type") {
                    p.content_type = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("content_encoding") {
                    p.content_encoding = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("delivery_mode") {
                    p.delivery_mode = v
                        .long()
                        .map(|n| n as u8)
                        .or_else(|| v.double().map(|d| d as u8));
                }
                if let Some(v) = opts.get("priority") {
                    p.priority = v
                        .long()
                        .map(|n| n as u8)
                        .or_else(|| v.double().map(|d| d as u8));
                }
                if let Some(v) = opts.get("correlation_id") {
                    p.correlation_id = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("reply_to") {
                    p.reply_to = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("expiration") {
                    p.expiration = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("message_id") {
                    p.message_id = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("timestamp") {
                    p.timestamp = v
                        .long()
                        .map(|n| n as u64)
                        .or_else(|| v.double().map(|d| d as u64));
                }
                if let Some(v) = opts.get("type") {
                    p.kind = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("user_id") {
                    p.user_id = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("app_id") {
                    p.app_id = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("cluster_id") {
                    p.cluster_id = v.str().map(|s| s.to_string());
                }
                if let Some(v) = opts.get("application_headers") {
                    if let Some(ht) = v.array() {
                        p.headers = Some(AmqpMessage::ht_to_headers(ht));
                    }
                }
            }
            _ => { /* Traversable: add branch if needed */ }
        }

        p
    }
}
