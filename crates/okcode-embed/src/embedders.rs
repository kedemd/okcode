//! Embedder profiles (lib/embedders.js) plus the ollama / OpenAI-compatible HTTP clients.
//! JS registers factories on an okdb instance; here a host supplies `Embedder` impls
//! and `build_embedder` makes the built-in HTTP ones from a profile's embedder config.
use crate::identity::{self, Parts};
use serde_json::{json, Map, Value};

pub const DERIVED_PREFIX: &str = "okcode-";

pub type Vector = Vec<f32>;

/// A host-supplied embedding function (the `embed` / `embedBatch` of a custom profile).
pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Result<Vector, String>;
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vector>, String> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// Host function receiving the workspace context, like `embed(texts, { workspace })`.
pub trait HostEmbed: Send + Sync {
    fn call(&self, texts: &[String], workspace: Option<&str>) -> Result<Vec<Vector>, String>;
}

/// Wraps a host function as an `Embedder` bound to one workspace (customFactory).
pub struct Custom<H: HostEmbed> {
    pub host: H,
    pub workspace: Option<String>,
}
impl<H: HostEmbed> Embedder for Custom<H> {
    fn embed(&self, text: &str) -> Result<Vector, String> {
        self.host.call(&[text.to_string()], self.workspace.as_deref())?.into_iter().next().ok_or_else(|| "embedder returned something that is not a vector".into())
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vector>, String> {
        let out = self.host.call(texts, self.workspace.as_deref())?;
        if out.len() == texts.len() {
            return Ok(out);
        }
        // One-text-at-a-time host after all.
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

// ── HTTP clients ────────────────────────────────────────────────────────────

fn field<'a>(cfg: &'a Value, k: &str) -> Option<&'a str> {
    cfg.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(120)).build()
}

fn to_vec(v: &Value) -> Result<Vector, String> {
    v.as_array()
        .ok_or("embedder returned something that is not a vector")?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| "embedder returned something that is not a vector".to_string()))
        .collect()
}

fn post(agent: &ureq::Agent, url: &str, key: Option<&str>, body: Value) -> Result<Value, String> {
    let mut r = agent.post(url).set("content-type", "application/json");
    if let Some(k) = key {
        r = r.set("authorization", &format!("Bearer {k}"));
    }
    r.send_json(body).map_err(|e| format!("embed request to {url} failed: {e}"))?.into_json().map_err(|e| e.to_string())
}

pub struct Ollama {
    pub url: String,
    pub model: String,
    agent: ureq::Agent,
}
impl Ollama {
    pub fn new(cfg: &Value) -> Ollama {
        Ollama {
            url: field(cfg, "url").unwrap_or("http://localhost:11434").trim_end_matches('/').to_string(),
            model: field(cfg, "model").unwrap_or("nomic-embed-text").to_string(),
            agent: agent(),
        }
    }
}
impl Embedder for Ollama {
    fn embed(&self, text: &str) -> Result<Vector, String> {
        self.embed_batch(&[text.to_string()])?.into_iter().next().ok_or_else(|| "empty response".into())
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vector>, String> {
        let r = post(&self.agent, &format!("{}/api/embed", self.url), None, json!({"model": self.model, "input": texts}))?;
        r["embeddings"].as_array().ok_or("ollama: no embeddings in response")?.iter().map(to_vec).collect()
    }
}

pub struct OpenAi {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub dims: Option<u64>,
    agent: ureq::Agent,
}
impl OpenAi {
    pub fn new(cfg: &Value) -> OpenAi {
        OpenAi {
            base_url: field(cfg, "base_url").or(field(cfg, "url")).unwrap_or("https://api.openai.com/v1").trim_end_matches('/').to_string(),
            model: field(cfg, "model").unwrap_or("text-embedding-3-small").to_string(),
            api_key: field(cfg, "api_key").map(str::to_owned),
            dims: cfg.get("dims").and_then(identity::dims_of_opt),
            agent: agent(),
        }
    }
}
impl Embedder for OpenAi {
    fn embed(&self, text: &str) -> Result<Vector, String> {
        self.embed_batch(&[text.to_string()])?.into_iter().next().ok_or_else(|| "empty response".into())
    }
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vector>, String> {
        let mut body = json!({"model": self.model, "input": texts});
        if let Some(d) = self.dims {
            body["dimensions"] = json!(d);
        }
        let r = post(&self.agent, &format!("{}/embeddings", self.base_url), self.api_key.as_deref(), body)?;
        let mut data: Vec<&Value> = r["data"].as_array().ok_or("openai: no data in response")?.iter().collect();
        data.sort_by_key(|d| d["index"].as_u64().unwrap_or(0));
        data.into_iter().map(|d| to_vec(&d["embedding"])).collect()
    }
}

/// Built-in provider from an embedder config (`type` = "ollama" | "openai").
pub fn build_embedder(cfg: &Value) -> Result<Box<dyn Embedder>, String> {
    match cfg.get("type").and_then(|v| v.as_str()) {
        Some("ollama") => Ok(Box::new(Ollama::new(cfg))),
        Some("openai") => Ok(Box::new(OpenAi::new(cfg))),
        t => Err(format!("okdb has no embedder factory \"{}\"", t.unwrap_or(""))),
    }
}

// ── profiles ────────────────────────────────────────────────────────────────

pub fn derived_type(name: &str) -> String {
    let s = identity::slug(name);
    format!("{DERIVED_PREFIX}{}", if s.is_empty() { "profile" } else { &s })
}

fn alias(k: &str) -> &str {
    match k {
        "apiKey" => "api_key",
        "baseUrl" => "base_url",
        k => k,
    }
}

/// The dims-free identity parts of a persisted profile record.
pub fn parts_of_record(rec: &Value) -> Option<Parts> {
    if rec.is_null() {
        return None;
    }
    let s = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(v) => identity::js_string(v),
    };
    if rec["kind"] == "custom" {
        return Some(Parts { r#type: "custom".into(), endpoint: s(rec.get("id")), model: s(rec.get("model")) });
    }
    let empty = json!({});
    let fields = rec.get("fields").filter(|v| !v.is_null()).unwrap_or(&empty);
    let model = fields.get("model").filter(|v| v.as_str() != Some("") && !v.is_null()).or(rec.get("model"));
    let ty = rec.get("provider").filter(|v| v.as_str().map_or(!v.is_null(), |s| !s.is_empty())).or(rec.get("type"));
    Some(Parts { r#type: s(ty), endpoint: identity::endpoint_of(fields), model: s(model) })
}

/// { type, endpoint, model, dims, identity } for a host.
pub fn describe(rec: &Value, dims: &Value) -> Value {
    let parts = parts_of_record(rec).unwrap_or_default();
    let d = identity::dims_of(dims).or_else(|| rec.get("dims").and_then(identity::dims_of));
    let nn = |s: &str| if s.is_empty() { Value::Null } else { json!(s) };
    json!({
        "type": nn(&parts.r#type), "endpoint": nn(&parts.endpoint), "model": nn(&parts.model),
        "dims": d, "identity": d.and_then(|d| identity::identity_of_dims(&parts, d)),
    })
}

/// A translated host profile: the store profile and the persisted record.
pub struct Translated {
    pub profile: Value,
    pub record: Value,
}

/// fromHost for built-in providers with a plain (string / absent) api key.
/// Function api keys and custom `embed` functions are registered by the host as `Embedder`s;
/// pass `key_fn = true` / use `from_host_custom` to get their records.
pub fn from_host(name: &str, cfg: &Value, key_fn: bool) -> Result<Translated, String> {
    if name.is_empty() {
        return Err("an embedder profile needs a name".into());
    }
    let obj = cfg.as_object().ok_or_else(|| format!("embedder \"{name}\": config must be an object"))?;
    let dims = obj.get("dims").and_then(identity::dims_of);
    let ty = obj.get("type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| format!("embedder \"{name}\": needs a type ('ollama', 'openai', …) or an embed function"))?;
    let mut fields = Map::new();
    let mut key_source: Option<&str> = if key_fn { Some("function") } else { None };
    for (k, v) in obj {
        if matches!(k.as_str(), "type" | "dims" | "pipeline") {
            continue;
        }
        if k == "apiKey" || k == "api_key" {
            if !key_fn && !v.is_null() && v.as_str() != Some("") {
                fields.insert("api_key".into(), json!(identity::js_string(v)));
                key_source = Some("string");
            }
            continue;
        }
        fields.insert(alias(k).to_string(), v.clone());
    }
    let mut safe = fields.clone();
    safe.remove("api_key");
    let pipeline = obj.get("pipeline").filter(|v| v.is_object()).cloned();
    let model = safe.get("model").filter(|v| truthy_json(v)).cloned().unwrap_or(Value::Null);
    let rec_type = if key_fn { derived_type(name) } else { ty.to_string() };
    let mut embedder = fields.clone();
    if key_fn {
        embedder = safe.clone();
    }
    embedder.insert("type".into(), json!(rec_type));
    if let Some(d) = dims {
        embedder.insert("dims".into(), json!(d));
    }
    let mut record = json!({
        "name": name, "kind": "builtin", "type": rec_type, "provider": ty, "model": model,
        "fields": Value::Object(safe), "dims": dims, "apiKey": key_source,
        "needs": if key_fn { json!("apiKey") } else { Value::Null },
    });
    let mut profile = json!({"name": name, "embedder": Value::Object(embedder), "dims": dims, "identity": parts_of_record(&record).map(|p| p.to_json())});
    if let Some(p) = pipeline {
        record["pipeline"] = p.clone();
        profile["pipeline"] = p;
    }
    Ok(Translated { profile, record })
}

fn truthy_json(v: &Value) -> bool {
    !(v.is_null() || v.as_str() == Some("") || v == &json!(false) || v == &json!(0))
}

/// fromHost for a custom `embed` function profile (the function itself is the host's `Embedder`).
pub fn from_host_custom(name: &str, cfg: &Value) -> Result<Translated, String> {
    let id = cfg.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| format!("embedder \"{name}\": a custom embed profile needs an `id` naming its vector space (e.g. 'my-service@v2') — change it whenever the function's vectors change"))?;
    let dims = cfg.get("dims").and_then(identity::dims_of);
    let ty = derived_type(name);
    let model = cfg.get("model").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let mut embedder = json!({"type": ty, "model": model.unwrap_or(&ty)});
    if let Some(d) = dims {
        embedder["dims"] = json!(d);
    }
    let record = json!({"name": name, "kind": "custom", "type": ty, "provider": "custom", "id": id, "model": model, "dims": dims, "needs": "embed"});
    let profile = json!({"name": name, "embedder": embedder, "dims": dims, "identity": parts_of_record(&record).map(|p| p.to_json())});
    Ok(Translated { profile, record })
}

/// Rebuild a profile from a persisted record (`needsConfig` when a host function is missing).
pub fn from_record(rec: &Value) -> Option<Value> {
    let name = rec.get("name").filter(|v| truthy_json(v))?;
    if let Some(needs) = rec.get("needs").filter(|v| truthy_json(v)) {
        let reason = if needs == "embed" { format!("custom embed function for \"{}\" not supplied to this process", identity::js_string(name)) } else { format!("apiKey function for \"{}\" not supplied to this process", identity::js_string(name)) };
        return Some(json!({"profile": null, "record": rec, "needsConfig": true, "reason": reason}));
    }
    let mut embedder = rec.get("fields").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    embedder.insert("type".into(), rec.get("type").cloned().unwrap_or(Value::Null));
    if let Some(d) = rec.get("dims").filter(|v| truthy_json(v)) {
        embedder.insert("dims".into(), d.clone());
    }
    let mut profile = json!({"name": name, "embedder": Value::Object(embedder), "dims": rec.get("dims").filter(|v| truthy_json(v)).cloned().unwrap_or(Value::Null), "identity": parts_of_record(rec).map(|p| p.to_json())});
    if let Some(p) = rec.get("pipeline").filter(|v| truthy_json(v)) {
        profile["pipeline"] = p.clone();
    }
    Some(json!({"profile": profile, "record": rec, "needsConfig": false}))
}

/// The model name the store derives the pipeline name from.
pub fn model_of(embedder: &Value) -> Option<String> {
    ["model", "type"].iter().find_map(|k| embedder.get(*k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned))
}
