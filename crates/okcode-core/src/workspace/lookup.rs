//! Lookups and reads.
//!
//! carveRegions/cutLines/splitPiece/labelPieces, parseRange/pathEndsWith,
//! list/byRel/ownerOf/windowAt/fileOf, resolve/textOfFresh/symbolText/sliceSymbol/analysisOf;
//! api structure, find, files, symbols, outline, read, readFile, readAt.
//! An unindexed workspace (or a library path) delegates to library.rs (`Workspace::library_*`).
//! extensionFor is hardcoded to okcode_analysis::okjs (the only registered extension);
//! libraryPath has a local twin, lk_library_path, to carry the {error} branch (library_path cannot).
use super::library::ReadLibraryOpts;
use super::{Workspace, MENTIONS_REASON_CAP, MIN_PIECE_LINES};
use crate::error::{Error, Result};
use crate::record::FileRecord;
use crate::store::quote_lines;
use indexmap::IndexMap;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

#[derive(Default)]
pub struct LookupState {}

/// JS `Number(x) > 0 ? Math.floor(Number(x)) : 0` over any JSON value.
fn js_pos(v: &Value) -> Option<usize> {
    let f = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse::<f64>().ok()? }
        }
        Value::Bool(true) => 1.0,
        _ => return None,
    };
    if f > 0.0 { Some(f.floor() as usize) } else { None }
}
fn de_off<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(js_pos(&v).unwrap_or(0))
}
/// A limit: a positive number wins; anything else (null, NaN, <= 0) is "no limit given" (0).
fn de_lim<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<usize, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(js_pos(&v).unwrap_or(0))
}
fn de_line<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<i64>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(num_arg(&v))
}
/// JS truthiness of a JSON value.
fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
/// A strict-compare filter (`kind`, `lang`): falsy = no filter; a non-string truthy value matches
/// nothing (the sentinel never equals a real kind/lang).
fn de_kind<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<String>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match &v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        other if js_truthy(other) => Some("\u{0}<no match>".to_string()),
        _ => None,
    })
}
/// `String(dir || '')`
fn de_dir<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<String>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(if js_truthy(&v) { Some(js_string(&v)) } else { None })
}
fn de_truthy<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    Ok(js_truthy(&Value::deserialize(d)?))
}
/// maxLines: absent = default (the field stays None); present (even null) = Number(x), NaN/negative as 0.
fn de_max_lines<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<usize>, D::Error> {
    let v = Value::deserialize(d)?;
    let f = match &v {
        Value::Null => 0.0,
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::Bool(b) => *b as u8 as f64,
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(0.0) }
        }
        _ => 0.0,
    };
    Ok(Some(if f > 0.0 { f.floor() as usize } else { 0 }))
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StructureOpts {
    #[serde(deserialize_with = "de_dir")]
    pub dir: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FindOpts {
    #[serde(deserialize_with = "de_kind")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "de_lim")]
    pub limit: usize,
    #[serde(deserialize_with = "de_off")]
    pub offset: usize,
    pub path: Option<Value>,
}
impl Default for FindOpts {
    fn default() -> Self {
        FindOpts { kind: None, limit: 20, offset: 0, path: None }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FilesOpts {
    #[serde(deserialize_with = "de_dir")]
    pub dir: Option<String>,
    #[serde(deserialize_with = "de_kind")]
    pub lang: Option<String>,
    #[serde(deserialize_with = "de_lim")]
    pub limit: usize,
    #[serde(deserialize_with = "de_off")]
    pub offset: usize,
    #[serde(deserialize_with = "de_truthy")]
    pub stat: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SymbolsOpts {
    #[serde(deserialize_with = "de_lim")]
    pub limit: usize,
    #[serde(deserialize_with = "de_off")]
    pub offset: usize,
    #[serde(deserialize_with = "de_kind")]
    pub kind: Option<String>,
    #[serde(deserialize_with = "de_truthy")]
    pub stat: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OutlineOpts {
    #[serde(deserialize_with = "de_lim")]
    pub limit: usize,
    #[serde(deserialize_with = "de_off")]
    pub offset: usize,
}
impl Default for OutlineOpts {
    fn default() -> Self {
        OutlineOpts { limit: 80, offset: 0 }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReadOpts {
    #[serde(deserialize_with = "de_line")]
    pub from: Option<i64>,
    #[serde(deserialize_with = "de_line")]
    pub to: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReadFileOpts {
    /// None = opts.fileReadLines
    #[serde(deserialize_with = "de_max_lines")]
    pub max_lines: Option<usize>,
}

/// parseRange(query) → `{ file, from, to }`
#[derive(Clone, Debug, PartialEq)]
pub struct Range {
    pub file: String,
    pub from: i64,
    pub to: i64,
}

// ── JS string helpers (JS strings are UTF-16; offsets recorded by the parsers are UTF-16) ──────

fn jlen(s: &str) -> usize {
    if s.is_ascii() {
        s.len()
    } else {
        s.encode_utf16().count()
    }
}

/// `s.slice(a, b)` over UTF-16 code units (clamped like JS).
pub fn js_slice(s: &str, a: usize, b: usize) -> String {
    if s.is_ascii() {
        let (a, b) = (a.min(s.len()), b.min(s.len()));
        return if a >= b { String::new() } else { s[a..b].to_string() };
    }
    let u: Vec<u16> = s.encode_utf16().collect();
    let (a, b) = (a.min(u.len()), b.min(u.len()));
    if a >= b {
        String::new()
    } else {
        String::from_utf16_lossy(&u[a..b])
    }
}

fn is_js_space(c: char) -> bool {
    c != '\u{85}' && (c.is_whitespace() || c == '\u{FEFF}')
}
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// JS `String(x)`.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn vstr<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}
/// JS truthiness of a string-valued field.
fn vstr_t<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    vstr(v, k).filter(|s| !s.is_empty())
}
fn vnum(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_f64).map(|f| f as i64)
}
fn vfloat(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(Value::as_f64).unwrap_or(0.0)
}
fn sym_path(s: &Value) -> String {
    vstr_t(s, "path").or_else(|| vstr_t(s, "name")).unwrap_or("").to_string()
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        _ => true,
    }
}
/// Number(x) for a line argument; NaN/null/0 → None (falsy).
fn num_arg(v: &Value) -> Option<i64> {
    let f = match v {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        Value::Bool(true) => 1.0,
        _ => return None,
    };
    if f == 0.0 || f.is_nan() {
        None
    } else {
        Some(f as i64)
    }
}

/// An array result that carries own properties in JS (`res.total = …`): the binding's JS layer
/// turns `{"$arr": […], "$props": {…}}` into an Array with those properties.
fn lk_arr(items: Vec<Value>, props: Vec<(&str, Value)>) -> Value {
    let mut m = Map::new();
    for (k, v) in props {
        m.insert(k.to_string(), v);
    }
    let mut o = Map::new();
    o.insert("$arr".into(), Value::Array(items));
    o.insert("$props".into(), Value::Object(m));
    Value::Object(o)
}

/// cappedList's tail: (how many to show, the ` (showing n of m…)` clause).
fn lk_more(total: usize, cap: usize, hint: Option<&str>) -> (usize, String) {
    let shown = total.min(cap);
    if total > shown {
        let h = hint.map(|h| format!("; {h}")).unwrap_or_default();
        (shown, format!(" (showing {shown} of {total}{h})"))
    } else {
        (shown, String::new())
    }
}

fn lines_join(lines: &[&str], lo: i64, hi: i64) -> String {
    let a = (lo - 1).max(0) as usize;
    let b = (hi.max(0) as usize).min(lines.len());
    if a >= b {
        String::new()
    } else {
        lines[a..b].join("\n")
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

fn ikey(k: &str) -> String {
    k.to_string()
}

// ── pure helpers (some exported by workspace.js) ────────────────────────

/// carveRegions(regions): nested regions carved to what each alone covers (JSON in, JSON out).
pub fn carve_regions(regions: &[Value]) -> Vec<Value> {
    let mut sorted: Vec<&Value> = regions.iter().collect();
    sorted.sort_by(|a, b| {
        vfloat(a, "lineStart")
            .partial_cmp(&vfloat(b, "lineStart"))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(vfloat(b, "lineEnd").partial_cmp(&vfloat(a, "lineEnd")).unwrap_or(std::cmp::Ordering::Equal))
    });
    let ls = |v: &Value| vnum(v, "lineStart").unwrap_or(0);
    let le = |v: &Value| vnum(v, "lineEnd").unwrap_or(0);
    let mut out: Vec<Value> = Vec::new();
    for (ri, r) in sorted.iter().enumerate() {
        let span = le(r) - ls(r);
        // STRICTLY inside: two regions over the identical range must not erase each other.
        let inner: Vec<&&Value> = sorted
            .iter()
            .enumerate()
            .filter(|(oi, o)| *oi != ri && ls(o) >= ls(r) && le(o) <= le(r) && le(o) - ls(o) < span)
            .map(|(_, o)| o)
            .collect();
        if inner.is_empty() {
            out.push((*r).clone());
            continue;
        }
        let mut gaps: Vec<(i64, i64)> = Vec::new();
        let mut at = ls(r);
        for o in inner {
            if ls(o) > at {
                gaps.push((at, ls(o) - 1));
            }
            at = at.max(le(o) + 1);
        }
        if at <= le(r) {
            gaps.push((at, le(r)));
        }
        for (a, b) in gaps {
            if b - a + 1 >= MIN_PIECE_LINES as i64 {
                let mut c = (*r).clone();
                if let Some(m) = c.as_object_mut() {
                    m.insert("lineStart".into(), json!(a));
                    m.insert("lineEnd".into(), json!(b));
                }
                out.push(c);
            }
        }
    }
    out.sort_by(|a, b| vfloat(a, "lineStart").partial_cmp(&vfloat(b, "lineStart")).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// cutLines(text): the 1-based start line of each chunkCode piece.
pub fn cut_lines(text: &str) -> Vec<usize> {
    let chunks = okcode_analysis::chunk::chunk::chunk_code(text, None);
    if chunks.is_empty() {
        return Vec::new();
    }
    let u: Vec<u16> = text.encode_utf16().collect();
    let mut nl = Vec::with_capacity(u.len() + 1);
    let mut n = 0usize;
    nl.push(0usize);
    for &c in &u {
        if c == 10 {
            n += 1;
        }
        nl.push(n);
    }
    chunks.iter().map(|c| nl[c.start.min(u.len())] + 1).collect()
}

/// splitPiece(a, b, text, max) → `[[a, b], …]`
pub fn split_piece(a: usize, b: usize, text: &str, max: usize) -> Vec<(usize, usize)> {
    let total = b + 1 - a.min(b + 1);
    if total <= max || max == 0 {
        return vec![(a, b)];
    }
    let mut edges: Vec<usize> = vec![1];
    edges.extend(cut_lines(text).into_iter().filter(|&c| c > 1 && c <= total));
    edges.push(total + 1);
    edges.sort_unstable();
    edges.dedup();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut start = edges[0];
    for i in 1..edges.len() {
        if edges[i] - start >= max || i == edges.len() - 1 {
            runs.push((start, edges[i] - 1));
            start = edges[i];
        }
    }
    let mut out = Vec::new();
    for (s, e) in runs {
        let mut n = s;
        while n <= e {
            out.push((a + n - 1, a + e.min(n + max - 1) - 1));
            n += max;
        }
    }
    out
}

/// labelPieces(pieces, lines)
pub fn label_pieces(pieces: Vec<Value>, lines: &[&str]) -> Vec<Value> {
    // Keyed by the actual value (JS Map key semantics): missing, "" and numbers stay distinct.
    let name_of = |p: &Value| match p.get("name") {
        None => "u:".to_string(),
        Some(Value::String(s)) => format!("s:{s}"),
        Some(v) => format!("v:{v}"),
    };
    let mut seen: HashMap<String, usize> = HashMap::new();
    for p in &pieces {
        *seen.entry(name_of(p)).or_insert(0) += 1;
    }
    let mut n: HashMap<String, usize> = HashMap::new();
    pieces
        .into_iter()
        .map(|p| {
            let ls = vnum(&p, "lineStart").unwrap_or(1);
            let le = vnum(&p, "lineEnd").unwrap_or(ls);
            let a = (ls - 1).max(0) as usize;
            let b = (le.max(0) as usize).min(lines.len());
            let first = if a < b { lines[a..b].iter().find(|l| !js_trim(l).is_empty()).copied().unwrap_or("") } else { "" };
            let head = js_slice(js_trim(first), 0, 120);
            let mut parts: Vec<String> = Vec::new();
            if let Some(d) = vstr_t(&p, "doc") {
                parts.push(d.to_string());
            }
            if !head.is_empty() {
                parts.push(head);
            }
            let doc = if parts.is_empty() { Value::Null } else { Value::String(parts.join(" — ")) };
            let mut o = p.as_object().cloned().unwrap_or_default();
            o.insert("doc".into(), doc);
            let name = name_of(&Value::Object(o.clone()));
            if seen.get(&name).copied().unwrap_or(0) >= 2 {
                let i = n.entry(name.clone()).or_insert(0);
                *i += 1;
                let base = vstr(&Value::Object(o.clone()), "name").unwrap_or("").to_string();
                o.insert("name".into(), json!(format!("{base} §{i}")));
            }
            Value::Object(o)
        })
        .collect()
}

/// parseRange(query): `file:12` / `file:12-30` → Range; None otherwise.
pub fn parse_range(query: &str) -> Option<Range> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^([^\n\r\u{2028}\u{2029}]*?)[:#]([0-9]+)(?:-([0-9]+))?$").unwrap());
    let m = re.captures(query)?;
    let head = m.get(1)?.as_str();
    if jlen(head) <= 1 {
        return None;
    }
    let num = |s: &str| s.parse::<f64>().unwrap_or(0.0) as i64;
    let from = num(m.get(2)?.as_str());
    let to = m.get(3).map(|x| num(x.as_str())).unwrap_or(from);
    Some(Range { file: head.to_string(), from, to })
}

/// pathEndsWith(sym, hint)
pub fn path_ends_with(sym: &Value, hint: &str) -> bool {
    let p = sym_path(sym);
    hint.contains('.') && jlen(&p) > jlen(hint) && p.ends_with(&format!(".{hint}"))
}

/// ownerOf(file, line): the innermost symbol containing the line.
pub fn owner_of<'a>(file: &'a FileRecord, line: i64) -> Option<&'a Value> {
    let mut best: Option<&Value> = None;
    for s in &file.symbols {
        let (Some(a), Some(b)) = (vnum(s, "lineStart"), vnum(s, "lineEnd")) else { continue };
        if line < a || line > b {
            continue;
        }
        let better = match best {
            None => true,
            Some(cur) => b - a < vnum(cur, "lineEnd").unwrap_or(0) - vnum(cur, "lineStart").unwrap_or(0),
        };
        if better {
            best = Some(s);
        }
    }
    best
}

/// symbolText(src, symbol): the exact bytes by the recorded offsets, else the line range.
pub fn symbol_text(src: &str, symbol: &Value) -> String {
    let int = |k: &str| symbol.get(k).and_then(Value::as_f64).filter(|f| f.fract() == 0.0);
    if let (Some(a), Some(b)) = (int("start"), int("end")) {
        let a = a.max(0.0) as usize;
        let b = b.max(0.0) as usize;
        return js_slice(src, a, b);
    }
    let ls = vnum(symbol, "lineStart").unwrap_or(1);
    let le = vnum(symbol, "lineEnd").unwrap_or(ls);
    let lines: Vec<&str> = src.split('\n').collect();
    lines_join(&lines, ls.max(1), le)
}

static RANGE_TAIL: OnceLock<Regex> = OnceLock::new();
fn range_tail(s: &str) -> bool {
    RANGE_TAIL.get_or_init(|| Regex::new(r"^[0-9]+(?:-[0-9]+)?$").unwrap()).is_match(s)
}

fn base_name(p: &str) -> String {
    p.replace('\\', "/").rsplit('/').next().unwrap_or("").to_string()
}
fn rel_base(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or("")
}

fn name_score(q: &str, name: Option<&str>, sym_path: Option<&str>) -> i64 {
    let mut best = 0;
    for c in [name, sym_path].into_iter().flatten() {
        if c.is_empty() {
            continue;
        }
        let n = c.to_lowercase();
        let score = if n == q {
            100
        } else if n.starts_with(q) {
            70
        } else if n.contains(q) {
            50
        } else {
            0
        };
        if score > best {
            best = score;
        }
    }
    best
}

/// symbolHit(f, s, extra)
fn symbol_hit(f: &FileRecord, s: &Value, extra: Vec<(&str, Value)>) -> Map<String, Value> {
    let ls = vnum(s, "lineStart").unwrap_or(0);
    let le = vnum(s, "lineEnd").unwrap_or(0);
    let mut m = Map::new();
    m.insert(ikey("name"), s.get("name").cloned().unwrap_or(Value::Null));
    m.insert(ikey("kind"), s.get("kind").cloned().unwrap_or(Value::Null));
    m.insert(ikey("file"), json!(f.rel));
    m.insert(ikey("path"), json!(sym_path(s)));
    m.insert(ikey("parent"), vstr_t(s, "parent").map(|p| json!(p)).unwrap_or(Value::Null));
    m.insert(ikey("lines"), json!(format!("{ls}-{le}")));
    m.insert(ikey("span"), json!(le - ls + 1));
    m.insert(ikey("signature"), vstr_t(s, "signature").map(|p| json!(p)).unwrap_or(Value::Null));
    let doc = js_slice(vstr(s, "doc").unwrap_or(""), 0, 160);
    m.insert(ikey("doc"), if doc.is_empty() { Value::Null } else { json!(doc) });
    m.insert(ikey("exported"), json!(truthy(s.get("exported"))));
    m.insert(ikey("at"), f.hash.as_ref().map(|h| json!(h)).unwrap_or(Value::Null));
    for (k, v) in extra {
        m.insert(k.to_string(), v);
    }
    m
}

fn hit_score(m: &Map<String, Value>) -> f64 {
    m.get("score").and_then(Value::as_f64).unwrap_or(0.0)
}
fn hit_rel(m: &Map<String, Value>) -> f64 {
    m.get("relevance").and_then(Value::as_f64).unwrap_or(0.0)
}
fn hit_span(m: &Map<String, Value>) -> f64 {
    m.get("span").and_then(Value::as_f64).unwrap_or(0.0)
}

/// The `offer` merge of find(): one hit per `file:path`, the better score wins, `alsoVia` records the rest.
fn offer(out: &mut IndexMap<String, Map<String, Value>>, hit: Map<String, Value>) {
    let k = format!("{}:{}", hit.get("file").and_then(Value::as_str).unwrap_or(""), hit.get("path").and_then(Value::as_str).or_else(|| hit.get("name").and_then(Value::as_str)).unwrap_or(""));
    let Some(prior) = out.get(&k) else {
        out.insert(k, hit);
        return;
    };
    let hit_wins = hit_score(&hit) > hit_score(prior);
    let (strong, weak) = if hit_wins { (hit, prior.clone()) } else { (prior.clone(), hit) };
    let mut also: Vec<Value> = Vec::new();
    let mut push = |v: &Value| {
        if !also.contains(v) {
            also.push(v.clone());
        }
    };
    for src in [&strong, &weak] {
        if let Some(a) = src.get("alsoVia").and_then(Value::as_array) {
            for v in a {
                push(v);
            }
        }
    }
    if let Some(wv) = weak.get("via") {
        if strong.get("via") != Some(wv) {
            push(wv);
        }
    }
    let quote = if truthy(strong.get("quote")) {
        strong.get("quote").cloned().unwrap()
    } else if truthy(weak.get("quote")) {
        weak.get("quote").cloned().unwrap()
    } else {
        Value::Null
    };
    let mut merged = strong;
    if !also.is_empty() {
        merged.insert("alsoVia".into(), Value::Array(also));
    }
    merged.insert("quote".into(), quote);
    out.insert(k, merged);
}

fn sort_ranked(v: &mut [Map<String, Value>]) {
    use std::cmp::Ordering::Equal;
    v.sort_by(|a, b| {
        hit_score(b)
            .partial_cmp(&hit_score(a))
            .unwrap_or(Equal)
            .then(hit_rel(b).partial_cmp(&hit_rel(a)).unwrap_or(Equal))
            .then(hit_span(a).partial_cmp(&hit_span(b)).unwrap_or(Equal))
    });
}

impl Workspace {
    // ── private helpers (prefixed lk_ so they never collide with another file's) ──

    /// libraryPath(p) with the error branch: None = not library; Some(Ok(rel)) addressable;
    /// Some(Err(msg)) a library path the facade refuses. A deliberate local twin (library.rs's
    /// `library_path` cannot carry the error).
    fn lk_library_path(&self, p: &str) -> Option<std::result::Result<String, String>> {
        let mut s = p.replace('\\', "/");
        if let Some(root) = self.access.root() {
            let root = root.replace('\\', "/");
            let root = root.strip_suffix('/').unwrap_or(&root).to_string();
            let pre = format!("{}/", root.to_lowercase());
            if !root.is_empty() && s.to_lowercase().starts_with(&pre) {
                s = s.chars().skip(root.chars().count() + 1).collect();
            }
        }
        if s == "." {
            s.clear();
        } else if let Some(r) = s.strip_prefix("./") {
            s = r.to_string();
        }
        let raw = s.trim_matches('/').to_string();
        let check = |raw: &str| self.access.check_path(raw);
        if self.unindexed {
            if raw.is_empty() {
                return Some(Ok(String::new()));
            }
            return Some(check(&raw));
        }
        if raw.is_empty() {
            return None;
        }
        let segs: Vec<&str> = raw.split('/').collect();
        let skip_hit = |x: &str| {
            !x.starts_with('.')
                && match &self.opts.skip {
                    Some(v) => v.iter().any(|k| k == x),
                    None => okcode_analysis::access::DEFAULT_SKIP.iter().any(|k| *k == x),
                }
        };
        if !segs.iter().any(|x| skip_hit(x)) {
            return None;
        }
        if self.read_ix().files.contains_key(&raw) {
            return None;
        }
        if !segs.contains(&"..") && segs.iter().any(|x| x.starts_with('.') && *x != ".") {
            return None;
        }
        Some(check(&raw))
    }

    fn lk_rec(&self, rel: &str) -> Option<FileRecord> {
        self.read_ix().files.get(rel).cloned()
    }

    fn lk_not_indexed(&self, what: &str) -> Error {
        Error::code(
            "OKCODE_NOT_INDEXED",
            format!("workspace \"{}\" is not indexed — {what} needs an index; grep it, outline a file, or read by path, range or heading", self.id),
        )
    }

    fn lk_file_json(&self, rel: &str) -> Value {
        self.lk_rec(rel).map(|f| f.to_json(false)).unwrap_or(Value::Null)
    }

    /// windowAt(file, line, owner, want) → `{ lineStart, lineEnd, text }` or null
    pub fn window_at(&self, file: &FileRecord, line: i64, owner: Option<&Value>, want: Option<usize>) -> Option<Value> {
        let src = file.content.as_deref().unwrap_or("");
        if src.is_empty() {
            return None;
        }
        let want = want.unwrap_or(self.opts.window_lines) as i64;
        let lines: Vec<&str> = src.split('\n').collect();
        let half = want / 2;
        let mut lo = (line - half).max(1);
        let mut hi = (lines.len() as i64).min(line + half);
        if let Some(o) = owner {
            lo = lo.max(vnum(o, "lineStart").unwrap_or(lo));
            hi = hi.min(vnum(o, "lineEnd").unwrap_or(hi));
        }
        let max = self.opts.window_max_chars;
        let mut text = lines_join(&lines, lo, hi);
        // Trim from the END, never the middle: the located line is the last to go.
        while jlen(&text) > max && hi > line {
            hi -= 1;
            text = lines_join(&lines, lo, hi);
        }
        while jlen(&text) > max && lo < line {
            lo += 1;
            text = lines_join(&lines, lo, hi);
        }
        Some(json!({"lineStart": lo, "lineEnd": hi, "text": js_slice(&text, 0, max)}))
    }

    /// fileOf(hint): exact rel (then case-insensitive), then a unique `/`-suffix match → rel
    pub fn file_of(&self, hint: &str) -> Option<String> {
        if hint.is_empty() {
            return None;
        }
        let mut want = hint.replace('\\', "/");
        if let Some(r) = want.strip_prefix("./") {
            want = r.to_string();
        }
        if let Some(root) = self.access.root() {
            let root = root.replace('\\', "/");
            let root = root.strip_suffix('/').unwrap_or(&root);
            let pre = format!("{}/", root.to_lowercase());
            if !root.is_empty() && want.to_lowercase().starts_with(&pre) {
                let cut = root.chars().count() + 1;
                want = want.chars().skip(cut).collect();
            }
        }
        let ix = self.read_ix();
        if ix.files.contains_key(&want) {
            return Some(want);
        }
        let lw = want.to_lowercase();
        if let Some(k) = ix.files.keys().find(|k| k.to_lowercase() == lw) {
            return Some(k.clone());
        }
        let suffix = format!("/{want}");
        let mut tail = ix.files.keys().filter(|k| k.ends_with(&suffix));
        match (tail.next(), tail.next()) {
            (Some(k), None) => Some(k.clone()),
            _ => None,
        }
    }

    /// resolve(query) → `{ ok: true, file, symbol }` or a refusal value
    pub fn resolve(&self, query: &str) -> Result<Value> {
        let (file_hint, name_hint): (Option<&str>, &str) = if query.contains('#') {
            let mut it = query.split('#');
            (it.next(), it.next().unwrap_or(""))
        } else {
            (None, query)
        };
        let file_hint = file_hint.filter(|h| !h.is_empty());
        let pick = |pred: &dyn Fn(&Value) -> bool| -> Vec<(String, Value)> {
            let ix = self.read_ix();
            let mut out = Vec::new();
            for (rel, f) in ix.files.iter() {
                if let Some(h) = file_hint {
                    if !rel.contains(h) {
                        continue;
                    }
                }
                for s in &f.symbols {
                    if pred(s) {
                        out.push((rel.clone(), s.clone()));
                    }
                }
            }
            out
        };
        let mut hits = pick(&|s| sym_path(s) == name_hint);
        if hits.is_empty() {
            hits = pick(&|s| path_ends_with(s, name_hint));
        }
        if hits.is_empty() {
            hits = pick(&|s| vstr(s, "name") == Some(name_hint));
        }
        if hits.is_empty() {
            if file_hint.is_none() {
                if let Some(f) = self.file_of(name_hint) {
                    return Ok(json!({"ok": false, "isFile": true, "file": self.lk_file_json(&f), "reason": format!("\"{name_hint}\" is a file, not a symbol")}));
                }
            }
            // A path that does not exist must not be called a missing symbol.
            static EXT: OnceLock<Regex> = OnceLock::new();
            let ext = EXT.get_or_init(|| Regex::new(r"(?i)\.[a-z0-9]{1,6}$").unwrap());
            if file_hint.is_none() && (name_hint.contains('/') || name_hint.contains('\\')) && ext.is_match(name_hint) {
                let base = base_name(name_hint);
                let all: Vec<String> = self.read_ix().files.keys().cloned().collect();
                let near: Vec<String> = all.iter().filter(|r| rel_base(r) == base).take(4).cloned().collect();
                // Nearest by SHARED NAME TOKEN, not by substring.
                let tokens = |s: &str| -> Vec<String> {
                    s.split('.')
                        .next()
                        .unwrap_or("")
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .filter(|t| t.chars().count() >= 3)
                        .map(|t| t.to_lowercase())
                        .collect()
                };
                let want_tokens: HashSet<String> = tokens(&base).into_iter().collect();
                let kin: Vec<String> = if !near.is_empty() || want_tokens.is_empty() {
                    Vec::new()
                } else {
                    all.iter().filter(|r| tokens(rel_base(r)).iter().any(|t| want_tokens.contains(t))).take(4).cloned().collect()
                };
                let mut reason = format!("there is no file \"{name_hint}\" in this workspace");
                if !near.is_empty() {
                    reason += &format!(" — but {} {} that name", near.join(", "), if near.len() == 1 { "has" } else { "have" });
                }
                if !kin.is_empty() {
                    reason += &format!(" — the nearest existing files are {}", kin.join(", "));
                }
                if near.is_empty() && kin.is_empty() {
                    reason += " — nothing with a similar name exists either; use structure() to see what is actually here";
                }
                let cands = if near.is_empty() { kin } else { near };
                return Ok(json!({"ok": false, "noSuchFile": true, "reason": reason, "candidates": cands}));
            }
            let mentions = match self.mentions_of(name_hint)? {
                Value::Array(a) => a,
                _ => Vec::new(),
            };
            let hint = format!("code_grep(\"{name_hint}\") for the rest");
            let (n, more) = lk_more(mentions.len(), MENTIONS_REASON_CAP, Some(&hint));
            let shown: Vec<Value> = mentions.iter().take(n).cloned().collect();
            let total = mentions.len();
            let where_ = shown
                .iter()
                .map(|m| format!("{}:{}", vstr(m, "rel").unwrap_or(""), m.get("line").map(js_string).unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ");
            let reason = format!(
                "no symbol named \"{name_hint}\"{}{}",
                file_hint.map(|h| format!(" in {h}")).unwrap_or_default(),
                if total > 0 {
                    format!(" — but the text appears in {total} place{}: {where_}{more}; read the file, or use find() for what contains it", if total == 1 { "" } else { "s" })
                } else {
                    " — nothing in the workspace mentions it either; check the spelling or use ask()".to_string()
                }
            );
            return Ok(json!({"ok": false, "reason": reason, "mentions": shown, "mentionsTotal": total}));
        }
        if hits.len() > 1 {
            let distinct: HashSet<&String> = hits.iter().map(|(r, _)| r).collect();
            let where_ = if distinct.len() > 1 { "files" } else { "places" };
            let (n, more) = lk_more(hits.len(), MENTIONS_REASON_CAP, None);
            return Ok(json!({
                "ok": false,
                "ambiguous": true,
                "reason": format!("\"{name_hint}\" is defined in {} {where_}{more} — qualify it as <file>#<path>", hits.len()),
                "candidates": hits.iter().take(n).map(|(r, s)| format!("{r}#{}", sym_path(s))).collect::<Vec<_>>(),
            }));
        }
        let (rel, sym) = hits.remove(0);
        Ok(json!({"ok": true, "file": self.lk_file_json(&rel), "symbol": sym}))
    }

    /// textOfFresh(rel): the verified current text — the cache, else the facade (None = no such record).
    pub fn text_of_fresh(&self, rel: &str) -> Result<Option<Arc<str>>> {
        let cached = {
            let ix = self.read_ix();
            match ix.files.get(rel) {
                None => return Ok(None),
                Some(f) => f.content.clone(),
            }
        };
        if let Some(c) = cached {
            return Ok(Some(c));
        }
        self.ensure_texts(&[rel.to_string()])?;
        let ix = self.read_ix();
        Ok(Some(ix.files.get(rel).and_then(|f| f.content.clone()).unwrap_or_else(|| Arc::from(""))))
    }

    /// sliceSymbol(file, symbol) → `{ text, src, file, symbol }`: verified first, then served.
    pub fn slice_symbol(&self, rel: &str, symbol: &Value) -> Result<Value> {
        self.slice_symbol_fb(rel, symbol, None)
    }

    /// `fallback` is the record JSON resolve() returned (JS: `files.get(path) || fresh`).
    fn slice_symbol_fb(&self, rel: &str, symbol: &Value, fallback: Option<Value>) -> Result<Value> {
        self.verify(&[rel.to_string()], false)?;
        let src = self.text_of_fresh(rel)?.unwrap_or_else(|| Arc::from(""));
        let fresh = self.lk_rec(rel);
        // verify() may just have re-ingested this file with NEW offsets: slice what the fresh record says.
        let want = sym_path(symbol);
        let current = fresh.as_ref().and_then(|f| f.symbols.iter().find(|s| sym_path(s) == want).cloned()).unwrap_or_else(|| symbol.clone());
        let text = symbol_text(&src, &current);
        Ok(json!({"text": text, "src": &*src, "file": fresh.map(|f| f.to_json(false)).or(fallback).unwrap_or(Value::Null), "symbol": current}))
    }

    /// analysisOf(file): the dialect-specific envelope for an extension-owned file (cached on the record).
    pub fn analysis_of(&self, rel: &str) -> Result<Value> {
        let Some(file) = self.lk_rec(rel) else { return Ok(Value::Null) };
        if let Some(a) = file.extra.get("okAnalysis").filter(|a| truthy(Some(a))) {
            return Ok(a.clone());
        }
        if !okcode_analysis::okjs::matches(&file.rel) {
            return Ok(Value::Null);
        }
        self.verify(&[rel.to_string()], false)?;
        let Some(fresh) = self.lk_rec(rel) else { return Ok(Value::Null) };
        if let Some(a) = fresh.extra.get("okAnalysis").filter(|a| truthy(Some(a))) {
            return Ok(a.clone());
        }
        let src = self.text_of_fresh(rel)?.unwrap_or_else(|| Arc::from(""));
        // A checker that throws loses only the envelope, never the caller.
        let result = match okcode_analysis::okjs::analyze(&fresh.rel, &src) {
            Ok(v) => v,
            Err(_) => return Ok(Value::Null),
        };
        if let Some(a) = result.get("okAnalysis").filter(|a| truthy(Some(a))) {
            let mut ix = self.write_ix();
            if let Some(f) = ix.files.get_mut(rel) {
                f.extra.insert("okAnalysis".into(), a.clone());
                for k in ["diagnostics", "coverage", "regions"] {
                    match result.get(k) {
                        Some(v) => {
                            f.extra.insert(k.into(), v.clone());
                        }
                        None => {
                            f.extra.shift_remove(k);
                        }
                    }
                }
            }
            return Ok(a.clone());
        }
        Ok(Value::Null)
    }

    /// explainTarget(target): WHY a target did not resolve, in the caller's own terms.
    pub fn explain_target(&self, target: &str) -> Result<String> {
        let cut = [target.rfind(':'), target.rfind('#')].into_iter().flatten().max();
        if let Some(cut) = cut.filter(|&c| jlen(&target[..c]) > 1) {
            let head = &target[..cut];
            let tail = &target[cut + 1..];
            if let Some(f) = self.file_of(head) {
                return Ok(if range_tail(tail) {
                    format!("{f} IS indexed — \"{target}\" failed on the separator, not the file; address a line range as \"{f}:{tail}\"")
                } else {
                    format!("{f} IS indexed, but has no symbol \"{tail}\" — address a line range as \"{f}:<from>-<to>\", or read {f} to find the symbol's real name")
                });
            }
        }
        let r = self.resolve(target)?;
        if truthy(r.get("noSuchFile")) {
            if let Some(reason) = vstr_t(&r, "reason") {
                return Ok(reason.to_string());
            }
        }
        if let Some(reason) = vstr_t(&r, "reason") {
            return Ok(format!("no indexed file matching \"{target}\" — {reason}"));
        }
        Ok(format!("no indexed file matching \"{target}\""))
    }

    // ── api ─────────────────────────────────────────────────────────────

    pub fn structure(&self, o: StructureOpts) -> Result<Value> {
        if self.unindexed {
            return Err(self.lk_not_indexed("structure()"));
        }
        self.ensure_open()?;
        let prefix = o.dir.as_deref().filter(|d| !d.is_empty()).map(|d| d.replace('\\', "/"));
        let all: Vec<FileRecord> = {
            let ix = self.read_ix();
            ix.files.values().filter(|f| prefix.as_deref().map(|p| f.rel.starts_with(p)).unwrap_or(true)).cloned().collect()
        };
        let mut by_dir: IndexMap<String, (u64, u64, u64)> = IndexMap::new();
        for f in &all {
            let d = match f.rel.rfind('/') {
                Some(0) => "/".to_string(),
                Some(i) => f.rel[..i].to_string(),
                None => ".".to_string(),
            };
            let e = by_dir.entry(d).or_insert((0, 0, 0));
            e.0 += 1;
            e.1 += f.lines;
            e.2 += f.symbols.len() as u64;
        }
        let unparsed: Vec<Value> = all
            .iter()
            .filter(|f| !f.parsed && f.lang == "javascript")
            .map(|f| {
                let mut m = Map::new();
                m.insert("file".into(), json!(f.rel));
                m.insert("reason".into(), f.reason.as_ref().map(|r| json!(r)).unwrap_or(Value::Null));
                Value::Object(m)
            })
            .collect();
        // The biggest single symbols; top-level only (a nested one is contained by its parent).
        let mut largest: Vec<(Value, String, i64)> = Vec::new();
        for f in &all {
            for s in f.symbols.iter().filter(|s| !truthy(s.get("parent"))) {
                let lines = vnum(s, "lineEnd").unwrap_or(0) - vnum(s, "lineStart").unwrap_or(0) + 1;
                largest.push((s.get("name").cloned().unwrap_or(Value::Null), f.rel.clone(), lines));
            }
        }
        largest.sort_by(|a, b| b.2.cmp(&a.2));
        largest.truncate(12);
        // RESULT-SET CHECK, bounded to the dozen files actually named here.
        let named: Vec<String> = largest.iter().map(|l| l.1.clone()).collect();
        self.result_set_check(&named)?;
        let mut dirs: Vec<Value> =
            by_dir.iter().map(|(d, (files, lines, symbols))| json!({"dir": d, "files": files, "lines": lines, "symbols": symbols})).collect();
        dirs.sort_by(|a, b| vfloat(b, "symbols").partial_cmp(&vfloat(a, "symbols")).unwrap_or(std::cmp::Ordering::Equal));
        let (unsaved, skipped) = {
            let ix = self.read_ix();
            (
                ix.store_errors.iter().map(|(f, e)| json!({"file": f, "error": e})).collect::<Vec<_>>(),
                ix.scan_warnings.iter().map(|(f, r)| json!({"file": f, "reason": r})).collect::<Vec<_>>(),
            )
        };
        Ok(json!({
            "id": self.id,
            "files": all.len(),
            "lines": all.iter().map(|f| f.lines).sum::<u64>(),
            "symbols": all.iter().map(|f| f.symbols.len()).sum::<usize>(),
            "dirs": dirs,
            "unparsed": unparsed,
            "unsaved": unsaved,
            "skipped": skipped,
            "largest": largest.iter().map(|(n, f, l)| json!({"name": n, "file": f, "lines": l})).collect::<Vec<_>>(),
        }))
    }

    pub fn find(&self, query: &Value, o: FindOpts) -> Result<Value> {
        self.find_inner(query, &o, false)
    }

    /// The core ranking of find(): every hit, scored and merged, unsliced, plus whether a
    /// full-text eye hit its own fetch cap.
    fn lk_rank_all(&self, query: &str, kind: Option<&str>, fts_limit: usize) -> Result<(Vec<Map<String, Value>>, bool)> {
        let q = query.to_lowercase();
        let mut out: IndexMap<String, Map<String, Value>> = IndexMap::new();
        // Open BEFORE querying the store, so a full-text hit is checked against the graph as it is now.
        self.ensure_open()?;
        let store = self.store.as_ref();
        // Both full-text indexes, strictly, then both loosely — and only if strict found nothing ANYWHERE.
        let mut symbol_hits: Vec<Value> = match store {
            Some(st) => st.search(query, fts_limit, "and")?,
            None => Vec::new(),
        };
        let content_limit = fts_limit.max(6);
        let mut content_hits: Vec<Value> = match store {
            Some(st) => st.search_content(query, content_limit, "and")?,
            None => Vec::new(),
        };
        let mut loose = false;
        if let Some(st) = store {
            if query.split_whitespace().count() > 1 && symbol_hits.is_empty() && content_hits.is_empty() {
                loose = true;
                symbol_hits = st.search(query, fts_limit, "or")?;
                content_hits = st.search_content(query, content_limit, "or")?;
            }
        }
        let prose: i64 = if loose { 18 } else { 40 };
        let content: i64 = if loose { 16 } else { 35 };
        // The ladder: exact name 100, prefix 70, substring 50, prose via full-text 40, prose via substring 25, content 35.
        let ix = self.read_ix();
        for h in &symbol_hits {
            if let Some(k) = kind {
                if vstr(h, "kind") != Some(k) {
                    continue;
                }
            }
            let Some(f) = vstr(h, "file").and_then(|r| ix.files.get(r)) else { continue };
            let score = match name_score(&q, vstr(h, "name"), vstr(h, "path")) {
                0 => prose,
                s => s,
            };
            offer(
                &mut out,
                symbol_hit(f, h, vec![("score", json!(score)), ("via", json!(if loose { "fts~" } else { "fts" })), ("relevance", json!(vfloat(h, "relevance")))]),
            );
        }
        drop(ix);
        // Substring matching still runs and merges: FTS misses partial identifiers.
        {
            let ix = self.read_ix();
            for f in ix.files.values() {
                for s in &f.symbols {
                    if let Some(k) = kind {
                        if vstr(s, "kind") != Some(k) {
                            continue;
                        }
                    }
                    let mut score = name_score(&q, vstr(s, "name"), vstr(s, "path"));
                    if score == 0 && vstr(s, "doc").unwrap_or("").to_lowercase().contains(&q) {
                        score = 25;
                    }
                    if score == 0 {
                        continue;
                    }
                    offer(&mut out, symbol_hit(f, s, vec![("score", json!(score))]));
                }
            }
        }
        // The content eye: every matched line is attributed to the symbol containing it.
        if !content_hits.is_empty() {
            let rels: Vec<String> = content_hits.iter().filter_map(|h| vstr(h, "file").map(String::from)).collect();
            self.verify(&rels, false)?;
        }
        let ix = self.read_ix();
        for hit in &content_hits {
            let Some(f) = vstr(hit, "file").and_then(|r| ix.files.get(r)) else { continue };
            let Some(text) = f.content.as_deref() else { continue };
            let lines = quote_lines(text, query, 3);
            let fallback = [crate::store::Quote { line: 1, n: 0, text: String::new() }];
            let used: &[crate::store::Quote] = if lines.is_empty() { &fallback } else { &lines };
            // Lines group by the symbol containing them, best citation wins.
            let mut best: IndexMap<String, (Option<Value>, &crate::store::Quote)> = IndexMap::new();
            for ln in used {
                let owner = owner_of(f, ln.line as i64).cloned();
                let key = owner.as_ref().map(sym_path).unwrap_or_else(|| f.rel.clone());
                let replace = match best.get(&key) {
                    None => true,
                    Some((_, prior)) => ln.n > prior.n,
                };
                if replace {
                    best.insert(key, (owner, ln));
                }
            }
            for (owner, ln) in best.values() {
                let win = self.window_at(f, ln.line as i64, owner.as_ref(), None);
                let common = vec![
                    ("score", json!(content)),
                    ("via", json!(if loose { "content~" } else { "content" })),
                    ("relevance", hit.get("relevance").cloned().unwrap_or(Value::Null)),
                    ("quote", json!(format!("{}:{}  {}", f.rel, ln.line, ln.text))),
                    ("line", json!(ln.line)),
                    ("window", win.clone().unwrap_or(Value::Null)),
                ];
                match owner {
                    Some(ow) => offer(&mut out, symbol_hit(f, ow, common)),
                    None => {
                        let (lines_s, span) = match &win {
                            Some(w) => {
                                let (a, b) = (vnum(w, "lineStart").unwrap_or(0), vnum(w, "lineEnd").unwrap_or(0));
                                (format!("{a}-{b}"), b - a + 1)
                            }
                            None => (format!("{0}-{0}", ln.line), 1),
                        };
                        let mut m = Map::new();
                        m.insert("name".into(), json!(f.rel));
                        m.insert("kind".into(), json!(f.lang));
                        m.insert("file".into(), json!(f.rel));
                        m.insert("path".into(), json!(f.rel));
                        m.insert("parent".into(), Value::Null);
                        m.insert("lines".into(), json!(lines_s));
                        m.insert("span".into(), json!(span));
                        m.insert("signature".into(), Value::Null);
                        m.insert("doc".into(), Value::Null);
                        m.insert("exported".into(), json!(false));
                        m.insert("at".into(), f.hash.as_ref().map(|h| json!(h)).unwrap_or(Value::Null));
                        for (k, v) in common {
                            m.insert(k.to_string(), v);
                        }
                        offer(&mut out, m);
                    }
                }
            }
        }
        // Tier first, then full-text relevance inside the tier, then the smaller symbol.
        let mut ranked: Vec<Map<String, Value>> = out.into_values().collect();
        sort_ranked(&mut ranked);
        let capped = symbol_hits.len() >= fts_limit || content_hits.len() >= content_limit;
        Ok((ranked, capped))
    }

    fn find_inner(&self, query: &Value, o: &FindOpts, retried: bool) -> Result<Value> {
        let qs = js_string(query);
        let q = qs.to_lowercase();
        let kind = o.kind.as_deref();
        let limit = o.limit;
        let off = o.offset;
        let path_s: Option<String> = match &o.path {
            None | Some(Value::Null) => None,
            Some(v) => Some(js_string(v)).filter(|p| !p.is_empty()),
        };
        // Scoped to a PATH. A library path is looked up by name in that subtree's scan.
        if self.unindexed || path_s.is_some() {
            if let Some(lib) = self.lk_library_path(path_s.as_deref().unwrap_or("")) {
                let lib = lib.map_err(|e| Error::code("OKCODE_BAD_PATH", format!("{}: {e}", path_s.as_deref().unwrap_or(""))))?;
                let scan = self.scan_library(&lib)?;
                let mut hits: Vec<Map<String, Value>> = Vec::new();
                for f in scan.get("files").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
                    let frel = vstr(f, "rel").unwrap_or("");
                    for s in f.get("symbols").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]) {
                        if let Some(k) = kind {
                            if vstr(s, "kind") != Some(k) {
                                continue;
                            }
                        }
                        let mut score = name_score(&q, vstr(s, "name"), vstr(s, "path"));
                        if score == 0 && vstr(s, "doc").unwrap_or("").to_lowercase().contains(&q) {
                            score = 25;
                        }
                        if score == 0 {
                            continue;
                        }
                        let ls = vnum(s, "lineStart").unwrap_or(0);
                        let le = vnum(s, "lineEnd").unwrap_or(0);
                        let mut m = Map::new();
                        m.insert("name".into(), s.get("name").cloned().unwrap_or(Value::Null));
                        m.insert("kind".into(), s.get("kind").cloned().unwrap_or(Value::Null));
                        m.insert("file".into(), json!(frel));
                        m.insert("path".into(), json!(sym_path(s)));
                        m.insert("parent".into(), vstr_t(s, "parent").map(|p| json!(p)).unwrap_or(Value::Null));
                        m.insert("lines".into(), json!(format!("{ls}-{le}")));
                        m.insert("span".into(), json!(le - ls + 1));
                        m.insert("signature".into(), vstr_t(s, "signature").map(|p| json!(p)).unwrap_or(Value::Null));
                        let doc = js_slice(vstr(s, "doc").unwrap_or(""), 0, 160);
                        m.insert("doc".into(), if doc.is_empty() { Value::Null } else { json!(doc) });
                        m.insert("exported".into(), json!(truthy(s.get("exported"))));
                        m.insert("score".into(), json!(score));
                        m.insert("via".into(), json!("library"));
                        hits.push(m);
                    }
                }
                use std::cmp::Ordering::Equal;
                hits.sort_by(|a, b| hit_score(b).partial_cmp(&hit_score(a)).unwrap_or(Equal).then(hit_span(a).partial_cmp(&hit_span(b)).unwrap_or(Equal)));
                let total = hits.len();
                let res: Vec<Value> = hits.into_iter().skip(off).take(limit).map(Value::Object).collect();
                let truncated = off + res.len() < total;
                let next = if truncated { json!(off + res.len()) } else { Value::Null };
                let mut library = Map::new();
                library.insert("path".into(), json!(lib));
                library.insert("files".into(), json!(scan.get("files").and_then(Value::as_array).map(Vec::len).unwrap_or(0)));
                library.insert("truncated".into(), scan.get("truncated").cloned().unwrap_or(Value::Null));
                if truthy(scan.get("missing")) {
                    library.insert("missing".into(), json!(true));
                }
                return Ok(lk_arr(
                    res,
                    vec![("total", json!(total)), ("offset", json!(off)), ("truncated", json!(truncated)), ("next", next), ("library", Value::Object(library))],
                ));
            }
            if let Some(p) = &path_s {
                let in_path = okcode_analysis::grep::path_matcher(&[], std::slice::from_ref(p)).map_err(Error::msg)?;
                if let Some(in_path) = in_path {
                    // Rank the FULL set, THEN filter to the path and page.
                    let (ranked, capped) = self.lk_rank_all(&qs, kind, 20000)?;
                    let filtered: Vec<Map<String, Value>> =
                        ranked.into_iter().filter(|h| in_path.matches(h.get("file").and_then(Value::as_str).unwrap_or(""))).collect();
                    let res: Vec<Map<String, Value>> = filtered.iter().skip(off).take(limit).cloned().collect();
                    if !retried && !res.is_empty() {
                        let files: Vec<String> = res.iter().filter_map(|h| h.get("file").and_then(Value::as_str).map(String::from)).collect();
                        if truthy(self.result_set_check(&files)?.get("changed")) {
                            return self.find_inner(query, o, true);
                        }
                    }
                    let truncated = off + res.len() < filtered.len();
                    let next = if truncated { json!(off + res.len()) } else { Value::Null };
                    return Ok(lk_arr(
                        res.into_iter().map(Value::Object).collect(),
                        vec![
                            ("total", json!(filtered.len())),
                            ("offset", json!(off)),
                            ("capped", json!(capped)),
                            ("truncated", json!(truncated)),
                            ("next", next),
                        ],
                    ));
                }
            }
        }
        let (ranked, _) = self.lk_rank_all(&qs, kind, limit)?;
        let results: Vec<Map<String, Value>> = ranked.iter().skip(off).take(limit).cloned().collect();
        // RESULT-SET CHECK: catches a file a symbol hit still points at but that has since changed.
        if !retried && !results.is_empty() {
            let files: Vec<String> = results.iter().filter_map(|h| h.get("file").and_then(Value::as_str).map(String::from)).collect();
            if truthy(self.result_set_check(&files)?.get("changed")) {
                let mut again = o.clone();
                again.path = None;
                return self.find_inner(query, &again, true);
            }
        }
        let truncated = off + results.len() < ranked.len();
        let next = if truncated { json!(off + results.len()) } else { Value::Null };
        Ok(lk_arr(
            results.into_iter().map(Value::Object).collect(),
            vec![("total", json!(ranked.len())), ("offset", json!(off)), ("truncated", json!(truncated)), ("next", next)],
        ))
    }

    pub fn files(&self, o: FilesOpts) -> Result<Value> {
        // A directory, not a string prefix: `lib` lists lib/, not libs/.
        let d = o.dir.clone().unwrap_or_default().replace('\\', "/");
        let d = d.strip_prefix("./").map(String::from).or_else(|| if d == "." { Some(String::new()) } else { None }).unwrap_or(d);
        let d = d.trim_matches('/').to_string();
        let prefix: Option<String> = if d.is_empty() { None } else { Some(format!("{d}/")) };
        let off = o.offset;
        let limit = o.limit;
        let lang = o.lang.as_deref().filter(|l| !l.is_empty());
        // Unindexed: a fresh listing (one facade call) — no rows, so no line or symbol counts.
        if self.unindexed {
            let rel = self
                .lk_library_path(&d)
                .unwrap_or_else(|| Ok(d.clone()))
                .map_err(|e| Error::code("OKCODE_BAD_PATH", format!("{d}: {e}")))?;
            let listed = self.list_library(&[rel], self.opts.library_files.max(1))?;
            let rows_in: Vec<Value> = listed.get("rows").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut all: Vec<Value> = rows_in.into_iter().filter(|r| lang.map(|l| okcode_analysis::lang_of(vstr(r, "rel").unwrap_or("")) == l).unwrap_or(true)).collect();
            let total = all.len();
            if limit > 0 || off > 0 {
                let end = if limit > 0 { off + limit } else { all.len() };
                all = all.into_iter().skip(off).take(end.saturating_sub(off)).collect();
            }
            let rows: Vec<Value> = all
                .iter()
                .map(|r| {
                    let rel = vstr(r, "rel").unwrap_or("");
                    json!({"file": rel, "size": r.get("size").cloned().filter(|v| !v.is_null()).unwrap_or(Value::Null), "lines": Value::Null,
                           "lang": okcode_analysis::lang_of(rel), "symbols": Value::Null, "indexed": false, "hash": Value::Null})
                })
                .collect();
            let truncated = off + rows.len() < total || truthy(listed.get("truncated"));
            return Ok(json!({
                "id": self.id,
                "dir": prefix,
                "total": total,
                "offset": off,
                "truncated": truncated,
                "next": if truncated { json!(off + rows.len()) } else { Value::Null },
                "asOf": now_ms(),
                "files": rows,
            }));
        }
        self.ensure_open()?;
        let (mut all, as_of): (Vec<FileRecord>, f64) = {
            let ix = self.read_ix();
            (
                ix.files
                    .values()
                    .filter(|f| prefix.as_deref().map(|p| f.rel.starts_with(p)).unwrap_or(true))
                    .filter(|f| lang.map(|l| f.lang == l).unwrap_or(true))
                    .cloned()
                    .collect(),
                ix.last_scan_at,
            )
        };
        all.sort_by(|a, b| a.rel.cmp(&b.rel));
        let total = all.len();
        if limit > 0 || off > 0 {
            let end = if limit > 0 { off + limit } else { all.len() };
            all = all.into_iter().skip(off).take(end.saturating_sub(off)).collect();
        }
        let mut rows = Value::Array(
            all.iter()
                .map(|f| {
                    json!({"file": f.rel, "size": f.size, "lines": f.lines, "lang": if f.lang.is_empty() { Value::Null } else { json!(f.lang) }, "symbols": f.symbols.len(),
                           "indexed": f.indexed, "hash": f.hash})
                })
                .collect(),
        );
        if o.stat {
            rows = self.flag_moved(rows)?;
        }
        let n = rows.as_array().map(Vec::len).unwrap_or(0);
        let truncated = off + n < total;
        Ok(json!({
            "id": self.id,
            "dir": prefix,
            "total": total,
            "offset": off,
            "truncated": truncated,
            "next": if truncated { json!(off + n) } else { Value::Null },
            "asOf": if as_of > 0.0 { json!(as_of) } else { Value::Null },
            "files": rows,
        }))
    }

    pub fn symbols(&self, file: &str, o: SymbolsOpts) -> Result<Value> {
        if self.unindexed {
            return Ok(json!({"ok": false, "code": "OKCODE_NOT_INDEXED", "reason": self.lk_not_indexed("symbols()").message}));
        }
        self.ensure_open()?;
        let Some(rel) = self.file_of(file) else {
            // Not explainTarget(): its miss path greps the workspace text.
            let base = base_name(file);
            let near: Vec<String> = {
                let ix = self.read_ix();
                ix.files.keys().filter(|r| !base.is_empty() && rel_base(r) == base).take(4).cloned().collect()
            };
            let mut m = Map::new();
            m.insert("ok".into(), json!(false));
            m.insert(
                "reason".into(),
                json!(format!("no indexed file matching \"{file}\"{}", if near.is_empty() { String::new() } else { format!(" — did you mean {}?", near.join(", ")) })),
            );
            if !near.is_empty() {
                m.insert("candidates".into(), json!(near));
            }
            return Ok(Value::Object(m));
        };
        let Some(f) = self.lk_rec(&rel) else {
            return Ok(json!({"ok": false, "reason": format!("no indexed file matching \"{file}\"")}));
        };
        let mut syms: Vec<&Value> = f.symbols.iter().filter(|s| o.kind.as_deref().map(|k| vstr(s, "kind") == Some(k)).unwrap_or(true)).collect();
        let total = syms.len();
        let off = o.offset;
        if o.limit > 0 || off > 0 {
            let end = if o.limit > 0 { off + o.limit } else { syms.len() };
            syms = syms.into_iter().skip(off).take(end.saturating_sub(off)).collect();
        }
        let truncated = off + syms.len() < total;
        let as_of = self.read_ix().last_scan_at;
        let page: Vec<Value> = syms.iter().map(|s| lk_symbol_row(s, f.rel.as_str(), false)).collect();
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("file".into(), json!(f.rel));
        out.insert("size".into(), json!(f.size));
        out.insert("lines".into(), json!(f.lines));
        out.insert("lang".into(), if f.lang.is_empty() { Value::Null } else { json!(f.lang) });
        out.insert("indexed".into(), json!(f.indexed));
        out.insert("hash".into(), json!(f.hash));
        out.insert("asOf".into(), if as_of > 0.0 { json!(as_of) } else { Value::Null });
        out.insert("total".into(), json!(total));
        out.insert("offset".into(), json!(off));
        out.insert("truncated".into(), json!(truncated));
        out.insert("next".into(), if truncated { json!(off + page.len()) } else { Value::Null });
        out.insert("symbols".into(), Value::Array(page));
        if o.stat {
            let rows = self.flag_moved(json!([{"file": f.rel, "size": f.size}]))?;
            if let Some(row) = rows.get(0) {
                for k in ["moved", "gone", "unverified"] {
                    if truthy(row.get(k)) {
                        out.insert(k.into(), json!(true));
                    }
                }
            }
        }
        Ok(Value::Object(out))
    }

    pub fn outline(&self, file: &str, o: OutlineOpts) -> Result<Value> {
        let off = o.offset;
        let limit = o.limit;
        if let Some(lib) = self.lk_library_path(file) {
            // JS hands the {rel}|{error} object to outlineLibrary; an error object has no rel.
            let lib = match lib {
                Ok(l) => l,
                Err(e) => return Ok(json!({"ok": false, "reason": e})),
            };
            return self.outline_library(&lib, limit, off);
        }
        self.ensure_open()?;
        let Some(rel) = self.file_of(file) else {
            return Ok(json!({"ok": false, "reason": self.explain_target(file)?}));
        };
        let Some(f) = self.lk_rec(&rel) else { return Ok(json!({"ok": false, "reason": self.explain_target(file)?})) };
        if !f.indexed {
            return Ok(json!({"ok": false, "reason": format!("{} is not indexed as text ({})", f.rel, f.reason.as_deref().filter(|r| !r.is_empty()).unwrap_or("binary or withheld"))}));
        }
        let is_ext = okcode_analysis::okjs::matches(&f.rel);
        // A plain file with symbols is answered from its rows (stat-first); the rest needs text.
        if !is_ext && !f.symbols.is_empty() {
            self.result_set_check(&[rel.clone()])?;
        } else {
            self.verify(&[rel.clone()], false)?;
        }
        let gone = |rel: &str| json!({"ok": false, "reason": format!("{rel} no longer exists")});
        let Some(mut fresh) = self.lk_rec(&rel) else { return Ok(gone(&f.rel)) };
        // Moved and lost every symbol: the rich paths below need text.
        if fresh.symbols.is_empty() && fresh.content.is_none() {
            self.verify(&[rel.clone()], false)?;
            match self.lk_rec(&rel) {
                Some(r) => fresh = r,
                None => return Ok(gone(&f.rel)),
            }
        }
        let max = self.opts.outline_max_lines;
        let pieces_out = |by: &str, flat: &[Value], map_kind: &dyn Fn(&Value) -> Value, fresh: &FileRecord| -> Value {
            let page: Vec<&Value> = flat.iter().skip(off).take(limit).collect();
            let truncated = off + page.len() < flat.len();
            let symbols: Vec<Value> = page
                .iter()
                .map(|p| {
                    let (a, b) = (vnum(p, "lineStart").unwrap_or(0), vnum(p, "lineEnd").unwrap_or(0));
                    json!({
                        "name": p.get("name").cloned().unwrap_or(Value::Null),
                        "path": format!("{}:{a}-{b}", fresh.rel),
                        "kind": map_kind(p),
                        "parent": Value::Null,
                        "lineStart": a,
                        "lineEnd": b,
                        "span": (b - a + 1).max(1),
                        "signature": Value::Null,
                        "doc": p.get("doc").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            json!({
                "ok": true,
                "file": fresh.rel,
                "lines": fresh.lines,
                "at": fresh.hash,
                "by": by,
                "total": flat.len(),
                "offset": off,
                "truncated": truncated,
                "next": if truncated { json!(off + page.len()) } else { Value::Null },
                "symbols": symbols,
            })
        };
        // An EXTENSION's structural regions are checked BEFORE the symbol count.
        if is_ext {
            self.analysis_of(&rel)?;
        }
        let regions: Vec<Value> = self
            .lk_rec(&rel)
            .unwrap_or_else(|| fresh.clone())
            .extra
            .get("regions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !regions.is_empty() {
            let text = self.text_of_fresh(&rel)?.unwrap_or_else(|| Arc::from(""));
            let all: Vec<&str> = text.split('\n').collect();
            let mut cut: Vec<Value> = Vec::new();
            for p in carve_regions(&regions) {
                let (ls, le) = (vnum(&p, "lineStart").unwrap_or(1), vnum(&p, "lineEnd").unwrap_or(1));
                let body = lines_join(&all, ls, le);
                for (a, b) in split_piece(ls.max(0) as usize, le.max(0) as usize, &body, max) {
                    let mut c = p.clone();
                    if let Some(m) = c.as_object_mut() {
                        m.insert("lineStart".into(), json!(a));
                        m.insert("lineEnd".into(), json!(b));
                    }
                    cut.push(c);
                }
            }
            let labelled = label_pieces(cut, &all);
            return Ok(pieces_out("region", &labelled, &|p| p.get("kind").cloned().unwrap_or(Value::Null), &fresh));
        }
        // Not every file has symbols: fall back to CHUNKS cut at real seams.
        if fresh.symbols.is_empty() {
            let src = self.text_of_fresh(&rel)?.unwrap_or_else(|| Arc::from(""));
            // A document (markdown, prose, JSON, a log): its titled sections.
            let sections = self.sections_outline(&fresh.rel, &src, limit, off);
            if let Some(sec) = sections.as_object() {
                let mut m = Map::new();
                m.insert("ok".into(), json!(true));
                m.insert("file".into(), json!(fresh.rel));
                m.insert("lines".into(), json!(fresh.lines));
                m.insert("at".into(), json!(fresh.hash));
                for (k, v) in sec {
                    m.insert(k.clone(), v.clone());
                }
                return Ok(Value::Object(m));
            }
            let u: Vec<u16> = src.encode_utf16().collect();
            let mut nl = Vec::with_capacity(u.len() + 1);
            let mut n = 0usize;
            nl.push(0usize);
            for &c in &u {
                if c == 10 {
                    n += 1;
                }
                nl.push(n);
            }
            let before = |k: usize| nl[k.min(u.len())] + 1;
            let all: Vec<&str> = src.split('\n').collect();
            let mut cut: Vec<Value> = Vec::new();
            for c in okcode_analysis::chunk::chunk::chunk_code(&src, Some(&fresh.rel)) {
                let line_start = before(c.start);
                let ends_nl = c.end >= 1 && u.get(c.end - 1) == Some(&10);
                let line_end = line_start.max(before(c.end) - usize::from(ends_nl));
                for (a, b) in split_piece(line_start, line_end, &c.text, max) {
                    cut.push(json!({"name": "chunk", "kind": "chunk", "lineStart": a, "lineEnd": b, "doc": Value::Null}));
                }
            }
            let labelled = label_pieces(cut, &all);
            return Ok(pieces_out("chunk", &labelled, &|_| json!("chunk"), &fresh));
        }
        let page: Vec<&Value> = fresh.symbols.iter().skip(off).take(limit).collect();
        let truncated = off + page.len() < fresh.symbols.len();
        Ok(json!({
            "ok": true,
            "file": fresh.rel,
            "lines": fresh.lines,
            "at": fresh.hash,
            "by": "symbol",
            "total": fresh.symbols.len(),
            "offset": off,
            "truncated": truncated,
            "next": if truncated { json!(off + page.len()) } else { Value::Null },
            "symbols": page.iter().map(|s| lk_symbol_row(s, &fresh.rel, true)).collect::<Vec<_>>(),
        }))
    }

    pub fn read(&self, query: &Value, o: ReadOpts) -> Result<Value> {
        let mut query = js_string(query);
        let mut from = o.from.filter(|&f| f != 0);
        let mut to = o.to.filter(|&t| t != 0);
        // A trailing :N-M (or #N-M) is a range, but a bare `store.js#name` is not.
        if let Some(range) = parse_range(&query) {
            from = from.or(Some(range.from)).filter(|&f| f != 0);
            to = to.or(Some(range.to)).filter(|&t| t != 0);
            query = range.file;
        }
        let cut = query.find('#').filter(|&c| c > 0);
        let head: String = match cut {
            Some(c) => query[..c].to_string(),
            None => query.clone(),
        };
        // Only a PATH (with a `/`) is library code; unindexed, every name is a path.
        let lib = if self.unindexed || head.contains('/') || head.contains('\\') { self.lk_library_path(&head) } else { None };
        if let Some(lib) = lib {
            let lib = match lib {
                Ok(l) => l,
                Err(e) => return Ok(json!({"ok": false, "reason": e})),
            };
            return self.read_library(
                &lib,
                ReadLibraryOpts { from, to, symbol: cut.map(|c| query[c + 1..].to_string()), max_lines: None },
            );
        }
        self.ensure_open()?;
        let r = self.resolve(&query)?;
        let ok = truthy(r.get("ok"));
        let ambiguous = truthy(r.get("ambiguous"));
        let is_file = truthy(r.get("isFile"));
        // `file#Title`: not a symbol, but perhaps a heading of that file.
        if !ok && !ambiguous && cut.is_some() && from.is_none() && to.is_none() {
            if let Some(rel) = self.file_of(&head) {
                if self.lk_rec(&rel).map(|f| f.indexed).unwrap_or(false) {
                    self.verify(&[rel.clone()], false)?;
                    if let Some(cur) = self.lk_rec(&rel) {
                        if let Some(src) = self.text_of_fresh(&rel)? {
                            let hash = self.lk_rec(&rel).unwrap_or(cur.clone()).hash;
                            let title = query[cut.unwrap() + 1..].to_string();
                            let sec = self.read_section(&cur.rel, &src, hash.as_deref(), &title, json!({}));
                            if !sec.is_null() {
                                return Ok(sec);
                            }
                        }
                    }
                }
            }
        }
        let jv = |x: Option<i64>| x.map(|n| json!(n)).unwrap_or(Value::Null);
        // Not a symbol, but a file the index knows — serve it rather than sending the caller to the shell.
        if !ok && !is_file && (from.is_some() || to.is_some()) {
            if let Some(guess) = self.file_of(&query) {
                return self.read_at(&guess, jv(from), jv(to.or(from)), None);
            }
        }
        if !ok && is_file {
            let rel = r.get("file").and_then(|f| vstr(f, "rel")).unwrap_or("").to_string();
            return if from.is_some() || to.is_some() {
                self.read_at(&rel, jv(from.or(Some(1))), jv(to.or(from)), None)
            } else {
                self.read_file(&rel, ReadFileOpts::default())
            };
        }
        if !ok {
            return Ok(r);
        }
        let rel = r.get("file").and_then(|f| vstr(f, "rel")).unwrap_or("").to_string();
        let sym = r.get("symbol").cloned().unwrap_or(Value::Null);
        if from.is_some() || to.is_some() {
            return self.read_at(&rel, jv(from.or(vnum(&sym, "lineStart"))), jv(to.or(vnum(&sym, "lineEnd"))), None);
        }
        let sliced = self.slice_symbol_fb(&rel, &sym, r.get("file").cloned())?;
        let symbol = sliced.get("symbol").cloned().unwrap_or(Value::Null);
        let file = sliced.get("file").cloned().unwrap_or(Value::Null);
        Ok(json!({
            "ok": true,
            "name": symbol.get("name").cloned().unwrap_or(Value::Null),
            "kind": symbol.get("kind").cloned().unwrap_or(Value::Null),
            "file": file.get("rel").cloned().unwrap_or(Value::Null),
            "path": sym_path(&symbol),
            "parent": vstr_t(&symbol, "parent").map(|p| json!(p)).unwrap_or(Value::Null),
            "lineStart": symbol.get("lineStart").cloned().unwrap_or(Value::Null),
            "lineEnd": symbol.get("lineEnd").cloned().unwrap_or(Value::Null),
            "at": file.get("hash").cloned().unwrap_or(Value::Null),
            "body": sliced.get("text").cloned().unwrap_or(Value::Null),
        }))
    }

    pub fn read_file(&self, file: &str, o: ReadFileOpts) -> Result<Value> {
        let max_lines = o.max_lines.unwrap_or(self.opts.file_read_lines);
        if let Some(lib) = self.lk_library_path(file) {
            let lib = match lib {
                Ok(l) => l,
                Err(e) => return Ok(json!({"ok": false, "reason": e})),
            };
            return self.read_library(&lib, ReadLibraryOpts { max_lines: Some(max_lines), ..Default::default() });
        }
        self.ensure_open()?;
        let Some(rel) = self.file_of(file) else { return Ok(json!({"ok": false, "reason": self.explain_target(file)?})) };
        let Some(f) = self.lk_rec(&rel) else { return Ok(json!({"ok": false, "reason": self.explain_target(file)?})) };
        if !f.indexed {
            return Ok(json!({"ok": false, "reason": format!("{} is not indexed as text ({})", f.rel, f.reason.as_deref().filter(|r| !r.is_empty()).unwrap_or("binary or withheld"))}));
        }
        self.verify(&[rel.clone()], false)?;
        if self.lk_rec(&rel).is_none() {
            return Ok(json!({"ok": false, "reason": format!("{} no longer exists", f.rel)}));
        }
        let src = self.text_of_fresh(&rel)?.unwrap_or_else(|| Arc::from(""));
        let cur = self.lk_rec(&rel).unwrap_or(f);
        let lines: Vec<&str> = src.split('\n').collect();
        let shown = lines.len().min(max_lines);
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        m.insert("file".into(), json!(cur.rel));
        m.insert("lineStart".into(), json!(1));
        m.insert("lineEnd".into(), json!(shown));
        m.insert("lines".into(), json!(lines.len()));
        m.insert("at".into(), json!(cur.hash));
        m.insert("body".into(), json!(lines[..shown].join("\n")));
        if shown < lines.len() {
            m.insert("truncated".into(), json!(true));
            m.insert("more".into(), json!(format!("{}:{}-{}", cur.rel, shown + 1, lines.len())));
            m.insert(
                "note".into(),
                json!(format!(
                    "showing lines 1-{shown} of {}; read \"{}:{}-{}\" for the next stretch",
                    lines.len(),
                    cur.rel,
                    shown + 1,
                    lines.len().min(shown + max_lines)
                )),
            );
        }
        Ok(Value::Object(m))
    }

    pub fn read_at(&self, file: &str, line_start: Value, line_end: Value, at: Option<String>) -> Result<Value> {
        let at = at.filter(|a| !a.is_empty());
        let ls = num_arg(&line_start);
        let le = num_arg(&line_end);
        if let Some(lib) = self.lk_library_path(file) {
            let lib = match lib {
                Ok(l) => l,
                Err(e) => return Ok(json!({"ok": false, "reason": e})),
            };
            let got = self.read_library(&lib, ReadLibraryOpts { from: Some(ls.unwrap_or(1)), to: Some(le.or(ls).unwrap_or(1)), ..Default::default() })?;
            if truthy(got.get("ok")) {
                if let Some(a) = &at {
                    if got.get("at").and_then(Value::as_str) != Some(a.as_str()) {
                        return Ok(json!({"ok": false, "stale": true, "reason": "the file changed since that range was resolved", "at": got.get("at").cloned().unwrap_or(Value::Null)}));
                    }
                }
            }
            return Ok(got);
        }
        self.ensure_open()?;
        let Some(rel) = self.file_of(file) else { return Ok(json!({"ok": false, "reason": self.explain_target(file)?})) };
        // Verify BEFORE judging the caller's `at`.
        self.verify(&[rel.clone()], false)?;
        let Some(fresh) = self.lk_rec(&rel) else { return Ok(json!({"ok": false, "reason": format!("{rel} no longer exists")})) };
        let src = self.text_of_fresh(&rel)?.unwrap_or_else(|| Arc::from(""));
        let cur = self.lk_rec(&rel).unwrap_or(fresh);
        if let Some(a) = &at {
            if cur.hash.as_deref() != Some(a.as_str()) {
                return Ok(json!({"ok": false, "stale": true, "reason": "the file changed since that range was resolved", "at": cur.hash}));
            }
        }
        let lines: Vec<&str> = src.split('\n').collect();
        let total = lines.len() as i64;
        let start = ls.unwrap_or(1).max(1);
        let end = total.min(start.max(le.unwrap_or(start)));
        Ok(json!({
            "ok": true,
            "file": cur.rel,
            "lineStart": start,
            "lineEnd": end,
            "lines": total,
            "at": cur.hash,
            "body": lines_join(&lines, start, end),
        }))
    }
}

/// One symbol-table row (symbols() and outline()).
fn lk_symbol_row(s: &Value, _rel: &str, outline: bool) -> Value {
    let (a, b) = (vnum(s, "lineStart").unwrap_or(0), vnum(s, "lineEnd").unwrap_or(0));
    let doc = match vstr_t(s, "doc") {
        Some(d) => json!(js_slice(d, 0, 200)),
        None => Value::Null,
    };
    let mut row = json!({
        "name": s.get("name").cloned().unwrap_or(Value::Null),
        "path": sym_path(s),
        "kind": s.get("kind").cloned().unwrap_or(Value::Null),
        "parent": vstr_t(s, "parent").map(|p| json!(p)).unwrap_or(Value::Null),
        "lineStart": a,
        "lineEnd": b,
        "span": b - a + 1,
        "signature": vstr_t(s, "signature").map(|p| json!(p)).unwrap_or(Value::Null),
        "doc": doc,
    });
    if !outline {
        // symbols() rows carry `exported` (before `doc`); outline() rows do not.
        let mut m = Map::new();
        for (k, v) in row.as_object().cloned().unwrap_or_default() {
            if k == "doc" {
                m.insert("exported".into(), json!(truthy(s.get("exported"))));
            }
            m.insert(k, v);
        }
        row = Value::Object(m);
    }
    row
}
