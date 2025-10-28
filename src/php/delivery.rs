use crate::core::channels::message::{CoreDelivery, HeaderValue};
use ext_php_rs::convert::IntoZval;
use ext_php_rs::exception::PhpResult;
use ext_php_rs::prelude::PhpException;
use ext_php_rs::{binary::Binary, php_class, php_impl, types::Zval};
use std::collections::HashMap;

#[php_class]
#[php(name = "Goopil\\RabbitRs\\AmqpDelivery")]
pub struct AmqpDelivery {
    delivery: CoreDelivery,
    no_ack: bool,
}

#[php_impl]
impl AmqpDelivery {
    pub fn get_body(&self) -> Binary<u8> {
        Binary::from(self.delivery.body.clone())
    }

    pub fn get_exchange(&self) -> String {
        self.delivery.exchange.clone()
    }

    pub fn get_routing_key(&self) -> String {
        self.delivery.routing_key.clone()
    }

    pub fn is_redelivered(&self) -> bool {
        self.delivery.redelivered
    }

    pub fn get_delivery_tag(&self) -> i64 {
        self.delivery.delivery_tag as i64
    }

    pub fn get_content_type(&self) -> Option<String> {
        self.delivery.properties.content_type.clone()
    }

    pub fn get_content_encoding(&self) -> Option<String> {
        self.delivery.properties.content_encoding.clone()
    }

    pub fn get_correlation_id(&self) -> Option<String> {
        self.delivery.properties.correlation_id.clone()
    }

    pub fn get_reply_to(&self) -> Option<String> {
        self.delivery.properties.reply_to.clone()
    }

    pub fn get_expiration(&self) -> Option<String> {
        self.delivery.properties.expiration.clone()
    }

    pub fn get_message_id(&self) -> Option<String> {
        self.delivery.properties.message_id.clone()
    }

    pub fn get_timestamp(&self) -> Option<i64> {
        self.delivery.properties.timestamp.map(|t| t as i64)
    }

    pub fn get_type(&self) -> Option<String> {
        self.delivery.properties.kind.clone()
    }

    pub fn get_user_id(&self) -> Option<String> {
        self.delivery.properties.user_id.clone()
    }

    pub fn get_app_id(&self) -> Option<String> {
        self.delivery.properties.app_id.clone()
    }

    pub fn get_cluster_id(&self) -> Option<String> {
        self.delivery.properties.cluster_id.clone()
    }

    pub fn get_priority(&self) -> Option<i64> {
        self.delivery.properties.priority.map(|p| p as i64)
    }

    pub fn get_delivery_mode(&self) -> Option<i64> {
        self.delivery.properties.delivery_mode.map(|m| m as i64)
    }

    pub fn get_headers(&self) -> HashMap<String, Zval> {
        let mut out: HashMap<String, Zval> = HashMap::new();

        if let Some(ref hdrs) = self.delivery.properties.headers {
            for (k, v) in hdrs {
                out.insert(k.to_string(), AmqpDelivery::header_value_to_zval(v));
            }
        }

        out
    }

    pub fn ack(&self) -> PhpResult<bool> {
        if self.no_ack {
            return Err(PhpException::default("Cannot ack in auto-ack mode".into()));
        }

        Ok(self.delivery.ack())
    }

    pub fn nack(&self, requeue: bool) -> PhpResult<bool> {
        if self.no_ack {
            return Err(PhpException::default("Cannot nack in auto-ack mode".into()));
        }

        Ok(self.delivery.nack(requeue))
    }

    pub fn reject(&self, requeue: bool) -> PhpResult<bool> {
        if self.no_ack {
            return Err(PhpException::default(
                "Cannot reject in auto-ack mode".into(),
            ));
        }

        Ok(self.delivery.reject(requeue))
    }
}

impl AmqpDelivery {
    pub fn new(delivery: CoreDelivery, no_ack: bool) -> Self {
        Self { delivery, no_ack }
    }

    fn header_value_to_zval(hv: &HeaderValue) -> Zval {
        match hv {
            HeaderValue::Null => {
                let mut z = Zval::new();
                z.set_null();
                z
            }
            HeaderValue::Bool(b) => {
                let mut z = Zval::new();
                z.set_bool(*b);
                z
            }
            HeaderValue::Int(i) => {
                let mut z = Zval::new();
                z.set_long(*i);
                z
            }
            HeaderValue::Double(f) => {
                let mut z = Zval::new();
                z.set_double(*f);
                z
            }
            HeaderValue::String(s) => {
                let mut z = Zval::new();
                let _ = z.set_string(s.as_str(), false);
                z
            }
            HeaderValue::Bytes(b) => {
                // Binary PHP string (Binary<u8> -> Zval)
                Binary::from(b.clone())
                    .into_zval(false)
                    .expect("bytes into_zval")
            }
            HeaderValue::Table(map) => {
                let mut m: HashMap<String, Zval> = HashMap::with_capacity(map.len());
                for (k, v) in map {
                    m.insert(k.clone(), AmqpDelivery::header_value_to_zval(v));
                }
                m.into_zval(false).expect("hashmap into_zval")
            }
            HeaderValue::Array(items) => {
                let mut v: Vec<Zval> = Vec::with_capacity(items.len());
                for it in items {
                    v.push(AmqpDelivery::header_value_to_zval(it));
                }
                v.into_zval(false).expect("vec into_zval")
            }
        }
    }
}
