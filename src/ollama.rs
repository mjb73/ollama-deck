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

/// Turn the server's technical error text into something a person can act on.
/// `None` means the message is already readable and is shown as it came.
fn plain_error(detail: &str) -> Option<String> {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("requires more system memory") {
        return Some(
            "Ollama could not fit the model in memory. Lower the context size or pick a smaller model, then try again."
                .to_string(),
        );
    }
    if lower.contains("out of memory") || lower.contains("outofmemory") {
        return Some(
            "Memory ran out while loading. Lower the context size or choose a smaller model."
                .to_string(),
        );
    }
    if lower.contains("no space left on device") {
        return Some("The disk is full, so Ollama could not unpack the model.".to_string());
    }
    if lower.contains("not found") && lower.contains("model") {
        return Some(
            "Ollama does not have that model. Pull it first, then reload the list.".to_string(),
        );
    }
    if lower.contains("context") && lower.contains("exceed") {
        return Some(
            "The chosen context is larger than this model supports. Pick a smaller one."
                .to_string(),
        );
    }
    None
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
            if let Some(plain) = plain_error(detail) {
                return plain.to_string();
            }
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

    let layers = find_number(&info, ".block_count")
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32;
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

/// What Ollama actually did with a load request, read back from `/api/ps`.
///
/// Ollama answers "done" even when it quietly capped the context at the
/// model's own limit or moved part of the model to system RAM, so the only
/// honest report is the one the server gives about the resident model.
#[derive(Clone, Debug, Default)]
pub struct LoadReport {
    pub resident: bool,
    pub ctx: u32,
    pub size: u64,
    pub size_vram: u64,
}

impl LoadReport {
    pub fn on_cpu(&self) -> u64 {
        self.size.saturating_sub(self.size_vram)
    }
}

/// Two model names are the same model when they agree after dropping the
/// implicit `:latest` tag.
pub fn same_model(a: &str, b: &str) -> bool {
    fn base(name: &str) -> &str {
        name.split(':').next().unwrap_or(name)
    }
    a == b || (base(a) == base(b) && (!a.contains(':') || !b.contains(':')))
}

/// Ollama reads a bare number as **seconds**, so `5` means five seconds, not
/// five minutes. Durations are sent as strings to keep the meaning explicit;
/// `-1` is the documented "never unload" value.
pub fn keep_alive_value(minutes: i64) -> Value {
    if minutes <= 0 {
        json!(-1)
    } else {
        json!(format!("{minutes}m"))
    }
}

pub fn load(model: &str, ctx: u32, keep_alive_minutes: i64) -> Result<LoadReport, String> {
    post_json(
        "/api/generate",
        json!({
            "model": model,
            "prompt": "",
            "stream": false,
            "keep_alive": keep_alive_value(keep_alive_minutes),
            "options": { "num_ctx": ctx }
        }),
    )?;

    // Give the server a moment to publish the new resident model, then read
    // back what it really holds in memory.
    for attempt in 0..4 {
        if let Ok(loaded) = ps() {
            if let Some(entry) = loaded.iter().find(|l| same_model(&l.name, model)) {
                return Ok(LoadReport {
                    resident: true,
                    ctx: entry.ctx,
                    size: entry.size,
                    size_vram: entry.size_vram,
                });
            }
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(LoadReport::default())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_alive_is_sent_as_a_duration_not_a_bare_number() {
        // A bare 5 is five *seconds* to Ollama, which unloads the model right
        // after it finished loading.
        assert_eq!(keep_alive_value(5), json!("5m"));
        assert_eq!(keep_alive_value(15), json!("15m"));
        assert_eq!(keep_alive_value(60), json!("60m"));
        assert_eq!(keep_alive_value(-1), json!(-1));
    }

    #[test]
    fn latest_tag_matches_its_bare_name() {
        assert!(same_model("llama3.2:3b", "llama3.2:3b"));
        assert!(same_model("gemma4", "gemma4:latest"));
        assert!(!same_model("llama3.2:3b", "llama3.2:1b"));
        assert!(!same_model("qwen2.5-coder:14b", "gemma4:latest"));
    }

    /// Talks to a real Ollama; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a running Ollama with llama3.2:3b"]
    fn live_load_reads_back_the_resident_context() {
        let model =
            std::env::var("OLLAMA_DECK_TEST_MODEL").unwrap_or_else(|_| "llama3.2:3b".to_string());
        let _ = unload(&model);
        let report = load(&model, 8192, 5).expect("load request");
        assert!(report.resident, "model should be resident after load");
        assert_eq!(report.ctx, 8192, "context should match the request");
        assert!(report.size_vram > 0, "model should occupy VRAM");

        // The old payload asked for 131072 on a 32768 model; Ollama caps it.
        let capped = load(&model, 131072, 5).expect("reload request");
        assert!(capped.resident);
        assert!(capped.ctx > 0 && capped.ctx <= 131072);

        let _ = unload(&model);
    }
}
