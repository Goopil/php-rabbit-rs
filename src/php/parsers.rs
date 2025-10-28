use crate::core::connections::connection_options::{
    ConnectionOptions, ReconnectOptions, SslOptions,
};
use ext_php_rs::convert::FromZval;
use ext_php_rs::prelude::{PhpException, PhpResult};
#[allow(unused_imports)]
use ext_php_rs::props::Prop;
use ext_php_rs::types::{Iterable, ZendHashTable, Zval};
use lapin::types as ltypes;
use lapin::{options as lopt, ExchangeKind};
use std::path::PathBuf;

fn extract_iterable_options(mut arr: Option<Iterable>, mut on_kv: impl FnMut(&str, &Zval)) {
    if let Some(ref mut it) = arr {
        match it {
            Iterable::Array(ht) => {
                let mut iter = ht.iter();
                while let Some((k, v)) = iter.next() {
                    on_kv(&k.to_string(), v);
                }
            }
            Iterable::Traversable(tr) => {
                if let Some(mut iter) = tr.iter() {
                    while let Some((k, v)) = iter.next() {
                        if let Some(key) = String::from_zval(&k) {
                            on_kv(key.as_str(), v);
                        }
                    }
                }
            }
        }
    }
}

// Extract a FieldTable from a zval if it contains an arguments array
fn parse_field_table_from_zval(z: &Zval) -> Option<ltypes::FieldTable> {
    z.array().and_then(|ht| match ht_to_table_or_array(ht) {
        ltypes::AMQPValue::FieldTable(t) => Some(t),
        _ => None,
    })
}

// === Key constants ===

const K_HOST: &str = "host";
const K_PORT: &str = "port";
const K_USER: &str = "user";
const K_PASSWORD: &str = "password";
const K_VHOST: &str = "vhost";
const K_SSL: &str = "ssl";
const K_RECONNECT: &str = "reconnect";

const K_PASSIVE: &str = "passive";
const K_DURABLE: &str = "durable";
const K_EXCLUSIVE: &str = "exclusive";
const K_AUTO_DELETE: &str = "auto_delete";
const K_NOWAIT: &str = "nowait";
const K_ARGUMENTS: &str = "arguments";
const K_NO_LOCAL: &str = "no_local";
const K_CONSUMER_TAG: &str = "consumer_tag";
const K_NO_ACK: &str = "no_ack";
const K_REJECT_ON_EXCEPTION: &str = "reject_on_exception";
const K_REQUEUE_ON_REJECT: &str = "requeue_on_reject";
const K_INTERNAL: &str = "internal";
const K_MANDATORY: &str = "mandatory";
const K_GLOBAL: &str = "global";
const K_PREFETCH_SIZE: &str = "prefetch_size";
const K_IF_UNUSED: &str = "if_unused";
const K_IF_EMPTY: &str = "if_empty";
const K_ENABLED: &str = "enabled";
const K_MAX_RETRIES: &str = "max_retries";
const K_INITIAL_DELAY_MS: &str = "initial_delay_ms";
const K_MAX_DELAY_MS: &str = "max_delay_ms";
const K_TOTAL_TIMEOUT_MS: &str = "total_timeout_ms";
const K_JITTER: &str = "jitter";
const K_CAFILE: &str = "cafile";
const K_CAPATH: &str = "capath";
const K_VERIFY_PEER: &str = "verify_peer";
const K_VERIFY_PEER_NAME: &str = "verify_peer_name";
const K_PEER_NAME: &str = "peer_name";
const K_ALLOW_SELF_SIGNED: &str = "allow_self_signed";
const K_CERTFILE: &str = "certfile";
const K_KEYFILE: &str = "keyfile";
const K_PASSPHRASE: &str = "passphrase";
const K_PKCS12: &str = "pkcs12";
const K_HEARTBEAT: &str = "heartbeat";
const K_FRAME_MAX: &str = "frame_max";
const K_CONNECTION_TIMEOUT: &str = "connection_timeout";

// Convert zval into AMQPValue for headers/arguments (scalars, tables, arrays, bytes)
fn zval_to_amqp_value(z: &Zval) -> Option<ltypes::AMQPValue> {
    if z.is_null() {
        return Some(ltypes::AMQPValue::Void);
    }
    if let Some(b) = z.bool() {
        return Some(ltypes::AMQPValue::Boolean(b));
    }
    if let Some(i) = z.long() {
        return Some(ltypes::AMQPValue::LongLongInt(i));
    }
    if let Some(f) = z.double() {
        return Some(ltypes::AMQPValue::Double(f));
    }
    if let Some(s) = z.str() {
        return Some(ltypes::AMQPValue::LongString(ltypes::LongString::from(s)));
    }
    if let Some(bin) = z.binary() {
        return Some(ltypes::AMQPValue::ByteArray(bin.to_vec().into()));
    }
    if let Some(bs) = z.binary_slice() {
        return Some(ltypes::AMQPValue::ByteArray(bs.to_vec().into()));
    }
    if let Some(ht) = z.array() {
        return Some(ht_to_table_or_array(ht));
    }
    None
}

// Detect whether the ZendHashTable is a list (0..N-1) -> FieldArray, otherwise FieldTable
fn ht_to_table_or_array(ht: &ZendHashTable) -> ltypes::AMQPValue {
    let mut items: Vec<(Option<i64>, String, &Zval)> = Vec::new();
    let mut it = ht.iter();
    while let Some((k, v)) = it.next() {
        let key_s = k.to_string();
        let key_n = key_s.parse::<i64>().ok();
        items.push((key_n, key_s, v));
    }
    let is_list = if items.is_empty() {
        true
    } else {
        let mut all_num = true;
        let mut max = -1i64;
        for (kn, _, _) in &items {
            if let Some(n) = kn {
                if *n > max {
                    max = *n;
                }
            } else {
                all_num = false;
                break;
            }
        }
        all_num && max as usize == items.len().saturating_sub(1)
    };

    if is_list {
        items.sort_by_key(|(kn, _, _)| *kn);
        let mut v = Vec::with_capacity(items.len());
        for (_, _, zv) in items {
            if let Some(amqp) = zval_to_amqp_value(zv) {
                v.push(amqp);
            }
        }
        ltypes::AMQPValue::FieldArray(v.into())
    } else {
        let mut t = ltypes::FieldTable::default();
        for (_, ks, zv) in items {
            if let Some(amqp) = zval_to_amqp_value(zv) {
                t.insert(ltypes::ShortString::from(ks.as_str()), amqp);
            }
        }
        ltypes::AMQPValue::FieldTable(t)
    }
}

// === Connection / SSL / Reconnect parsers ===

pub fn parse_connection_options(arr: Option<Iterable>) -> PhpResult<ConnectionOptions> {
    let mut opts = ConnectionOptions::default();
    let mut err: Option<String> = None;

    extract_iterable_options(arr, |key, v| match key {
        K_HOST => {
            if let Some(s) = v.str() {
                opts.host = s.to_string();
            }
        }
        K_PORT => {
            if let Some(p) = v.long() {
                if (1..=65535).contains(&p) {
                    opts.port = Some(p as u16);
                } else {
                    err = Some("Invalid value given for argument `port`.".into());
                }
            } else if let Some(s) = v.str() {
                if let Ok(p) = s.parse::<u16>() {
                    if p >= 1 {
                        opts.port = Some(p);
                    } else {
                        err = Some("Invalid value given for argument `port`.".into());
                    }
                } else {
                    err = Some("Invalid value given for argument `port`.".into());
                }
            } else {
                err = Some("Invalid value given for argument `port`.".into());
            }
        }
        K_USER => {
            if let Some(s) = v.str() {
                opts.user = s.to_string();
            }
        }
        K_PASSWORD => {
            if let Some(s) = v.str() {
                opts.password = s.to_string();
            }
        }
        K_VHOST => {
            if let Some(s) = v.str() {
                opts.vhost = s.to_string();
            }
        }
        K_SSL => {
            if let Some(ht) = v.array() {
                match parse_ssl_options(Iterable::Array(ht)) {
                    Ok(ssl) => opts.ssl = Some(ssl),
                    Err(e) => err = Some(format!("{:?}", e)),
                }
            }
        }
        K_RECONNECT => {
            if let Some(ht) = v.array() {
                match parse_reconnect_options(Iterable::Array(ht)) {
                    Ok(r) => opts.reconnect = Some(r),
                    Err(e) => err = Some(format!("{:?}", e)),
                }
            }
        }
        K_HEARTBEAT => {
            if let Some(n) = v.long() {
                if n > 0 && n <= u16::MAX as i64 {
                    opts.heartbeat = Some(n as u16);
                } else if n == 0 {
                    opts.heartbeat = Some(0); // 0 = disable heartbeat
                } else {
                    err = Some("Invalid value for `heartbeat` (must be 0-65535 seconds)".into());
                }
            }
        }
        K_FRAME_MAX => {
            if let Some(n) = v.long() {
                if n >= 0 {
                    opts.frame_max = Some(n as u32);
                } else {
                    err = Some("Invalid value for `frame_max` (must be >= 0)".into());
                }
            }
        }
        K_CONNECTION_TIMEOUT => {
            if let Some(n) = v.long() {
                if n > 0 {
                    opts.connection_timeout_ms = Some(n as u64);
                } else {
                    err = Some("Invalid value for `connection_timeout` (must be > 0 ms)".into());
                }
            }
        }
        _ => {}
    });

    if let Some(msg) = err {
        return Err(PhpException::default(msg.into()));
    }

    Ok(opts)
}

fn parse_ssl_options(arr: Iterable) -> PhpResult<SslOptions> {
    let mut s = SslOptions::default();

    extract_iterable_options(Some(arr), |key, v| match key {
        K_CAFILE => {
            if let Some(sv) = v.str() {
                s.cafile = Some(PathBuf::from(sv));
            }
        }
        K_CAPATH => {
            if let Some(sv) = v.str() {
                s.capath = Some(PathBuf::from(sv));
            }
        }
        K_VERIFY_PEER => {
            s.verify_peer = v.bool();
        }
        K_VERIFY_PEER_NAME => {
            s.verify_peer_name = v.bool();
        }
        K_PEER_NAME => {
            s.peer_name = v.str().map(|s| s.to_string());
        }
        K_ALLOW_SELF_SIGNED => {
            s.allow_self_signed = v.bool();
        }
        K_CERTFILE => {
            if let Some(sv) = v.str() {
                s.certfile = Some(PathBuf::from(sv));
            }
        }
        K_KEYFILE => {
            if let Some(sv) = v.str() {
                s.keyfile = Some(PathBuf::from(sv));
            }
        }
        K_PASSPHRASE => {
            s.passphrase = v.str().map(|s| s.to_string());
        }
        K_PKCS12 => {
            if let Some(sv) = v.str() {
                s.pkcs12 = Some(PathBuf::from(sv));
            }
        }
        _ => {}
    });

    Ok(s)
}

fn parse_reconnect_options(arr: Iterable) -> PhpResult<ReconnectOptions> {
    let mut r = ReconnectOptions::default();
    let mut err: Option<String> = None;

    extract_iterable_options(Some(arr), |key, v| match key {
        K_ENABLED => {
            if let Some(b) = v.bool() {
                r.enabled = b;
            }
        }
        K_MAX_RETRIES => match v.long() {
            Some(n) => {
                if n < 0 {
                    err = Some("`reconnect.max_retries` must be >= 0".into());
                } else {
                    r.max_retries = n as u32;
                }
            }
            None => err = Some("Invalid value for `reconnect.max_retries`.".into()),
        },
        K_INITIAL_DELAY_MS => match v.long() {
            Some(n) => {
                if n < 0 {
                    err = Some("`reconnect.initial_delay_ms` must be >= 0".into());
                } else {
                    r.initial_delay_ms = n as u64;
                }
            }
            None => err = Some("Invalid value for `reconnect.initial_delay_ms`.".into()),
        },
        K_MAX_DELAY_MS => match v.long() {
            Some(n) => {
                if n < 0 {
                    err = Some("`reconnect.max_delay_ms` must be >= 0".into());
                } else {
                    r.max_delay_ms = n as u64;
                }
            }
            None => err = Some("Invalid value for `reconnect.max_delay_ms`.".into()),
        },
        K_TOTAL_TIMEOUT_MS => match v.long() {
            Some(n) => {
                if n < 0 {
                    err = Some("`reconnect.total_timeout_ms` must be >= 0".into());
                } else {
                    r.total_timeout_ms = n as u64;
                }
            }
            None => err = Some("Invalid value for `reconnect.total_timeout_ms`.".into()),
        },
        K_JITTER => match v.double() {
            Some(f) => {
                if !(0.0..=1.0).contains(&f) {
                    err = Some("`reconnect.jitter` must be between 0.0 and 1.0".into());
                } else {
                    r.jitter = f;
                }
            }
            None => err = Some("Invalid value for `reconnect.jitter`.".into()),
        },
        _ => {}
    });

    if let Some(msg) = err {
        return Err(PhpException::default(msg.into()));
    }

    Ok(r)
}

// === Queue parsers ===

pub fn parse_queue_declare_options(
    arr: Option<Iterable>,
) -> (lopt::QueueDeclareOptions, ltypes::FieldTable) {
    let mut opts = lopt::QueueDeclareOptions::default();
    let mut args = ltypes::FieldTable::default();

    extract_iterable_options(arr, |key, v| match key {
        K_PASSIVE => {
            if let Some(b) = v.bool() {
                opts.passive = b;
            }
        }
        K_DURABLE => {
            if let Some(b) = v.bool() {
                opts.durable = b;
            }
        }
        K_EXCLUSIVE => {
            if let Some(b) = v.bool() {
                opts.exclusive = b;
            }
        }
        K_AUTO_DELETE => {
            if let Some(b) = v.bool() {
                opts.auto_delete = b;
            }
        }
        K_NOWAIT => {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
        K_ARGUMENTS => {
            if let Some(t) = parse_field_table_from_zval(v) {
                args = t;
            }
        }
        _ => {}
    });

    (opts, args)
}

pub fn parse_queue_bind_options(
    arr: Option<Iterable>,
) -> (lopt::QueueBindOptions, ltypes::FieldTable) {
    let mut opts = lopt::QueueBindOptions::default();
    let mut args = ltypes::FieldTable::default();

    extract_iterable_options(arr, |key, v| match key {
        K_NOWAIT => {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
        K_ARGUMENTS => {
            if let Some(t) = parse_field_table_from_zval(v) {
                args = t;
            }
        }
        _ => {}
    });

    (opts, args)
}

pub fn parse_queue_delete_options(arr: Option<Iterable>) -> lopt::QueueDeleteOptions {
    let mut opts = lopt::QueueDeleteOptions::default();

    extract_iterable_options(arr, |key, v| match key {
        K_IF_UNUSED => {
            if let Some(b) = v.bool() {
                opts.if_unused = b;
            }
        }
        K_IF_EMPTY => {
            if let Some(b) = v.bool() {
                opts.if_empty = b;
            }
        }
        K_NOWAIT => {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
        _ => {}
    });

    opts
}

pub fn parse_queue_purge_options(arr: Option<Iterable>) -> lopt::QueuePurgeOptions {
    let mut opts = lopt::QueuePurgeOptions::default();

    extract_iterable_options(arr, |key, v| {
        if key == K_NOWAIT {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
    });

    opts
}

pub fn parse_queue_unbind_args(arr: Option<Iterable>) -> ltypes::FieldTable {
    match arr {
        Some(Iterable::Array(ht)) => {
            if let Some(arg_z) = ht.get(K_ARGUMENTS) {
                if let Some(t) = parse_field_table_from_zval(arg_z) {
                    return t;
                }
            }
            if let ltypes::AMQPValue::FieldTable(t) = ht_to_table_or_array(ht) {
                return t;
            }
            ltypes::FieldTable::default()
        }
        Some(Iterable::Traversable(tr)) => {
            if let Some(mut iter) = tr.iter() {
                while let Some((k, v)) = iter.next() {
                    if let Some(key) = String::from_zval(&k) {
                        if key.as_str() == K_ARGUMENTS {
                            if let Some(t) = parse_field_table_from_zval(v) {
                                return t;
                            }
                            break;
                        }
                    }
                }
            }
            ltypes::FieldTable::default()
        }
        None => ltypes::FieldTable::default(),
    }
}

// Exchange kind parser

pub fn parse_exchange_declare_kind(unknown_kind: String) -> PhpResult<ExchangeKind> {
    let kind = match unknown_kind.as_str() {
        "direct" => ExchangeKind::Direct,
        "fanout" => ExchangeKind::Fanout,
        "topic" => ExchangeKind::Topic,
        "headers" => ExchangeKind::Headers,
        other => {
            return Err(PhpException::default(format!(
                "Unsupported exchange type `{}`",
                other
            )))
        }
    };

    Ok(kind)
}

// === Exchange parsers ===

pub fn parse_exchange_declare_options(
    arr: Option<Iterable>,
) -> (lopt::ExchangeDeclareOptions, ltypes::FieldTable) {
    let mut opts = lopt::ExchangeDeclareOptions::default();
    let mut args = ltypes::FieldTable::default();
    extract_iterable_options(arr, |key, v| match key {
        K_PASSIVE => {
            if let Some(b) = v.bool() {
                opts.passive = b;
            }
        }
        K_DURABLE => {
            if let Some(b) = v.bool() {
                opts.durable = b;
            }
        }
        K_AUTO_DELETE => {
            if let Some(b) = v.bool() {
                opts.auto_delete = b;
            }
        }
        K_INTERNAL => {
            if let Some(b) = v.bool() {
                opts.internal = b;
            }
        }
        K_NOWAIT => {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
        K_ARGUMENTS => {
            if let Some(t) = parse_field_table_from_zval(v) {
                args = t;
            }
        }
        _ => {}
    });
    (opts, args)
}

pub fn parse_exchange_delete_options(arr: Option<Iterable>) -> lopt::ExchangeDeleteOptions {
    let mut opts = lopt::ExchangeDeleteOptions::default();

    extract_iterable_options(arr, |key, v| match key {
        K_IF_UNUSED => {
            if let Some(b) = v.bool() {
                opts.if_unused = b;
            }
        }
        K_NOWAIT => {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
        _ => {}
    });

    opts
}

pub fn parse_exchange_bind_options(
    arr: Option<Iterable>,
) -> (lopt::ExchangeBindOptions, ltypes::FieldTable) {
    let mut args = ltypes::FieldTable::default();
    let mut opts = lopt::ExchangeBindOptions::default();

    extract_iterable_options(arr, |key, v| match key {
        K_NOWAIT => {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
        K_ARGUMENTS => {
            if let Some(t) = parse_field_table_from_zval(v) {
                args = t;
            }
        }
        _ => {}
    });

    (opts, args)
}

pub fn parse_exchange_unbind_options(
    arr: Option<Iterable>,
) -> (lopt::ExchangeUnbindOptions, ltypes::FieldTable) {
    let mut args = ltypes::FieldTable::default();
    let mut opts = lopt::ExchangeUnbindOptions::default();

    extract_iterable_options(arr, |key, v| match key {
        K_NOWAIT => {
            if let Some(b) = v.bool() {
                opts.nowait = b;
            }
        }
        K_ARGUMENTS => {
            if let Some(t) = parse_field_table_from_zval(v) {
                args = t;
            }
        }
        _ => {}
    });

    (opts, args)
}

// === Publish parser ===

pub fn parse_publish_options(arr: Option<Iterable>) -> lopt::BasicPublishOptions {
    let mut o = lopt::BasicPublishOptions::default();
    extract_iterable_options(arr, |key, v| {
        if key == K_MANDATORY {
            if let Some(b) = v.bool() {
                o.mandatory = b;
            }
        }
    });
    o
}
// Extract message body as bytes from various zval representations

pub fn extract_body(body: &Zval) -> PhpResult<Vec<u8>> {
    if let Some(bs) = body.binary_slice() {
        return Ok(bs.to_vec());
    }
    if let Some(b) = body.binary() {
        return Ok(b.to_vec());
    }
    if let Some(s) = body.str() {
        return Ok(s.as_bytes().to_vec());
    }
    if let Some(zs) = body.string() {
        return Ok(zs.as_bytes().to_vec());
    }
    Err(PhpException::default(
        "Invalid value given for argument `body`.".into(),
    ))
}

// === Consume flags & policy ===

#[derive(Clone, Debug, Default)]
pub struct ConsumeFlags {
    pub no_local: bool,
    pub exclusive: bool,
    pub nowait: bool,
    pub consumer_tag: Option<String>,
}

// Parse consume flags (no_local, exclusive, nowait, consumer_tag)

pub fn parse_consume_flags(options: &Option<Iterable>) -> ConsumeFlags {
    let mut flags = ConsumeFlags::default();

    if let Some(Iterable::Array(arr)) = options {
        if let Some(v) = arr.get(K_NO_LOCAL) {
            if let Some(b) = v.bool() {
                flags.no_local = b;
            }
        }
        if let Some(v) = arr.get(K_EXCLUSIVE) {
            if let Some(b) = v.bool() {
                flags.exclusive = b;
            }
        }
        if let Some(v) = arr.get(K_NOWAIT) {
            if let Some(b) = v.bool() {
                flags.nowait = b;
            }
        }
        if let Some(v) = arr.get(K_CONSUMER_TAG) {
            if let Some(s) = v.str() {
                flags.consumer_tag = Some(s.to_string());
            }
        }
    }

    flags
}

#[derive(Clone, Debug)]
pub struct ConsumePolicy {
    pub no_ack: bool,
    pub reject_on_exception: bool,
    pub requeue_on_reject: bool,
}

impl Default for ConsumePolicy {
    fn default() -> Self {
        Self {
            no_ack: true, // Default: auto-ack, matching php-amqplib
            reject_on_exception: false,
            requeue_on_reject: false,
        }
    }
}

// Parse consume policy (no_ack, reject_on_exception, requeue_on_reject)

pub fn parse_consume_policy_options(options: &Option<Iterable>) -> ConsumePolicy {
    let mut policy = ConsumePolicy::default();
    let mut no_ack_set = false;

    if let Some(Iterable::Array(arr)) = options.as_ref() {
        if let Some(v) = arr.get(K_NO_ACK) {
            if let Some(b) = v.bool() {
                policy.no_ack = b;
                no_ack_set = true;
            }
        }
        if let Some(v) = arr.get(K_REJECT_ON_EXCEPTION) {
            if let Some(b) = v.bool() {
                policy.reject_on_exception = b;
            }
        }
        if let Some(v) = arr.get(K_REQUEUE_ON_REJECT) {
            if let Some(b) = v.bool() {
                policy.requeue_on_reject = b;
            }
        }
    }

    // Force manual mode only when reject_on_exception is enabled and no_ack was not provided
    if policy.reject_on_exception && !no_ack_set {
        policy.no_ack = false; // Enable manual acknowledgements so NACKs are allowed
    }

    policy
}

// === QoS parser ===

#[derive(Clone, Debug)]
pub struct QosOptions {
    pub global: bool,
    pub prefetch_size: Option<u32>,
}

impl Default for QosOptions {
    fn default() -> Self {
        Self {
            global: false,
            prefetch_size: Some(0),
        }
    }
}

// Parse QoS options (global, prefetch_size)

pub fn parse_qos_options(options: Option<Iterable>) -> QosOptions {
    let mut out = QosOptions::default();

    extract_iterable_options(options, |key, v| match key {
        K_GLOBAL => {
            if let Some(b) = v.bool() {
                out.global = b;
            }
        }
        K_PREFETCH_SIZE => {
            if let Some(n) = v.long() {
                out.prefetch_size = Some(n.max(0) as u32);
            } else if let Some(nf) = v.double() {
                out.prefetch_size = Some(nf.max(0.0) as u32);
            }
        }
        _ => {}
    });

    out
}
