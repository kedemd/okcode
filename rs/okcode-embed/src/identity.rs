//! The identity of a vector space: (type, endpoint, model, dims).
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

pub const PREFIX: &str = "code_";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parts {
    pub r#type: String,
    pub endpoint: String,
    pub model: String,
}

impl Parts {
    pub fn from_json(v: &Value) -> Parts {
        let g = |k: &str| v.get(k).map(js_string).unwrap_or_default();
        Parts { r#type: g("type"), endpoint: g("endpoint"), model: g("model") }
    }
    pub fn to_json(&self) -> Value {
        json!({"type": self.r#type, "endpoint": self.endpoint, "model": self.model})
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JS `String(v)` for the value shapes that occur (null/undefined -> "" is the caller's `|| ''`).
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `String(x || '')`
fn str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(v) if truthy(v) => js_string(v),
        _ => String::new(),
    }
}

pub fn endpoint_of(cfg: &Value) -> String {
    let url = cfg.get("url").filter(|v| truthy(v));
    let base = cfg.get("base_url").filter(|v| truthy(v));
    str_or_empty(url.or(base))
}

pub fn parts_of(embedder: &Value) -> Parts {
    Parts {
        r#type: str_or_empty(embedder.get("type")),
        endpoint: endpoint_of(embedder),
        model: str_or_empty(embedder.get("model")),
    }
}

/// A positive integer dims, or None (Number.isInteger(dims) && dims > 0).
pub fn dims_of(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().filter(|d| *d > 0).or_else(|| n.as_f64().filter(|f| f.fract() == 0.0 && *f > 0.0 && *f < 9e15).map(|f| f as u64)),
        _ => None,
    }
}

pub fn identity_of_dims(parts: &Parts, dims: u64) -> Option<String> {
    if parts.r#type.is_empty() {
        return None;
    }
    Some(serde_json::to_string(&json!([parts.r#type, parts.endpoint, parts.model, dims])).unwrap())
}

pub fn identity_of(parts: &Parts, dims: &Value) -> Option<String> {
    identity_of_dims(parts, dims_of(dims)?)
}

pub fn short_hash(s: &str) -> String {
    let d = Sha1::digest(s.as_bytes());
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// chunk.slug
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut pending = false;
    for c in s.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending {
                out.push('_');
                pending = false;
            }
            out.push(c);
        } else if !out.is_empty() {
            pending = true;
        }
    }
    out
}

fn name_slug(parts: &Parts) -> String {
    let s = slug(if parts.model.is_empty() { &parts.r#type } else { &parts.model });
    if s.is_empty() { "embedder".into() } else { s }
}

pub fn pipeline_name_dims(parts: &Parts, dims: u64) -> Option<String> {
    let id = identity_of_dims(parts, dims)?;
    Some(format!("{PREFIX}{}_{dims}_{}", name_slug(parts), short_hash(&id)))
}

pub fn pipeline_name(parts: &Parts, dims: &Value) -> Result<String, String> {
    let d = dims_of(dims);
    d.and_then(|d| pipeline_name_dims(parts, d)).ok_or_else(|| {
        format!("pipelineName needs a type and integer dims (got {}, {})", parts.r#type, if dims.is_null() { "undefined".to_string() } else { js_string(dims) })
    })
}

pub fn find_pipeline(names: &[String], parts: &Parts, dims: &Value) -> Option<(String, u64)> {
    if let Some(d) = dims_of(dims) {
        let want = pipeline_name_dims(parts, d)?;
        return names.contains(&want).then_some((want, d));
    }
    let head = format!("{PREFIX}{}_", name_slug(parts));
    for n in names {
        let Some(rest) = n.strip_prefix(&head) else { continue };
        let Some((ds, hash)) = rest.split_once('_') else { continue };
        if ds.is_empty() || !ds.bytes().all(|b| b.is_ascii_digit()) || hash.len() != 8 || !hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            continue;
        }
        let Ok(d) = ds.parse::<u64>() else { continue };
        if identity_of_dims(parts, d).map(|i| short_hash(&i)).as_deref() == Some(hash) {
            return Some((n.clone(), d));
        }
    }
    None
}

pub fn is_ours(name: &Value) -> bool {
    str_or_empty(Some(name)).starts_with(PREFIX)
}

pub fn dims_of_opt(v: &Value) -> Option<u64> {
    dims_of(v)
}
