//! Toxiproxy client for injecting network faults during chaos tests.
#![allow(dead_code)]

use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToxicType {
    /// Closes the connection by sending a TCP reset.
    ResetPeer,
    /// Stops all traffic (simulates a partition).
    Timeout,
    /// Adds latency.
    Latency,
    /// Limits bandwidth.
    Bandwidth,
}

impl ToxicType {
    fn as_str(self) -> &'static str {
        match self {
            Self::ResetPeer => "reset_peer",
            Self::Timeout => "timeout",
            Self::Latency => "latency",
            Self::Bandwidth => "bandwidth",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToxicSpec {
    pub name: &'static str,
    pub kind: ToxicType,
    pub direction: &'static str,
    pub toxicity: f32,
    pub timeout: Option<Duration>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToxicResponse {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub stream: String,
    pub toxicity: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<serde_json::Value>,
}

pub struct ToxiproxyClient {
    base_url: String,
    http: reqwest::Client,
}

impl ToxiproxyClient {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("toxiproxy http client");
        Self { base_url, http }
    }

    pub async fn add_toxic(&self, proxy: &str, spec: &ToxicSpec) -> Result<(), String> {
        let url = format!("{}/proxies/{}/toxics", self.base_url, proxy);

        let mut attributes = serde_json::Map::new();
        match spec.kind {
            ToxicType::ResetPeer => {
                if let Some(timeout) = spec.timeout {
                    attributes.insert(
                        "timeout".to_owned(),
                        serde_json::Value::Number(serde_json::Number::from(
                            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                        )),
                    );
                }
            }
            ToxicType::Timeout => {
                attributes.insert(
                    "timeout".to_owned(),
                    serde_json::Value::Number(serde_json::Number::from(
                        spec.timeout
                            .map_or(0, |t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX)),
                    )),
                );
            }
            ToxicType::Latency => {
                attributes.insert(
                    "latency".to_owned(),
                    serde_json::Value::Number(serde_json::Number::from(
                        spec.timeout
                            .map_or(100, |t| u64::try_from(t.as_millis()).unwrap_or(u64::MAX)),
                    )),
                );
            }
            ToxicType::Bandwidth => {
                attributes.insert(
                    "rate".to_owned(),
                    serde_json::Value::Number(serde_json::Number::from(1)),
                );
            }
        }

        let body = serde_json::json!({
            "name": spec.name,
            "type": spec.kind.as_str(),
            "stream": spec.direction,
            "toxicity": spec.toxicity,
            "attributes": attributes,
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("toxiproxy request failed: {e}"))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // If the toxic already exists, that's OK.
            if text.contains("already exists") || text.contains("409") {
                return Ok(());
            }
            Err(format!("toxiproxy add toxic failed ({status}): {text}"))
        }
    }

    pub async fn remove_toxic(&self, proxy: &str, name: &str) -> Result<(), String> {
        let url = format!("{}/proxies/{}/toxics/{}", self.base_url, proxy, name);

        let resp = self
            .http
            .delete(&url)
            .send()
            .await
            .map_err(|e| format!("toxiproxy request failed: {e}"))?;

        if resp.status().is_success() || resp.status().as_u16() == 404 {
            Ok(())
        } else {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            Err(format!("toxiproxy remove toxic failed ({status}): {text}"))
        }
    }

    pub async fn reset_all(&self) -> Result<(), String> {
        let url = format!("{}/reset", self.base_url);
        let resp = self
            .http
            .post(&url)
            .send()
            .await
            .map_err(|e| format!("toxiproxy reset failed: {e}"))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("toxiproxy reset failed: {}", resp.status()))
        }
    }

    pub async fn list_toxics(&self, proxy: &str) -> Result<Vec<ToxicResponse>, String> {
        let url = format!("{}/proxies/{}/toxics", self.base_url, proxy);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("toxiproxy request failed: {e}"))?;

        if resp.status().is_success() {
            let toxics: Vec<ToxicResponse> =
                resp.json().await.map_err(|e| format!("parse error: {e}"))?;
            Ok(toxics)
        } else {
            Err(format!("toxiproxy list failed: {}", resp.status()))
        }
    }
}
