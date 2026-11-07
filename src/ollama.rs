//! Minimal client for the Ollama HTTP API (localhost by default).

use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Debug, Default)]
pub struct ModelInfo {
    pub name: String,
    pub size: u64,
    pub params: String,
    pub quant: String,
    pub ctx_max: u32,
    pub cloud: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LoadedModel {
    pub name: String,
    pub size: u64,
    pub size_vram: u64,
    pub expires_at: String,
    pub ctx: u32,
}

#[derive(Clone, Debug, Default)]
pub struct ShowInfo {
    /// Total key/value heads summed over all layers.
    pub kv_heads_total: u32,
    pub key_len: u32,
    pub value_len: u32,
    pub ctx_max: u32,
}

impl ShowInfo {
    /// KV cache size in bytes for a given context window, assuming f16 cache.
    pub fn kv_bytes(&self, ctx: u32) -> u64 {
        if self.kv_heads_total == 0 || self.key_len == 0 {
            return 0;
        }
        let value_len = if self.value_len == 0 {
            self.key_len
        } else {
            self.value_len
        };
        ctx as u64 * self.kv_heads_total as u64 * (self.key_len + value_len) as u64 * 2
    }
}

pub fn base_url() -> String {
    std::env::var("OLLAMA_HOST")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|host| {
            let host = host.trim().trim_end_matches('/');
            if host.starts_with("http://") || host.starts_with("https://") {
                host.to_string()
            } else {
                format!("http://{host}")
            }
        })
        .unwrap_or_else(|| "http://127.0.0.1:11434".to_string())
}

pub fn pretty_endpoint() -> String {
    base_url()
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string()
}

fn err_text(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.as_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or(body);
            let detail = detail.trim();
            if detail.is_empty() {
                format!("HTTP {code}")
            } else {
                format!("{detail} (HTTP {code})")
            }
        }
        ureq::Error::Transport(t) => {
            let text = t.to_string();
            if text.contains("Connection refused") || text.contains("connection refused") {
                "Ollama is not answering - is it running?".to_string()
            } else {
                text
            }
        }
    }
}

fn get_json(path: &str) -> Result<Value, String> {
    let url = format!("http://{}{}", pretty_endpoint(), path);
    let response = ureq::get(&url)
        .timeout(Duration::from_secs(4))
        .call()
        .map_err(err_text)?;
    response.into_json::<Value>().map_err(|e| e.to_string())
}

fn post_json(path: &str, body: Value) -> Result<Value, String> {
    let url = format!("http://{}{}", pretty_endpoint(), path);
    let response = ureq::post(&url)
        .timeout(Duration::from_secs(900))
        .send_json(body)
        .map_err(err_text)?;
    response.into_json::<Value>().map_err(|e| e.to_string())
}

pub fn version() -> Result<String, String> {
    let value = get_json("/api/version")?;
    Ok(value
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string())
}

pub fn list_models() -> Result<Vec<ModelInfo>, String> {
    let value = get_json("/api/tags")?;
    let mut models = Vec::new();
    if let Some(items) = value.get("models").and_then(|m| m.as_array()) {
        for item in items {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let details = item.get("details").cloned().unwrap_or(Value::Null);
            let get_str = |key: &str| {
                details
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let ctx_max = details
                .get("context_length")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .min(u32::MAX as u64) as u32;
            let cloud = item
                .get("remote_host")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
                || name.ends_with(":cloud")
                || name.ends_with("-cloud");
            models.push(ModelInfo {
                name,
                size: item.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                params: get_str("parameter_size"),
                quant: get_str("quantization_level"),
                ctx_max,
                cloud,
            });
        }
    }
    models.sort_by(|a, b| a.cloud.cmp(&b.cloud).then_with(|| a.name.cmp(&b.name)));
    Ok(models)
}

pub fn ps() -> Result<Vec<LoadedModel>, String> {
    let value = get_json("/api/ps")?;
    let mut loaded = Vec::new();
    if let Some(items) = value.get("models").and_then(|m| m.as_array()) {
        for item in items {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            loaded.push(LoadedModel {
                name,
                size: item.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                size_vram: item.get("size_vram").and_then(|v| v.as_u64()).unwrap_or(0),
                expires_at: item
                    .get("expires_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                ctx: item
                    .get("context_length")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .min(u32::MAX as u64) as u32,
            });
        }
    }
    Ok(loaded)
}

fn find_number(map: &serde_json::Map<String, Value>, suffix: &str) -> Option<u64> {
    for (key, value) in map {
        if key.ends_with(suffix) {
            if let Some(n) = value.as_u64() {
                return Some(n);
            }
        }
    }
    None
}

fn find_array(map: &serde_json::Map<String, Value>, suffix: &str) -> Option<Vec<u64>> {
    for (key, value) in map {
        if key.ends_with(suffix) {
            if let Some(items) = value.as_array() {
                let numbers: Vec<u64> = items.iter().filter_map(|v| v.as_u64()).collect();
                if !numbers.is_empty() {
                    return Some(numbers);
                }
            }
        }
    }
    None
}

pub fn show(model: &str) -> Result<ShowInfo, String> {
    let value = post_json("/api/show", json!({ "model": model }))?;
    let info = value
        .get("model_info")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let layers = find_number(&info, ".block_count").unwrap_or(0).min(u32::MAX as u64) as u32;
    let head_count = find_number(&info, ".attention.head_count")
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let embedding = find_number(&info, ".embedding_length").unwrap_or(0);
    let key_len = find_number(&info, ".attention.key_length")
        .or_else(|| {
            if head_count > 0 && embedding > 0 {
                Some(embedding / head_count as u64)
            } else {
                None
            }
        })
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
    let value_len = find_number(&info, ".attention.value_length")
        .unwrap_or(key_len as u64)
        .min(u32::MAX as u64) as u32;

    let kv_heads_total = if let Some(per_layer) = find_array(&info, ".attention.head_count_kv") {
        per_layer.iter().sum::<u64>().min(u32::MAX as u64) as u32
    } else {
        find_number(&info, ".attention.head_count_kv")
            .unwrap_or(0)
            .saturating_mul(layers as u64)
            .min(u32::MAX as u64) as u32
    };

    let ctx_max = find_number(&info, ".context_length")
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;

    Ok(ShowInfo {
        kv_heads_total,
        key_len,
        value_len,
        ctx_max,
    })
}

pub fn load(model: &str, ctx: u32, keep_alive_minutes: i64) -> Result<(), String> {
    post_json(
        "/api/generate",
        json!({
            "model": model,
            "prompt": "",
            "stream": false,
            "keep_alive": keep_alive_minutes,
            "options": { "num_ctx": ctx }
        }),
    )
    .map(|_| ())
}

pub fn unload(model: &str) -> Result<(), String> {
    post_json(
        "/api/generate",
        json!({
            "model": model,
            "prompt": "",
            "stream": false,
            "keep_alive": 0
        }),
    )
    .map(|_| ())
}
