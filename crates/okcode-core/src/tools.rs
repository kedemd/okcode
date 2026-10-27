//! Model-facing tools (lib/tools.js).
//!
//! `createTools({ workspace, workspaces, grepMaxChars })` → [`Tools::new`] over a [`ToolHost`]
//! (host.rs: the host's two callbacks; the binding bridges JS ones). `tools.render(name, args)`
//! → [`Tools::render`]: the text a model sees, NEVER an error (a failure is a sentence).
//! `tools.schemas()` → [`Tools::schemas`] / [`schemas_for`] (the JS layer passes `workspaces()`'s
//! list in, so the sync JS call never round-trips to the JS thread it is running on).
//!
//! Every workspace verb goes through `ToolWorkspace::call(method, args)` with the JS argument
//! shapes (`ws.grep(q, { … })`, `ws.read(target, { … })`, …), so a native ws, an okcode ws and a
//! bridged JS ws render the same text. Offsets in hitWithin/uniqueAnchors are JS string indices
//! (UTF-16 code units).
use crate::error::{Error, Result};
use crate::host::{ToolHost, ToolWorkspace};
use okcode_analysis::grep as g;
use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};

pub const LOOKUPS: &[&str] = &["code_map", "code_find", "code_grep", "code_glob", "code_outline", "code_read", "code_package", "code_ask"];
pub const EDITS: &[&str] = &["code_edit", "code_write"];
pub const TOOLS: &[&str] = &[
    "code_map", "code_find", "code_grep", "code_glob", "code_outline", "code_read", "code_package", "code_ask", "code_edit", "code_write",
];
/// A diff beyond this is summarised, not dumped.
pub const DIFF_CAP: usize = 4000;
/// The CURRENT region re-read after a refused edit.
pub const REGION_CAP: usize = 3000;
pub const ANCHOR_CAP: usize = 1500;

pub fn is_tool(name: &str) -> bool {
    TOOLS.contains(&name)
}
pub fn is_lookup(name: &str) -> bool {
    LOOKUPS.contains(&name)
}
pub fn is_edit(name: &str) -> bool {
    EDITS.contains(&name)
}

// ── JS value semantics over serde_json ───────────────────────────────────────────────────────

static NULL: Value = Value::Null;

/// `v[k]` with a missing key as null.
fn get<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}

/// JS truthiness.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn numstr(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    if f.is_nan() {
        "NaN".into()
    } else if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{f:.0}")
    } else {
        format!("{f}")
    }
}

/// JS `String(v)`.
fn js_str(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => numstr(n),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(|x| if x.is_null() { String::new() } else { js_str(x) }).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// Array.join element: null renders as ''.
fn js_item(v: &Value) -> String {
    if v.is_null() { String::new() } else { js_str(v) }
}

/// `${v[k]}` — a missing key reads `undefined`.
fn sg(v: &Value, k: &str) -> String {
    match v.get(k) {
        None => "undefined".into(),
        Some(x) => js_str(x),
    }
}

/// `String(v[k] || fallback)`
fn or_str(v: &Value, k: &str, fallback: &str) -> String {
    let x = get(v, k);
    if truthy(x) {
        js_str(x)
    } else {
        fallback.to_string()
    }
}

/// JS `Number(v)`.
fn to_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => *b as i64 as f64,
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => str_to_number(s),
        Value::Array(a) => match a.len() {
            0 => 0.0,
            1 => to_number(&a[0]),
            _ => f64::NAN,
        },
        Value::Object(_) => f64::NAN,
    }
}

/// Number('0x…'/'0b…'/'0o…'): unsigned digits only, any length (a float beyond 2^53, as JS).
fn radix(d: &str, r: u32) -> f64 {
    if d.is_empty() || !d.chars().all(|c| c.is_digit(r)) {
        return f64::NAN;
    }
    d.chars().fold(0.0, |n, c| n * r as f64 + c.to_digit(r).unwrap() as f64)
}

fn str_to_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return radix(h, 16);
    }
    if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        return radix(b, 2);
    }
    if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return radix(o, 8);
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if t.chars().any(|c| c.is_alphabetic() && c != 'e' && c != 'E') {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

fn jnum(f: f64) -> Value {
    if !f.is_finite() {
        return Value::Null;
    }
    if f.fract() == 0.0 && f.abs() < 9e15 {
        return json!(f as i64);
    }
    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
}

/// tools.js `num`: None for null/''/non-finite.
fn num(v: &Value) -> Option<f64> {
    if v.is_null() || v.as_str() == Some("") {
        return None;
    }
    let n = to_number(v);
    if n.is_finite() {
        Some(n)
    } else {
        None
    }
}

/// `Number(v) || d`
fn num_or(v: &Value, d: f64) -> f64 {
    let n = to_number(v);
    if n == 0.0 || n.is_nan() {
        d
    } else {
        n
    }
}

/// `num(v) || 0`
fn num_or_zero(v: &Value) -> f64 {
    num(v).filter(|n| *n != 0.0).unwrap_or(0.0)
}

fn ni(v: &Value) -> i64 {
    match v {
        Value::Number(n) => n.as_i64().unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        Value::Null => 0,
        other => {
            let f = to_number(other);
            if f.is_nan() {
                0
            } else {
                f as i64
            }
        }
    }
}

fn len16(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)` by UTF-16 units (never splits a surrogate pair).
fn slice16(s: &str, n: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        let l = c.len_utf16();
        if used + l > n {
            break;
        }
        used += l;
        out.push(c);
    }
    out
}

fn pad_start(s: String, w: usize) -> String {
    let l = len16(&s);
    if l >= w {
        s
    } else {
        format!("{}{}", " ".repeat(w - l), s)
    }
}

/// An array result: a plain array, or the `{ $arr, $props }` form of an array that carries
/// properties (paged results). `props` is null for a plain one.
struct Arr<'a> {
    items: &'a [Value],
    props: &'a Value,
}

fn arr(v: &Value) -> Option<Arr<'_>> {
    match v {
        Value::Array(a) => Some(Arr { items: a, props: &NULL }),
        Value::Object(o) if o.len() <= 2 && o.contains_key("$arr") => match o.get("$arr") {
            Some(Value::Array(a)) => Some(Arr { items: a, props: o.get("$props").unwrap_or(&NULL) }),
            _ => None,
        },
        _ => None,
    }
}

fn items(v: &Value) -> &[Value] {
    arr(v).map(|a| a.items).unwrap_or(&[])
}

/// A JS array property with its elements (`x.files`), tolerating the marker form.
fn list<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    items(get(v, k))
}

fn code_of(e: &Error) -> &str {
    e.code.as_deref().unwrap_or("")
}

// ── pure exports ─────────────────────────────────────────────────────────────────────────────

/// subjectOf(name, args) — what a call was ABOUT
pub fn subject_of(name: &str, args: &Value) -> String {
    let _ = name;
    if !truthy(args) {
        return String::new();
    }
    for k in ["symbol", "target", "path", "file", "query", "text", "pattern", "glob", "question", "name", "workspace"] {
        let x = get(args, k);
        if truthy(x) {
            return js_str(x);
        }
    }
    String::new()
}

type U = Vec<u16>;

fn u(s: &str) -> U {
    s.encode_utf16().collect()
}
fn un(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

/// `hay.indexOf(needle, from)`
fn index_of(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return if from <= hay.len() { Some(from) } else { None };
    }
    if needle.len() > hay.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let mut i = from;
    while i <= last {
        if hay[i] == needle[0] && hay[i..i + needle.len()] == *needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 1-based line of an index: `content.slice(0, idx).split('\n').length`
fn line_of(content: &[u16], idx: usize) -> usize {
    content[..idx.min(content.len())].iter().filter(|&&c| c == 10).count() + 1
}

fn hit_within_u(content: &U, hits: &[usize], within: &U) -> std::result::Result<(usize, usize), String> {
    let mut marks = vec![];
    let mut i = index_of(content, within, 0);
    while let Some(at) = i {
        if marks.len() >= 2 {
            break;
        }
        marks.push(at);
        i = index_of(content, within, at + 1);
    }
    if marks.is_empty() {
        return Err("the `within` landmark matches nothing in the file — copy it verbatim from a fresh read".into());
    }
    if marks.len() > 1 {
        return Err("the `within` landmark itself occurs more than once — pick a more distinctive one".into());
    }
    let after: Vec<usize> = hits.iter().copied().filter(|&h| h >= marks[0]).collect();
    if after.is_empty() {
        let lines: Vec<usize> = hits.iter().map(|&h| line_of(content, h)).collect();
        let cap = 8;
        let shown = lines.iter().take(cap).map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
        let more = if lines.len() > cap { format!(" (showing {cap} of {})", lines.len()) } else { String::new() };
        return Err(format!(
            "every occurrence of `find` sits ABOVE the landmark (landmark at line {}, occurrences at lines {shown}{more}) — `within` must be text above the edit site",
            line_of(content, marks[0])
        ));
    }
    let hit = *after.iter().min().unwrap();
    Ok((hit, line_of(content, hit)))
}

/// hitWithin(content, hits, within) → { hit, line } | { error }
pub fn hit_within(content: &str, hits: &[usize], within: &str) -> Value {
    match hit_within_u(&u(content), hits, &u(within)) {
        Ok((hit, line)) => json!({"hit": hit, "line": line}),
        Err(error) => json!({ "error": error }),
    }
}

fn unique_anchors_u(content: &U, find: &U, hits: &[usize]) -> Vec<(usize, Option<String>)> {
    let lines: Vec<&[u16]> = content.split(|&c| c == 10).collect();
    hits.iter()
        .map(|&at| {
            let mut from = line_of(content, at) - 1;
            let mut to = line_of(content, (at + find.len().max(1)).saturating_sub(1)) - 1;
            let ln = line_of(content, at);
            loop {
                let hi = (to + 1).min(lines.len());
                let lo = from.min(hi);
                let mut probe: U = vec![];
                for (k, l) in lines[lo..hi].iter().enumerate() {
                    if k > 0 {
                        probe.push(10);
                    }
                    probe.extend_from_slice(l);
                }
                if probe.len() > ANCHOR_CAP {
                    return (ln, None);
                }
                let mut n = 0;
                let mut j = index_of(content, &probe, 0);
                while let Some(p) = j {
                    if n >= 2 {
                        break;
                    }
                    n += 1;
                    j = index_of(content, &probe, p + 1);
                }
                if n == 1 {
                    return (ln, Some(un(&probe)));
                }
                let can_up = from > 0;
                let can_down = to + 1 < lines.len();
                if !can_up && !can_down {
                    return (ln, None);
                }
                if can_up {
                    from -= 1;
                }
                if can_down {
                    to += 1;
                }
            }
        })
        .collect()
}

/// uniqueAnchors(content, findText, hits) → [{ line, probe | null }]
pub fn unique_anchors(content: &str, find_text: &str, hits: &[usize]) -> Value {
    let r = unique_anchors_u(&u(content), &u(find_text), hits);
    Value::Array(r.into_iter().map(|(line, probe)| json!({"line": line, "probe": probe})).collect())
}

// ── text helpers ─────────────────────────────────────────────────────────────────────────────

/// Workspace-layer reasons name library verbs; a model knows the tools.
fn to_tool_words(s: &str) -> String {
    static RES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let res = RES.get_or_init(|| {
        [
            (r"(?-u:\b)structure\(\)", "code_map"),
            (r"(?-u:\b)find\(\)", "code_find"),
            (r"(?-u:\b)ask\(\)", "code_ask"),
            (r"(?-u:\b)expectedHash(?-u:\b)", "`at`"),
            (r"(?-u:\b)writeWholeFile(?-u:\b)", "code_write"),
        ]
        .into_iter()
        .map(|(p, r)| (Regex::new(p).unwrap(), r))
        .collect()
    });
    let mut out = s.to_string();
    for (re, rep) in res {
        out = re.replace_all(&out, regex::NoExpand(rep)).into_owned();
    }
    out
}

/// `toToolWords(v)` (a falsy value is the empty string)
fn tw(v: &Value) -> String {
    if truthy(v) {
        to_tool_words(&js_str(v))
    } else {
        String::new()
    }
}

fn diag_lines(l: &Value, n: usize) -> String {
    items(l)
        .iter()
        .take(n)
        .map(|d| {
            format!(
                "  - {}{}{}",
                if truthy(get(d, "source")) { format!("[{}] ", sg(d, "source")) } else { String::new() },
                if truthy(get(d, "line")) { format!("line {}: ", sg(d, "line")) } else { String::new() },
                sg(d, "message")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn failed_validators(res: &Value) -> Vec<String> {
    list(res, "validation").iter().filter(|v| get(v, "status").as_str() == Some("failed")).map(|v| js_item(get(v, "validator"))).collect()
}

fn render_diff(res: &Value) -> String {
    let text = list(res, "diff").iter().map(|h| js_item(get(h, "text"))).collect::<Vec<_>>().join("\n\n");
    if text.is_empty() {
        return String::new();
    }
    let n = len16(&text);
    if n > DIFF_CAP {
        format!("{}\n…[diff truncated at {DIFF_CAP} of {n} chars — code_read the file to see the rest]", slice16(&text, DIFF_CAP))
    } else {
        text
    }
}

fn render_receipt(verb: &str, res: &Value) -> String {
    let new_at_truthy = truthy(get(res, "readbackHash"));
    let new_at = if new_at_truthy { js_str(get(res, "readbackHash")) } else { "null".to_string() };
    let warn = failed_validators(res);
    let head = if get(res, "outcome").as_str() == Some("ok") {
        format!("applied: {} (ok) — validated before commit, published atomically, verified by readback.  at={new_at}", sg(res, "file"))
    } else {
        let reason = if truthy(get(res, "reason")) { tw(get(res, "reason")) } else { "read the file again before trying anything further".to_string() };
        format!(
            "{verb} on {} finished as {} — {reason}{}",
            sg(res, "file"),
            sg(res, "outcome"),
            if new_at_truthy { format!("  at={new_at}") } else { String::new() }
        )
    };
    let mut lines = vec![head];
    if truthy(get(res, "baselineBroken")) {
        lines.push(format!("  note: the file already had {} diagnostic(s) before this edit; none were added.", list(res, "remaining").len()));
    }
    if !warn.is_empty() {
        lines.push(format!("  WARNING: {} validator(s) reported issues — not blocking this commit, but worth a look before calling it done.", warn.join(", ")));
    }
    let diff = render_diff(res);
    if !diff.is_empty() {
        lines.push(diff);
    }
    if get(res, "outcome").as_str() == Some("ok") {
        lines.push(format!("  [the new at is {new_at} — pass it to the next code_edit on {}]", sg(res, "file")));
    }
    lines.join("\n")
}

// ── content-addressed edits ──────────────────────────────────────────────────────────────────

enum Resolved {
    Err { error: String, at: Value },
    Ok { edits: Vec<Value>, at: Value },
}

const WAY_OUT: &str = " If the anchor keeps missing or the region is duplicated, skip anchors entirely: code_read the whole file, then land the complete new content with ONE code_write using the `at` from that same read — whole-file replace needs no anchor and either lands or conflicts honestly.";

/// Resolve `find` edits against a fresh whole-file read into line-range targets.
fn resolve_find_edits(ws: &dyn ToolWorkspace, file: &str, edits: &[Value]) -> Result<Resolved> {
    let gr = ws.call("readFile", &[json!(file), json!({"maxLines": 9007199254740991i64})])?;
    if !truthy(&gr) || !truthy(get(&gr, "ok")) {
        let reason = if truthy(&gr) && truthy(get(&gr, "reason")) { tw(get(&gr, "reason")) } else { to_tool_words("unknown") };
        return Ok(Resolved::Err { error: format!("cannot read {file}: {reason}"), at: Value::Null });
    }
    let content = u(&sg(&gr, "body"));
    let gfile = sg(&gr, "file");
    let mut out = vec![];
    for e0 in edits {
        let mut e: Map<String, Value> = e0.as_object().cloned().unwrap_or_default();
        if !e.contains_key("find") {
            out.push(Value::Object(e));
            continue;
        }
        let find = u(&js_str(&e["find"]));
        let mut hits = vec![];
        let mut i = index_of(&content, &find, 0);
        while let Some(at) = i {
            hits.push(at);
            i = index_of(&content, &find, at + 1);
        }
        let mut within_note = String::new();
        let within = e.get("within").cloned().unwrap_or(Value::Null);
        if hits.len() > 1 && truthy(&within) {
            match hit_within_u(&content, &hits, &u(&js_str(&within))) {
                Err(err) => within_note = format!(" (`within` did not settle it — {err}.)"),
                Ok((hit, _)) => hits = vec![hit],
            }
        }
        e.remove("within");
        if hits.len() != 1 {
            let mut hint = String::new();
            if hits.is_empty() {
                let find_s = un(&find);
                let probe = find_s.split('\n').map(|l| l.trim().to_string()).find(|l| len16(l) > 8);
                if let Some(probe) = probe {
                    match index_of(&content, &u(&probe), 0) {
                        Some(j) => {
                            let ln = line_of(&content, j);
                            hint = format!(
                                " The nearest similar text (\"{}\") is at line {ln} — copy the CURRENT text from there verbatim, whitespace included.",
                                slice16(&probe, 60)
                            );
                        }
                        None => hint = " No line of it appears in the file at all — re-read before editing.".into(),
                    }
                }
            } else {
                let shown = &hits[..hits.len().min(5)];
                let more = hits.len() - shown.len();
                let uniq = unique_anchors_u(&content, &find, shown);
                let where_ = uniq.iter().map(|(l, _)| l.to_string()).collect::<Vec<_>>().join(", ") + &if more > 0 { format!(" (+{more} more)") } else { String::new() };
                hint = if uniq.iter().any(|(_, p)| p.is_some()) {
                    let anchors: String = uniq
                        .iter()
                        .map(|(line, probe)| match probe {
                            Some(p) => format!("\n--- unique anchor for the line {line} occurrence ---\n{p}"),
                            None => format!("\n--- line {line}: no compact unique anchor exists for this one — use the whole-file path ---"),
                        })
                        .collect();
                    format!(" It occurs at lines {where_}.{within_note} Fastest retry: the SAME find plus within:<a distinctive line from ABOVE the one you mean, verbatim>. Or copy ONE anchor below as `find`, EXACTLY as printed — the body must then reproduce the extra surrounding lines unchanged:{anchors}\n--- end anchors ---")
                } else {
                    format!(" It occurs at lines {where_}.{within_note} Retry the SAME find with within:<a distinctive line from ABOVE the one you mean, verbatim> — the duplicated region is too large for any compact anchor to tell the copies apart.")
                };
            }
            let error = if !hits.is_empty() {
                format!("`find` matches {} places in {gfile} — it must match exactly one.{hint}{WAY_OUT}", hits.len())
            } else {
                format!("`find` matches NOTHING in {gfile} — the text is not there as written.{hint}{WAY_OUT}")
            };
            return Ok(Resolved::Err { error, at: get(&gr, "at").clone() });
        }
        let start = hits[0];
        let end = start + find.len();
        // content.lastIndexOf('\n', start - 1) + 1 (a negative fromIndex clamps to 0)
        let mut line_start = 0;
        if !content.is_empty() {
            let mut k = start.saturating_sub(1).min(content.len() - 1) as isize;
            while k >= 0 {
                if content[k as usize] == 10 {
                    line_start = k as usize + 1;
                    break;
                }
                k -= 1;
            }
        }
        let line_end = index_of(&content, &[10], end).unwrap_or(content.len());
        let body = u(&js_str(e.get("body").unwrap_or(&Value::Null)));
        let mut nb: U = vec![];
        if line_start < start {
            nb.extend_from_slice(&content[line_start..start]);
        }
        nb.extend_from_slice(&body);
        if end < line_end {
            nb.extend_from_slice(&content[end..line_end]);
        }
        e.insert("body".into(), json!(un(&nb)));
        let from_line = line_of(&content, line_start);
        let to_line = line_of(&content, line_end);
        e.insert("target".into(), json!(format!("{gfile}:{from_line}-{to_line}")));
        e.remove("find");
        out.push(Value::Object(e));
    }
    Ok(Resolved::Ok { edits: out, at: get(&gr, "at").clone() })
}

// ── the tools ────────────────────────────────────────────────────────────────────────────────

/// createTools options (besides the two callbacks)
#[derive(Clone, Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolOptions {
    pub grep_max_chars: usize,
}

impl Default for ToolOptions {
    fn default() -> Self {
        ToolOptions { grep_max_chars: 12000 }
    }
}

pub struct Tools {
    pub host: Arc<dyn ToolHost>,
    pub opts: ToolOptions,
}

/// One render's view: the resolved workspace and `workspaces()` read once.
struct Cx<'a> {
    t: &'a Tools,
    ws: Arc<dyn ToolWorkspace>,
    known: std::cell::OnceCell<Vec<Value>>,
}

const GREP_OUTPUTS: [&str; 3] = ["lines", "files", "matches"];

fn known_line(known: &[Value]) -> String {
    if known.is_empty() {
        "no workspaces are registered".into()
    } else {
        format!(
            "known workspaces: {}",
            known
                .iter()
                .map(|w| format!("{}{}", sg(w, "id"), if truthy(get(w, "root")) { format!(" ({})", sg(w, "root")) } else { String::new() }))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl Tools {
    pub fn new(host: Arc<dyn ToolHost>, opts: ToolOptions) -> Tools {
        Tools { host, opts }
    }

    /// render(name, args) — never fails.
    pub fn render(&self, name: &str, args: &Value) -> String {
        let a: Value = if args.is_object() { args.clone() } else { json!({}) };
        if !is_tool(name) {
            return format!("({name}) — not an okcode tool; known: {}", TOOLS.join(", "));
        }
        let run = || -> Result<String> {
            let known: std::cell::OnceCell<Vec<Value>> = std::cell::OnceCell::new();
            let ws = match self.ws_for_known(get(&a, "workspace"), &known)? {
                Ok(ws) => ws,
                Err(error) => return Ok(format!("({name}) — {error}")),
            };
            let cx = Cx { t: self, ws, known };
            match name {
                "code_map" => cx.code_map(),
                "code_find" => cx.code_find(&a),
                "code_grep" => cx.code_grep(&a),
                "code_glob" => cx.code_glob(&a),
                "code_outline" => cx.code_outline(&a),
                "code_read" => cx.code_read(&a),
                "code_package" => cx.code_package(&a),
                "code_ask" => cx.code_ask(&a),
                "code_edit" => cx.code_edit(&a),
                _ => cx.code_write(&a),
            }
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => format!("({name}) failed: {}", slice16(&e.message, 200)),
            Err(_) => format!("({name}) failed: internal error"),
        }
    }

    /// schemas() with the host's current workspaces()
    pub fn schemas(&self) -> Value {
        schemas_for(&self.host.workspaces())
    }

    /// resolve a hint the way wsFor does (exactly one known workspace = that one)
    pub fn ws_for(&self, hint: Option<&str>) -> Result<std::result::Result<Arc<dyn ToolWorkspace>, String>> {
        let h = match hint {
            Some(s) => json!(s),
            None => Value::Null,
        };
        let known = std::cell::OnceCell::new();
        self.ws_for_known(&h, &known)
    }

    fn ws_for_known(&self, hint: &Value, known: &std::cell::OnceCell<Vec<Value>>) -> Result<std::result::Result<Arc<dyn ToolWorkspace>, String>> {
        let empty = hint.is_null() || hint.as_str() == Some("");
        let h = if empty { None } else { Some(js_str(hint)) };
        let mut ws = self.host.workspace(h.as_deref())?;
        if ws.is_none() && empty {
            let list = known.get_or_init(|| self.host.workspaces());
            if list.len() == 1 {
                ws = self.host.workspace(Some(&js_str(get(&list[0], "id"))))?;
            }
            if ws.is_none() {
                return Ok(Err(format!("which workspace? pass `workspace` — {}", known_line(known.get_or_init(|| self.host.workspaces())))));
            }
        }
        match ws {
            Some(w) => Ok(Ok(w)),
            None => Ok(Err(format!("no workspace \"{}\" — {}", js_str(hint), known_line(known.get_or_init(|| self.host.workspaces()))))),
        }
    }
}

/// `{ a, b: undefined }` → only the defined keys
fn obj(pairs: Vec<(&str, Option<Value>)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            m.insert(k.to_string(), v);
        }
    }
    Value::Object(m)
}

fn opt_num(v: Option<f64>) -> Option<Value> {
    v.map(jnum)
}

fn opt_list(v: &Option<Vec<String>>) -> Option<Value> {
    v.as_ref().map(|l| json!(l))
}

fn list_arg(v: &Value) -> Option<Vec<String>> {
    g::split_list(v)
}

fn case_arg(a: &Value) -> Value {
    let v = if !get(a, "case_sensitive").is_null() {
        get(a, "case_sensitive")
    } else if !get(a, "caseSensitive").is_null() {
        get(a, "caseSensitive")
    } else {
        get(a, "case")
    };
    if v.as_str() == Some("smart") {
        return json!("smart");
    }
    if v == &json!(true) || v.as_str() == Some("true") || v.as_str() == Some("sensitive") {
        return json!(true);
    }
    json!(false)
}

/// `a.k1 ?? a.k2 ?? …`
fn coalesce<'a>(a: &'a Value, keys: &[&str]) -> &'a Value {
    for k in keys {
        let v = get(a, k);
        if !v.is_null() {
            return v;
        }
    }
    &NULL
}

/// `a.k1 || a.k2 || …` (the first truthy, else null)
fn first_truthy<'a>(a: &'a Value, keys: &[&str]) -> &'a Value {
    for k in keys {
        let v = get(a, k);
        if truthy(v) {
            return v;
        }
    }
    &NULL
}

/// `String(x || '')`
fn str_or_empty(v: &Value) -> String {
    if truthy(v) {
        js_str(v)
    } else {
        String::new()
    }
}

impl<'a> Cx<'a> {
    fn call(&self, method: &str, args: &[Value]) -> Result<Value> {
        self.ws.call(method, args)
    }
    fn id(&self) -> String {
        self.ws.id()
    }

    fn root_of(&self) -> Option<String> {
        let id = self.id();
        if let Some(entry) = self.known.get_or_init(|| self.t.host.workspaces()).iter().find(|w| get(w, "id").as_str() == Some(id.as_str())) {
            if truthy(get(entry, "root")) {
                return Some(js_str(get(entry, "root")));
            }
        }
        self.ws.access_root()
    }

    /// A model often pastes an ABSOLUTE path; inside this workspace's root, that is the relative path.
    fn rel_of(&self, p: &Value) -> String {
        let s = str_or_empty(p).replace('\\', "/");
        let root = match self.root_of() {
            Some(r) if !r.is_empty() => r,
            _ => return s,
        };
        let r = root.replace('\\', "/");
        let r = r.trim_end_matches('/');
        let pre = format!("{r}/");
        match s.strip_prefix(&pre) {
            Some(rest) => rest.to_string(),
            None => s,
        }
    }

    fn root_label(&self) -> String {
        match self.root_of() {
            Some(r) if !r.is_empty() => format!(" ({r})"),
            _ => String::new(),
        }
    }

    fn code_map(&self) -> Result<String> {
        let s = self.call("stats", &[])?;
        let st = self.call("structure", &[])?;
        let deps = self.call("packages", &[]).ok();
        let dep_lines: Option<Vec<String>> = deps.as_ref().and_then(|d| {
            if truthy(get(d, "error")) {
                return None;
            }
            let l = items(d);
            if l.is_empty() {
                return None;
            }
            Some(
                l.iter()
                    .filter(|p| get(p, "source").as_str() != Some("transitive"))
                    .map(|p| {
                        format!(
                            "  {}@{}{}",
                            sg(p, "name"),
                            or_str(p, "version", "?"),
                            if truthy(get(p, "description")) { format!(" — {}", slice16(&js_str(get(p, "description")), 120)) } else { String::new() }
                        )
                    })
                    .collect(),
            )
        });
        let by_lang = match get(&s, "byLang") {
            Value::Object(o) => o.iter().map(|(l, n)| format!("{l} {}", js_str(n))).collect::<Vec<_>>().join(", "),
            _ => String::new(),
        };
        let largest = list(&st, "largest");
        let parts = vec![
            format!("{}{}", self.id(), self.root_label()),
            format!(
                "{} files, {} symbols — {} with structure, {} searchable, {} opaque",
                sg(&s, "files"),
                sg(&s, "symbols"),
                sg(&s, "withSymbols"),
                sg(&s, "searchable"),
                sg(&s, "opaque")
            ),
            format!("languages: {by_lang}"),
            match &dep_lines {
                Some(l) if !l.is_empty() => format!("dependencies (code_package for exports):\n{}", l.join("\n")),
                _ => String::new(),
            },
            if !largest.is_empty() {
                format!(
                    "largest symbols:\n{}",
                    largest
                        .iter()
                        .take(10)
                        .map(|x| format!("  {} lines  {}  {}", pad_start(sg(x, "lines"), 5), sg(x, "name"), sg(x, "file")))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            } else {
                String::new()
            },
        ];
        Ok(parts.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n"))
    }

    fn code_ask(&self, a: &Value) -> Result<String> {
        let q = str_or_empty(first_truthy(a, &["question", "query"]));
        let vector = if get(a, "vector").is_null() { None } else { Some(get(a, "vector").clone()) };
        if q.is_empty() && vector.is_none() {
            return Ok("(code_ask) — needs `question`: what the code you are looking for DOES, in your own words.".into());
        }
        let profile = first_truthy(a, &["embedder", "profile"]).clone();
        let query = match &vector {
            Some(v) => obj(vec![
                ("text", if q.is_empty() { None } else { Some(json!(q)) }),
                ("vector", Some(v.clone())),
                ("identity", if truthy(get(a, "identity")) { Some(get(a, "identity").clone()) } else { None }),
            ]),
            None => json!(q),
        };
        let asked: Vec<String> = list_arg(coalesce(a, &["paths", "path"])).unwrap_or_default().iter().map(|p| self.rel_of(&json!(p))).collect();
        let mut lib_path: Option<String> = None;
        if self.ws.has("isLibraryPath") {
            for p in &asked {
                if truthy(&self.call("isLibraryPath", &[json!(p)])?) {
                    lib_path = Some(p.clone());
                    break;
                }
            }
        }
        let opts = json!({
            "limit": jnum(num_or(get(a, "limit"), 10.0)),
            "profile": profile,
            "embedder": profile,
            "path": lib_path,
        });
        let hits = match self.call("ask", &[query, opts]) {
            Ok(h) => h,
            Err(err) => {
                if code_of(&err) == "OKCODE_LIBRARY_NOT_INDEXED" {
                    let lp = lib_path.clone().unwrap_or_else(|| "null".into());
                    return Ok(format!("(code_ask \"{q}\") — {lp} is library code, which is never embedded: searching it by meaning is not supported. Use code_grep with paths: [\"{lp}\"] for its text, or code_find {{query, path: \"{lp}\"}} for a name."));
                }
                if code_of(&err) == "OKCODE_NO_EMBEDDINGS" {
                    return Ok(format!("(code_ask \"{q}\") — {} has no semantic index (no embedder configured). Use code_find for names and doc prose, or code_grep for exact text.", self.id()));
                }
                return Err(err);
            }
        };
        let h = arr(&hits).unwrap_or(Arr { items: &[], props: &NULL });
        if h.items.is_empty() {
            return Ok(format!("(code_ask \"{q}\") — nothing in {} matched.", self.id()));
        }
        let rows: Vec<String> = h
            .items
            .iter()
            .map(|h| {
                let how = match get(h, "via").as_str().unwrap_or("") {
                    "vector" => "meaning",
                    "content" => "file text",
                    "content~" | "fts~" => "loose match",
                    _ => "name/doc",
                };
                let where_ = format!("{}:{}", sg(h, "file"), sg(h, "lines"));
                let w = get(h, "window");
                let body = if truthy(w) {
                    format!(
                        "\n      {}:{}-{}  at={}\n{}",
                        sg(h, "file"),
                        sg(w, "lineStart"),
                        sg(w, "lineEnd"),
                        sg(h, "at"),
                        sg(w, "text").split('\n').map(|l| format!("      | {l}")).collect::<Vec<_>>().join("\n")
                    )
                } else if truthy(get(h, "quote")) {
                    format!("\n      {}", sg(h, "quote"))
                } else if truthy(get(h, "doc")) {
                    format!("\n      {}", sg(h, "doc"))
                } else {
                    String::new()
                };
                let nm = if truthy(get(h, "path")) { sg(h, "path") } else { sg(h, "name") };
                format!("  [{how}] {nm}  {where_}{body}")
            })
            .collect();
        let head = if q.is_empty() { "a query vector".to_string() } else { format!("\"{q}\"") };
        let building = get(h.props, "vectorsBuilding");
        Ok(format!(
            "Answering {head} from {}:\n{}{}{}{}",
            self.id(),
            rows.join("\n"),
            if truthy(get(h.props, "truncated")) { format!("\n  [top {} match(es) — raise limit to search deeper]", h.items.len()) } else { String::new() },
            if truthy(get(h.props, "contentEyeSkipped")) { format!("\n  [{} is indexed fts: symbols — no file-text eye; names/doc/meaning only]", self.id()) } else { String::new() },
            if truthy(building) {
                let n = |k: &str| building.get(k).and_then(Value::as_u64).unwrap_or(0);
                format!(
                    "\n  [meaning is still being indexed ({} of {} files embedded) — this answer leans on names and text; ask again later for a fuller one]",
                    n("done"),
                    n("done") + n("pending")
                )
            } else {
                String::new()
            }
        ))
    }

    fn code_package(&self, a: &Value) -> Result<String> {
        if truthy(get(a, "name")) {
            let name = js_str(get(a, "name"));
            let p = self.call("package", &[json!(name)])?;
            if truthy(get(&p, "error")) {
                return Ok(format!("(code_package {}) — {}", sg(a, "name"), sg(&p, "error")));
            }
            let surface = list(&p, "surface");
            let members = list(&p, "members");
            let lines = vec![
                format!(
                    "{}@{} ({}){}",
                    sg(&p, "name"),
                    or_str(&p, "version", "?"),
                    sg(&p, "source"),
                    if truthy(get(&p, "description")) { format!(" — {}", sg(&p, "description")) } else { String::new() }
                ),
                format!("entry: {}{}", or_str(&p, "main", "(none)"), if truthy(get(&p, "types")) { format!("   types: {}", sg(&p, "types")) } else { String::new() }),
                format!(
                    "exports ({}): {}{}",
                    or_str(&p, "surfaceFrom", "not found"),
                    { let j = surface.iter().map(|x| if x.is_null() { String::new() } else { js_str(x) }).collect::<Vec<_>>().join(", "); if j.is_empty() { "(none)".to_string() } else { j } },
                    if truthy(get(&p, "surfaceTruncated")) { format!("  [PARTIAL: cut at {} names]", surface.len()) } else { String::new() }
                ),
                if !members.is_empty() {
                    format!(
                        "members: {}{}",
                        members.iter().map(js_item).collect::<Vec<_>>().join(", "),
                        if truthy(get(&p, "membersTruncated")) { format!("  [PARTIAL: cut at {} names]", members.len()) } else { String::new() }
                    )
                } else {
                    String::new()
                },
            ];
            return Ok(lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n"));
        }
        let l = self.call("packages", &[])?;
        if truthy(get(&l, "error")) {
            return Ok(format!("(code_package) — {}", sg(&l, "error")));
        }
        let rows = items(&l);
        Ok(format!(
            "{} dependencies of {}:\n{}",
            rows.len(),
            self.id(),
            rows.iter()
                .map(|p| format!(
                    "  {}@{} ({}) — {} exported name(s){}",
                    sg(p, "name"),
                    or_str(p, "version", "?"),
                    sg(p, "source"),
                    list(p, "surface").len(),
                    if truthy(get(p, "description")) { format!(" — {}", slice16(&js_str(get(p, "description")), 120)) } else { String::new() }
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ))
    }

    fn code_grep(&self, a: &Value) -> Result<String> {
        let q = {
            let x = coalesce(a, &["pattern", "text", "query"]);
            if x.is_null() {
                String::new()
            } else {
                js_str(x)
            }
        };
        if q.is_empty() {
            return Ok("(code_grep) — needs `pattern`: the text (or, with regex: true, the expression) to look for.".into());
        }
        if !self.ws.has("grep") {
            return self.legacy_grep(&q, a);
        }
        let raw_out = coalesce(a, &["output", "mode"]);
        let output = if raw_out.is_null() || raw_out.as_str() == Some("") { "lines".to_string() } else { js_str(raw_out).to_lowercase() };
        if !GREP_OUTPUTS.contains(&output.as_str()) {
            return Ok(format!(
                "(code_grep \"{q}\") — output must be one of {} (got '{}').",
                GREP_OUTPUTS.iter().map(|o| format!("'{o}'")).collect::<Vec<_>>().join(", "),
                js_str(raw_out)
            ));
        }
        let glob = list_arg(coalesce(a, &["glob", "globs", "include"]));
        let paths = list_arg(coalesce(a, &["paths", "path", "dir"]));
        let max_chars = 1000f64.max(num(coalesce(a, &["max_chars", "maxChars"])).unwrap_or(self.t.opts.grep_max_chars as f64)) as i64;
        let max_matches = num(coalesce(a, &["max_matches", "maxMatches"])).unwrap_or(if output == "matches" { 500.0 } else { 100.0 });
        let max_files = num(coalesce(a, &["max_files", "maxFiles", "limit"])).or(if output == "files" { Some(500.0) } else { None });
        let opts = obj(vec![
            ("regex", Some(json!(get(a, "regex") == &json!(true) || get(a, "regex").as_str() == Some("true")))),
            ("caseSensitive", Some(case_arg(a))),
            ("glob", opt_list(&glob)),
            ("paths", opt_list(&paths)),
            ("output", Some(json!(output))),
            ("page", opt_num(num(get(a, "page")))),
            ("offset", opt_num(num(get(a, "offset")))),
            ("context", opt_num(num(get(a, "context")))),
            ("before", opt_num(num(get(a, "before")))),
            ("after", opt_num(num(get(a, "after")))),
            ("maxMatches", Some(jnum(max_matches))),
            ("maxPerFile", Some(jnum(num(coalesce(a, &["max_per_file", "maxPerFile"])).unwrap_or(10.0)))),
            ("maxFiles", opt_num(max_files)),
        ]);
        let r = match self.call("grep", &[json!(q), opts]) {
            Ok(r) => r,
            Err(err) => {
                if code_of(&err) == "GREP_BAD_PATTERN" || code_of(&err) == "OKCODE_BAD_PATH" {
                    return Ok(format!("(code_grep \"{q}\") — {}", err.message));
                }
                return Err(err);
            }
        };
        let lib = get(&r, "library");
        let cap_note = [
            if truthy(get(lib, "missing")) {
                format!("  [NOT FOUND: nothing at {} — not installed, or a wrong path; no match there means nothing. Check the package name (code_files or a listing of node_modules)]", list(lib, "missing").iter().map(js_item).collect::<Vec<_>>().join(", "))
            } else {
                String::new()
            },
            if truthy(get(lib, "truncated")) {
                format!("  [PARTIAL: listing {} stopped at {} files — narrow the path (one package, or its src/) for a complete answer]", list(lib, "paths").iter().map(js_item).collect::<Vec<_>>().join(", "), sg(lib, "cap"))
            } else {
                String::new()
            },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
        let with_cap = |text: String| -> String {
            if cap_note.is_empty() {
                return text;
            }
            match text.find('\n') {
                Some(i) => format!("{}\n{cap_note}{}", &text[..i], &text[i..]),
                None => format!("{text}\n{cap_note}"),
            }
        };
        let scope = [
            glob.as_ref().map(|g| format!("glob {}", g.join(" "))).unwrap_or_default(),
            paths.as_ref().map(|p| format!("under {}", p.join(", "))).unwrap_or_default(),
            if truthy(get(&r, "regex")) { "regex".into() } else { String::new() },
            if truthy(get(&r, "ignoreCase")) { "ignoring case".into() } else { "case-sensitive".into() },
            if truthy(lib) { "library code read directly, not indexed".into() } else { String::new() },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
        let files = list(&r, "files");
        let counts: &[Value] = if truthy(get(&r, "counts")) { list(&r, "counts") } else { files };
        let total_v = get(&r, "total");
        let (t_lines, t_files) = if truthy(total_v) {
            (ni(get(total_v, "lines")), ni(get(total_v, "files")))
        } else {
            (files.iter().map(|f| ni(get(f, "count"))).sum(), files.len() as i64)
        };
        let searched = sg(&r, "searched");
        let ws_id = self.id();
        if t_lines == 0 {
            return Ok(with_cap(format!(
                "(code_grep \"{q}\") — {} in {ws_id} ({searched} files searched; {scope}).{} If you expected it to exist, it does not as written: try a shorter distinctive fragment, the other case, or code_ask to search by meaning.",
                if truthy(get(&r, "regex")) { "no line matches that expression" } else { "that text appears nowhere" },
                if glob.is_some() || paths.is_some() { " Check the glob/paths filter, or drop it." } else { "" }
            )));
        }
        let plural = |n: i64| if n == 1 { "" } else { "s" };
        let n_lines = format!("{t_lines} matching line{}", plural(t_lines));
        let n_files = format!("{t_files} file{}", plural(t_files));
        let page = ni(get(&r, "page"));
        let r_offset = ni(get(&r, "offset"));
        let page_note = if page > 1 || r_offset > 0 {
            format!(" — page {}{}", sg(&r, "page"), if r_offset != 0 { format!(", from {}", sg(&r, "offset")) } else { String::new() })
        } else {
            String::new()
        };
        let read_hint = "  [code_read \"<file>:<from>-<to>\" for more around a line; code_edit {file, find, body} to change one]";
        let other = |o: &str| GREP_OUTPUTS.iter().filter(|x| **x != o).map(|x| format!("output:'{x}'")).collect::<Vec<_>>().join(" or ");
        const RESERVE: usize = 400;
        let fit = |head: &str, rows: &[String]| -> usize {
            let mut used = len16(head) + RESERVE;
            for (i, r) in rows.iter().enumerate() {
                used += len16(r) + 1;
                if used as i64 > max_chars {
                    return i;
                }
            }
            rows.len()
        };

        if output == "files" {
            let head = format!("code_grep \"{q}\" in {ws_id} — {n_lines} in {n_files} ({searched} searched; {scope}){page_note}; matching lines per file:");
            let rows: Vec<String> = files.iter().map(|f| format!("  {}: {}", sg(f, "file"), sg(f, "count"))).collect();
            let cut = fit(&head, &rows);
            let shown_to = r_offset + cut as i64;
            let mut tail = vec![];
            if shown_to < t_files {
                let rest_files = t_files - shown_to;
                let rest_lines: i64 = counts.iter().skip(shown_to.max(0) as usize).map(|f| ni(get(f, "count"))).sum();
                let how = if cut < rows.len() { format!("offset={shown_to}") } else { format!("page={}", page + 1) };
                tail.push(format!("  [{rest_files} more file(s) with {rest_lines} matching line(s) not shown — narrow with glob/paths, or continue with {how}]"));
            }
            tail.push("  [output:'lines' shows the lines themselves; paths:[\"<file>\"] for one file]".to_string());
            let mut all = vec![head];
            all.extend(rows[..cut].iter().cloned());
            all.extend(tail);
            return Ok(with_cap(all.join("\n")));
        }

        if output == "matches" {
            let distinct = list(&r, "distinct");
            let t_distinct = ni(get(total_v, "distinct"));
            let t_occ = sg(total_v, "occurrences");
            let head = format!(
                "code_grep \"{q}\" in {ws_id} — {t_distinct} distinct match{} ({t_occ} occurrence{} on {n_lines} in {n_files}; {searched} searched; {scope}){page_note}; count, match, where:",
                if t_distinct == 1 { "" } else { "es" },
                if get(total_v, "occurrences").as_f64() == Some(1.0) { "" } else { "s" }
            );
            let w = if distinct.is_empty() { 1 } else { len16(&sg(&distinct[0], "count")) };
            let rows: Vec<String> = distinct
                .iter()
                .map(|d| {
                    format!(
                        "  {}  {}  ({})",
                        pad_start(sg(d, "count"), w),
                        sg(d, "text"),
                        if get(d, "files").as_f64() == Some(1.0) { sg(d, "first") } else { format!("{} files", sg(d, "files")) }
                    )
                })
                .collect();
            let cut = fit(&head, &rows);
            let shown_to = r_offset + cut as i64;
            let mut tail = vec![];
            if shown_to < t_distinct {
                let how = if cut < rows.len() { format!("offset={shown_to}") } else { format!("page={}", page + 1) };
                tail.push(format!("  [{} more distinct match(es) not shown — narrow the pattern or glob/paths, or continue with {how}]", t_distinct - shown_to));
            }
            tail.push("  [output:'lines' (with paths/glob) shows where a match is; output:'files' counts per file]".to_string());
            let mut all = vec![head];
            all.extend(rows[..cut].iter().cloned());
            all.extend(tail);
            return Ok(with_cap(all.join("\n")));
        }

        let head = format!("code_grep \"{q}\" in {ws_id} — {n_lines} in {n_files} ({searched} searched; {scope}){page_note}:");
        let matches = list(&r, "matches");
        let mut by_file: Vec<(String, Vec<&Value>)> = vec![];
        for m in matches {
            let f = sg(m, "file");
            match by_file.iter_mut().find(|(k, _)| *k == f) {
                Some((_, v)) => v.push(m),
                None => by_file.push((f, vec![m])),
            }
        }
        let start_of = |file: &str| -> Option<i64> {
            // Map semantics: the LAST entry of a duplicated key wins
            counts.iter().rev().find(|f| get(f, "file").as_str() == Some(file)).and_then(|f| get(f, "start").as_i64())
        };
        let with_context = matches.iter().any(|m| !list(m, "before").is_empty() || !list(m, "after").is_empty());
        struct Row {
            text: String,
            s: Option<i64>,
        }
        let mut rows: Vec<Row> = vec![];
        for (i, f) in files.iter().enumerate() {
            let file = sg(f, "file");
            let ms: &[&Value] = by_file.iter().find(|(k, _)| *k == file).map(|(_, v)| v.as_slice()).unwrap_or(&[]);
            let (h, lines) = render_grep_file(f, ms, with_context);
            let s0 = start_of(&file);
            let at = |n: Option<i64>| match (n, s0) {
                (Some(n), Some(s)) => Some(s + n),
                _ => None,
            };
            let first_n = ms.first().and_then(|m| get(m, "n").as_i64());
            if i > 0 {
                rows.push(Row { text: String::new(), s: at(first_n) });
            }
            rows.push(Row { text: h, s: at(first_n) });
            for (text, n) in lines {
                rows.push(Row { text, s: at(n) });
            }
        }
        let texts: Vec<String> = rows.iter().map(|r| r.text.clone()).collect();
        let mut cut = fit(&head, &texts);
        let mut resume: Option<i64> = None;
        let next_off = if truthy(get(&r, "next")) { Some(ni(get(get(&r, "next"), "offset"))) } else { None };
        if cut < rows.len() {
            for row in rows.iter().skip(cut) {
                if row.s.is_some() {
                    resume = row.s;
                    break;
                }
            }
            if resume.is_none() && next_off.is_some() {
                resume = next_off;
            } else {
                while cut > 0 && rows[cut - 1].s == resume {
                    cut -= 1;
                }
            }
            while cut > 0 && (rows[cut - 1].text == "--" || rows[cut - 1].text.is_empty()) {
                cut -= 1;
            }
        }
        let body = rows[..cut].iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join("\n");
        let mut tail: Vec<String> = vec![];
        let rest_from = resume.or(next_off);
        let paged = rest_from.is_some() && !counts.is_empty() && !get(&counts[0], "start").is_null();
        if paged {
            let rf = rest_from.unwrap();
            let rest_lines = t_lines - rf;
            let rest_files = counts.iter().filter(|f| ni(get(f, "start")) + ni(get(f, "count")) > rf).count();
            let how = match resume {
                Some(rs) => format!("offset={rs}"),
                None => format!("page={}", page + 1),
            };
            tail.push(format!("  [{rest_lines} more matching line(s) in {rest_files} file(s) not shown — narrow with glob/paths, use {} for the whole picture in one answer, or continue with {how}]", other("lines")));
        }
        let passed: i64 = files.iter().map(|f| ni(get(f, "passed"))).sum();
        if passed != 0 && resume.is_none() {
            tail.push(format!("  [{passed} matching line(s) passed over by max_per_file (see the file headings) — raise max_per_file, narrow with glob/paths, or use {}]", other("lines")));
        }
        if !paged && truthy(get(&r, "unsearched")) {
            tail.push(format!("  [stopped at {} lines — {} more candidate file(s) not searched; narrow with glob/paths or raise max_matches]", matches.len(), sg(&r, "unsearched")));
        } else if matches.is_empty() {
            tail.push(format!("  [page {}{} is past the end — {} page(s) in all]", sg(&r, "page"), if r_offset != 0 { format!(" from {}", sg(&r, "offset")) } else { String::new() }, sg(&r, "pages")));
        }
        tail.push(read_hint.to_string());
        Ok(with_cap(format!("{head}\n{body}{}{}", if body.is_empty() { "" } else { "\n" }, tail.join("\n"))))
    }

    /// A host whose workspace object predates grep() (only mentions()).
    fn legacy_grep(&self, q: &str, a: &Value) -> Result<String> {
        let limit = num_or(get(a, "limit"), 20.0);
        let res = self.call("mentions", &[json!(q), json!({"limit": jnum(limit)})])?;
        let h = arr(&res).unwrap_or(Arr { items: &[], props: &NULL });
        if h.items.is_empty() {
            return Ok(format!("(code_grep \"{q}\") — that exact text appears nowhere in {}. If you expected it to exist, it does not: check the spelling, try a shorter distinctive fragment, or use code_ask to search by meaning instead.", self.id()));
        }
        let total = if get(h.props, "total").is_null() { h.items.len().to_string() } else { sg(h.props, "total") };
        Ok(format!(
            "Lines containing \"{q}\" in {} ({total}):\n{}{}\n  [exact text only — read one with code_read \"<file>:<from>-<to>\" to see it in context]",
            self.id(),
            h.items.iter().map(|x| format!("  {}:{}", sg(x, "rel"), sg(x, "line"))).collect::<Vec<_>>().join("\n"),
            if truthy(get(h.props, "truncated")) && !get(h.props, "next").is_null() { format!("\n  [showing {} — continue with offset={}]", h.items.len(), sg(h.props, "next")) } else { String::new() }
        ))
    }

    fn code_glob(&self, a: &Value) -> Result<String> {
        let pats = match list_arg(coalesce(a, &["pattern", "glob", "query", "path"])) {
            Some(p) => p,
            None => return Ok("(code_glob) — needs `pattern`: a glob such as \"src/**/*.js\", \"*.test.js\" or \"components\".".into()),
        };
        let limit = num(get(a, "limit")).unwrap_or(200.0);
        let offset = num_or_zero(get(a, "offset"));
        let r = if self.ws.has("glob") {
            self.call("glob", &[json!(pats), json!({"limit": jnum(limit), "offset": jnum(offset)})])?
        } else {
            let listed = self.call("files", &[json!({})])?;
            let m = g::path_matcher(&pats, &[]).map_err(Error::msg)?;
            let all: Vec<&Value> = list(&listed, "files").iter().filter(|f| m.as_ref().map_or(true, |m| m.matches(&sg(f, "file")))).collect();
            let off = offset.max(0.0) as usize;
            let lim = limit.max(0.0) as usize;
            let files: Vec<Value> = all.iter().skip(off).take(lim).map(|v| (*v).clone()).collect();
            let truncated = off + files.len() < all.len();
            json!({"ok": true, "total": all.len(), "offset": off, "truncated": truncated, "next": if truncated { json!(off + files.len()) } else { Value::Null }, "files": files})
        };
        if !truthy(get(&r, "ok")) {
            return Ok(format!("(code_glob) — {}", sg(&r, "reason")));
        }
        let label = pats.join(" ");
        if !truthy(get(&r, "total")) {
            return Ok(format!("(code_glob \"{label}\") — no file in {} matches.", self.id()));
        }
        let rows = list(&r, "files");
        let total = ni(get(&r, "total"));
        Ok(format!(
            "code_glob \"{label}\" in {} — {} file{}:\n{}{}",
            self.id(),
            sg(&r, "total"),
            if total == 1 { "" } else { "s" },
            rows.iter()
                .map(|f| format!(
                    "  {}{}{}",
                    sg(f, "file"),
                    if !get(f, "lines").is_null() {
                        format!("  ({} lines)", sg(f, "lines"))
                    } else if !get(f, "size").is_null() {
                        format!("  ({} bytes)", sg(f, "size"))
                    } else {
                        String::new()
                    },
                    if get(f, "indexed") == &json!(false) { "  [not searchable]" } else { "" }
                ))
                .collect::<Vec<_>>()
                .join("\n"),
            if truthy(get(&r, "truncated")) && !get(&r, "next").is_null() {
                format!("\n  [showing {} of {} from offset {} — continue with offset={}]", rows.len(), sg(&r, "total"), sg(&r, "offset"), sg(&r, "next"))
            } else {
                String::new()
            }
        ))
    }

    fn code_outline(&self, a: &Value) -> Result<String> {
        let f = self.rel_of(first_truthy(a, &["file", "path", "query"]));
        if f.is_empty() {
            return Ok("(code_outline) — needs `file`: the path to break into pieces.".into());
        }
        let offset = num_or_zero(get(a, "offset"));
        let o = self.call("outline", &[json!(f), json!({"limit": jnum(num_or(get(a, "limit"), 60.0)), "offset": jnum(offset)})])?;
        if !truthy(get(&o, "ok")) {
            return Ok(format!("(code_outline \"{f}\") — {}", tw(get(&o, "reason"))));
        }
        let by = get(&o, "by").as_str().unwrap_or("");
        if by == "files" {
            let entries = list(&o, "entries");
            let total = ni(get(&o, "total"));
            let mut s = format!(
                "{} — library code, parsed directly (not indexed): {} file{}, {} lines, {} symbols\n",
                sg(&o, "dir"),
                sg(&o, "total"),
                if total == 1 { "" } else { "s" },
                sg(&o, "lines"),
                sg(&o, "symbolCount")
            );
            if truthy(get(&o, "capped")) {
                s += &format!("  [PARTIAL: stopped at {} files — outline a narrower path (one package, or its src/)]\n", sg(&o, "cap"));
            }
            s += &entries
                .iter()
                .map(|e| {
                    let names: Vec<String> = list(e, "symbols").iter().map(|x| format!("{}{}", sg(x, "name"), if truthy(get(x, "exported")) { "*" } else { "" })).collect();
                    let mut shown = names.iter().take(12).cloned().collect::<Vec<_>>().join(", ");
                    if names.len() > 12 {
                        shown += &format!(", … {} more", names.len() - 12);
                    }
                    format!("  {}  ({} lines){}", sg(e, "file"), sg(e, "lines"), if !names.is_empty() { format!("  {shown}") } else { String::new() })
                })
                .collect::<Vec<_>>()
                .join("\n");
            if truthy(get(&o, "truncated")) && !get(&o, "next").is_null() {
                s += &format!("\n  [{} files, showing {} from offset {} — continue with offset={}]", sg(&o, "total"), entries.len(), sg(&o, "offset"), sg(&o, "next"));
            }
            s += &format!("\n  [* = exported. code_outline one file for line ranges; code_find {{query, path: \"{0}\"}} looks a name up here; code_grep with paths: [\"{0}\"] searches its text]", sg(&o, "dir"));
            return Ok(s);
        }
        let what = match by {
            "region" => "structural regions",
            "section" => "sections",
            "symbol" => "symbols",
            _ => "chunks (no parser structure)",
        };
        let mut importer_line = String::new();
        if !truthy(get(&o, "library")) && self.ws.has("importers") {
            let imp = self.call("importers", &[get(&o, "file").clone(), json!({"limit": 8})])?;
            if truthy(get(&imp, "ok")) && !list(&imp, "importers").is_empty() {
                let l = list(&imp, "importers");
                let shown = l
                    .iter()
                    .map(|i| format!("{}:{}{}", sg(i, "file"), sg(i, "line"), if truthy(get(i, "via")) { format!(" (via {})", sg(i, "via")) } else { String::new() }))
                    .collect::<Vec<_>>()
                    .join(", ");
                importer_line = format!(
                    "\n  used by: {shown}{}",
                    if truthy(get(&imp, "truncated")) { format!(", … {} more", ni(get(&imp, "total")) - l.len() as i64) } else { String::new() }
                );
            }
        }
        let symbols = list(&o, "symbols");
        Ok(format!(
            "{} — {} lines in {} {what}{}\n{}{}{importer_line}{}",
            sg(&o, "file"),
            sg(&o, "lines"),
            sg(&o, "total"),
            if truthy(get(&o, "library")) { "  (library code, read directly — not indexed)".to_string() } else { format!("  at={}", sg(&o, "at")) },
            symbols
                .iter()
                .map(|s| format!(
                    "  {}L  {}  {}{}{}",
                    pad_start(sg(s, "span"), 4),
                    sg(s, "path"),
                    sg(s, "name"),
                    if truthy(get(s, "signature")) { format!(" {}", sg(s, "signature")) } else { String::new() },
                    if truthy(get(s, "doc")) { format!("  — {}", sg(s, "doc")) } else { String::new() }
                ))
                .collect::<Vec<_>>()
                .join("\n"),
            if truthy(get(&o, "truncated")) && !get(&o, "next").is_null() {
                format!("\n  [{} pieces, showing {} from offset {} — continue with offset={}]", sg(&o, "total"), symbols.len(), sg(&o, "offset"), sg(&o, "next"))
            } else {
                String::new()
            },
            if by == "section" {
                "\n  [read any one with code_read \"<file>:<from>-<to>\", a heading also as \"<file>#<heading>\"]"
            } else {
                "\n  [read any one with code_read \"<file>:<from>-<to>\"]"
            }
        ))
    }

    fn code_find(&self, a: &Value) -> Result<String> {
        let q = str_or_empty(first_truthy(a, &["query", "symbol", "name"]));
        if q.is_empty() {
            return Ok("(code_find) — needs `query`: a symbol name or a descriptive phrase.".into());
        }
        let uses = truthy(get(a, "uses"));
        let scope: Option<String> = if truthy(get(a, "path")) { Some(self.rel_of(get(a, "path"))) } else { None };
        if uses {
            if let Some(sc) = &scope {
                if !sc.is_empty() && self.ws.has("isLibraryPath") && truthy(&self.call("isLibraryPath", &[json!(sc)])?) {
                    return Ok(format!("(code_find \"{q}\") — uses are not tracked in library code ({sc} is never indexed). Use code_grep with paths: [\"{sc}\"] to find where it appears."));
                }
            }
        }
        let limit = num_or(get(a, "limit"), if uses { 30.0 } else { 15.0 });
        let offset = num_or_zero(get(a, "offset"));
        let res = if uses {
            self.call("refs", &[json!(q), json!({"limit": jnum(limit), "offset": jnum(offset)})])
        } else {
            self.call(
                "find",
                &[json!(q), json!({"limit": jnum(limit), "offset": jnum(offset), "kind": if truthy(get(a, "kind")) { get(a, "kind").clone() } else { Value::Null }, "path": scope})],
            )
        };
        let res = match res {
            Ok(r) => r,
            Err(err) => {
                if code_of(&err) == "OKCODE_BAD_PATH" {
                    return Ok(format!("(code_find \"{q}\") — {}", err.message));
                }
                return Err(err);
            }
        };
        let h = arr(&res).unwrap_or(Arr { items: &[], props: &NULL });
        let hp = h.props;
        let library = get(hp, "library");
        let where_ = match &scope {
            Some(s) if !s.is_empty() => format!("{s} in {}", self.id()),
            _ => self.id(),
        };
        if h.items.is_empty() && truthy(library) && truthy(get(library, "missing")) {
            return Ok(format!("(code_find \"{q}\") — NOT FOUND: nothing at {} — not installed, or a wrong path; no hit there means nothing.", sg(library, "path")));
        }
        if h.items.is_empty() {
            return Ok(format!("(code_find \"{q}\") — no {} found in {where_}.", if uses { "uses" } else { "definitions" }));
        }
        let lib = if truthy(library) && truthy(get(library, "missing")) {
            format!("  [NOT FOUND: nothing at {} — not installed, or a wrong path; no hit there means nothing]\n", sg(library, "path"))
        } else if truthy(library) {
            format!(
                "  [library code, looked up by name in a direct parse of {} file(s) — not indexed{}]\n",
                sg(library, "files"),
                if truthy(get(library, "truncated")) { "; PARTIAL: the parse cap stopped the listing, narrow the path" } else { "" }
            )
        } else {
            String::new()
        };
        let at_least = || {
            let x = coalesce(hp, &["atLeast", "total"]);
            if x.is_null() {
                "?".to_string()
            } else {
                js_str(x)
            }
        };
        let continuation = if truthy(get(hp, "truncated")) && !get(hp, "next").is_null() {
            format!(
                "\n  [showing {} of {} — continue with offset={}]",
                h.items.len(),
                if truthy(get(hp, "capped")) { format!("at least {}", at_least()) } else { sg(hp, "total") },
                sg(hp, "next")
            )
        } else if truthy(get(hp, "capped")) {
            format!("\n  [PARTIAL: the scan stopped at {} matches — search a more specific name{}]", at_least(), if uses { "" } else { " or pass a path" })
        } else {
            String::new()
        };
        let rows = h
            .items
            .iter()
            .map(|x| {
                if uses {
                    format!(
                        "  {}:{}  in {}  [{}{}]  — {}",
                        sg(x, "file"),
                        sg(x, "line"),
                        sg(x, "symbol"),
                        sg(x, "label"),
                        if truthy(get(x, "call")) { ", call" } else { "" },
                        sg(x, "text")
                    )
                } else {
                    format!(
                        "  {} {}  {}:{}  ({} lines){}{}",
                        sg(x, "kind"),
                        if truthy(get(x, "path")) { sg(x, "path") } else { sg(x, "name") },
                        sg(x, "file"),
                        sg(x, "lines"),
                        sg(x, "span"),
                        if truthy(get(x, "signature")) { format!("\n      {}", sg(x, "signature")) } else { String::new() },
                        if truthy(get(x, "doc")) { format!("\n      {}", sg(x, "doc")) } else { String::new() }
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        Ok(format!("{} of \"{q}\" in {where_}:\n{lib}{rows}{continuation}", if uses { "Uses" } else { "Definitions" }))
    }

    fn code_read(&self, a: &Value) -> Result<String> {
        let target = self.rel_of(first_truthy(a, &["symbol", "target", "path", "file"]));
        if target.is_empty() {
            return Ok("(code_read) — needs `symbol`: a symbol, a file path, or \"file:from-to\".".into());
        }
        let from = if truthy(get(a, "from")) { jnum(to_number(get(a, "from"))) } else { Value::Null };
        let to = if truthy(get(a, "to")) { jnum(to_number(get(a, "to"))) } else { Value::Null };
        let got = self.call("read", &[json!(target), json!({"from": from, "to": to})])?;
        if !truthy(get(&got, "ok")) {
            let cands = list(&got, "candidates");
            return Ok(format!(
                "(code_read {target}) — {}{}",
                tw(get(&got, "reason")),
                if !cands.is_empty() { format!("\n  candidates: {}", cands.iter().map(js_item).collect::<Vec<_>>().join(", ")) } else { String::new() }
            ));
        }
        let kindpart = if truthy(get(&got, "kind")) {
            format!(" ({} {})", sg(&got, "kind"), if truthy(get(&got, "path")) { sg(&got, "path") } else { sg(&got, "name") })
        } else if truthy(get(&got, "lines")) {
            format!(" of {} lines", sg(&got, "lines"))
        } else {
            String::new()
        };
        Ok(format!(
            "{}:{}-{}{}{}\n{}{}",
            sg(&got, "file"),
            sg(&got, "lineStart"),
            sg(&got, "lineEnd"),
            kindpart,
            if truthy(get(&got, "library")) { "  (library code, read directly — not indexed)".to_string() } else { format!("  at={}", sg(&got, "at")) },
            sg(&got, "body"),
            if truthy(get(&got, "note")) { format!("\n\n  [{}]", sg(&got, "note")) } else { String::new() }
        ))
    }

    fn code_edit(&self, a: &Value) -> Result<String> {
        let has = |k: &str| a.get(k).is_some();
        let single_given = has("symbol") || has("target") || has("body") || has("find");
        let batch_given = get(a, "edits").is_array();
        let mut edits: Vec<Value> = vec![];
        let mut form_error: Option<&str> = None;
        if single_given && batch_given {
            form_error = Some("code_edit received BOTH a single edit and {edits} — use exactly one form, not both");
        } else if !single_given && !batch_given {
            form_error = Some("code_edit needs {file, find, body}, or {symbol, body, at}, or {edits: [...]} — none was given");
        } else if single_given {
            let sym = if has("symbol") { get(a, "symbol") } else { get(a, "target") };
            if !has("body") {
                form_error = Some("an edit needs a body");
            } else if has("find") {
                let mut e = Map::new();
                e.insert("find".into(), json!(js_str(get(a, "find"))));
                e.insert("body".into(), json!(js_str(get(a, "body"))));
                if has("within") {
                    e.insert("within".into(), json!(js_str(get(a, "within"))));
                }
                edits.push(Value::Object(e));
            } else if !has("symbol") && !has("target") {
                form_error = Some("a single edit needs `find` (preferred) or `symbol`");
            } else {
                edits.push(json!({"target": self.rel_of(sym), "body": js_str(get(a, "body"))}));
            }
        } else if get(a, "edits").as_array().map_or(true, |l| l.is_empty()) {
            form_error = Some("edits must be a non-empty array of {find, body} or {target, body}");
        } else {
            for e in get(a, "edits").as_array().unwrap() {
                let mut m = Map::new();
                if e.is_object() && e.get("find").is_some() {
                    m.insert("find".into(), json!(js_str(get(e, "find"))));
                    if e.get("within").is_some() {
                        m.insert("within".into(), json!(js_str(get(e, "within"))));
                    }
                } else {
                    let t = if e.is_object() { first_truthy(e, &["target", "symbol"]) } else { &NULL };
                    m.insert("target".into(), json!(self.rel_of(t)));
                }
                let body = if e.is_object() {
                    let b = get(e, "body");
                    if b.is_null() {
                        String::new()
                    } else {
                        js_str(b)
                    }
                } else if e.is_null() || truthy(e) {
                    String::new()
                } else {
                    js_str(e)
                };
                m.insert("body".into(), json!(body));
                edits.push(Value::Object(m));
            }
        }
        if let Some(fe) = form_error {
            return Ok(format!("(code_edit) REFUSED — {fe}"));
        }

        let mut at: Value = first_truthy(a, &["at", "expected_hash"]).clone();
        if edits.iter().any(|e| e.get("find").is_some()) {
            let fallback = edits.iter().find(|e| get(e, "target").as_str().map_or(false, |t| t.contains('.'))).map(|e| get(e, "target").clone()).unwrap_or(Value::Null);
            let raw = self.rel_of(if truthy(get(a, "file")) {
                get(a, "file")
            } else if truthy(get(a, "path")) {
                get(a, "path")
            } else {
                &fallback
            });
            static TAIL: OnceLock<Regex> = OnceLock::new();
            let tail = TAIL.get_or_init(|| Regex::new(r":[0-9]+(-[0-9]+)?$").unwrap());
            let file = tail.replace(&raw, "").into_owned();
            if file.is_empty() {
                return Ok("(code_edit) REFUSED — a `find` edit needs `file`: the file whose text is being replaced".into());
            }
            match resolve_find_edits(self.ws.as_ref(), &file, &edits)? {
                Resolved::Err { error, at: cur } => {
                    return Ok(format!("(code_edit) REFUSED — {error}{}", if truthy(&cur) { format!("\n  current at={}", js_str(&cur)) } else { String::new() }));
                }
                Resolved::Ok { edits: e2, at: fresh } => {
                    edits = e2;
                    if !truthy(&at) {
                        at = fresh;
                    }
                }
            }
        }

        let res = self.call("editBatch", &[json!({"at": if truthy(&at) { at.clone() } else { Value::Null }, "edits": edits})])?;
        if !truthy(get(&res, "outcome")) {
            let mut lines = vec![format!("(code_edit) REFUSED — {}", tw(get(&res, "reason")))];
            if truthy(get(&res, "stale")) && truthy(get(&res, "at")) {
                lines.push(format!("  the file's current at={}", sg(&res, "at")));
            }
            let cands = list(&res, "candidates");
            if !cands.is_empty() {
                lines.push(format!("  candidates: {}", cands.iter().map(js_item).collect::<Vec<_>>().join(", ")));
            }
            if !list(&res, "newDiagnostics").is_empty() {
                lines.push(format!("  new diagnostics:\n{}", diag_lines(get(&res, "newDiagnostics"), 5)));
            }
            if !truthy(get(&res, "crossFile")) {
                let mut seen: HashSet<String> = HashSet::new();
                for e in edits.iter().take(3) {
                    let t = sg(e, "target");
                    if !seen.insert(t.clone()) {
                        continue;
                    }
                    if let Ok(gr) = self.call("read", &[json!(t)]) {
                        if truthy(&gr) && truthy(get(&gr, "ok")) {
                            let b = sg(&gr, "body");
                            let body = if len16(&b) > REGION_CAP { format!("{}\n…[truncated]", slice16(&b, REGION_CAP)) } else { b };
                            lines.push(format!(
                                "── CURRENT {}:{}-{} at={} (re-read after the refusal — send the corrected edit against THIS at):\n{body}",
                                sg(&gr, "file"),
                                sg(&gr, "lineStart"),
                                sg(&gr, "lineEnd"),
                                sg(&gr, "at")
                            ));
                        }
                    }
                }
            }
            return Ok(lines.join("\n"));
        }
        Ok(render_receipt("code_edit", &res))
    }

    fn code_write(&self, a: &Value) -> Result<String> {
        let p = self.rel_of(first_truthy(a, &["path", "file"]));
        if p.is_empty() {
            return Ok("(code_write) — needs `path`: the workspace-relative file to create or replace.".into());
        }
        if get(a, "content").is_null() {
            return Ok("(code_write) — needs `content`: the complete file text.".into());
        }
        let at = first_truthy(a, &["at", "expected_hash"]).clone();
        let res = self.call("writeWholeFile", &[json!(p), json!(js_str(get(a, "content"))), json!({"expectedHash": at, "create": !truthy(&at)})])?;
        if !truthy(get(&res, "outcome")) {
            let mut lines = vec![format!("(code_write {p}) REFUSED — {}", tw(get(&res, "reason")))];
            if js_str(get(&res, "reason")).contains("already exists") {
                lines.push("  to replace it: code_read the file and pass its `at` here (or use code_edit for part of it).".into());
            }
            if truthy(get(&res, "stale")) && truthy(get(&res, "at")) {
                lines.push(format!("  the file's current at={}", sg(&res, "at")));
            }
            if !list(&res, "newDiagnostics").is_empty() {
                lines.push(format!("  diagnostics:\n{}", diag_lines(get(&res, "newDiagnostics"), 5)));
            }
            return Ok(lines.join("\n"));
        }
        if get(&res, "outcome").as_str() == Some("ok") && !truthy(get(&res, "diff")) {
            let warn = failed_validators(&res);
            return Ok(format!(
                "applied: {} ({}, ok) — validated, published atomically, verified by readback.  at={}{}",
                sg(&res, "file"),
                if truthy(&at) { "replaced" } else { "created" },
                sg(&res, "readbackHash"),
                if !warn.is_empty() { format!("\n  WARNING: {} validator(s) reported issues — not blocking, worth a look.", warn.join(", ")) } else { String::new() }
            ));
        }
        Ok(render_receipt("code_write", &res))
    }
}

/// rg-style rows for one file: the heading and `(text, n)` rows.
fn render_grep_file(file_row: &Value, ms: &[&Value], with_context: bool) -> (String, Vec<(String, Option<i64>)>) {
    struct R {
        text: String,
        hit: bool,
        n: Option<i64>,
    }
    let mut rows: BTreeMap<i64, R> = BTreeMap::new();
    for m in ms {
        let line = ni(get(m, "line"));
        let n = get(m, "n").as_i64();
        let before = list(m, "before");
        for (j, t) in before.iter().enumerate() {
            let k = line - before.len() as i64 + j as i64;
            rows.entry(k).or_insert(R { text: js_str(t), hit: false, n });
        }
        rows.insert(line, R { text: sg(m, "text"), hit: true, n });
        for (j, t) in list(m, "after").iter().enumerate() {
            let k = line + 1 + j as i64;
            rows.entry(k).or_insert(R { text: js_str(t), hit: false, n: None });
        }
    }
    let file = sg(file_row, "file");
    let mut lines = vec![];
    let mut prev: Option<i64> = None;
    for (k, r) in &rows {
        if with_context {
            if let Some(p) = prev {
                if *k > p + 1 {
                    lines.push(("--".to_string(), None));
                }
            }
        }
        lines.push((if r.hit { format!("{file}:{k}: {}", r.text) } else { format!("{file}-{k}- {}", r.text) }, r.n));
        prev = Some(*k);
    }
    let count = ni(get(file_row, "count"));
    let shown = ni(get(file_row, "shown"));
    let mut more = String::new();
    if shown < count {
        let from = ni(get(file_row, "from"));
        let range = if from == 0 {
            String::new()
        } else if shown == 1 {
            format!(" (match {})", from + 1)
        } else {
            format!(" (matches {}-{})", from + 1, from + shown)
        };
        more = format!(", showing {shown}{range}");
        if truthy(get(file_row, "passed")) {
            more += &format!(" — raise max_per_file for the rest (with paths:[\"{file}\"] for this file alone)");
        }
    }
    (format!("── {file} ({count} matching line{}{more}; at={})", if count == 1 { "" } else { "s" }, sg(file_row, "at")), lines)
}

// ── schemas ──────────────────────────────────────────────────────────────────────────────────

/// The definitions minus the `workspace` property and its `required` entry (the model-facing
/// prose is byte-identical to okcode 0.2's).
const SCHEMA_BASE: &str = r####"[{"name":"code_map","description":"Shape of a workspace: files, languages, dependencies, biggest symbols. Start here when you do not yet know a project.","properties":{},"required":[]},{"name":"code_find","description":"Where a symbol or phrase is DEFINED (default) or USED (uses: true): name, kind, file, line range, signature per hit. Searches names, doc comments and file text. Use code_ask when you know what the code does but not its name; code_grep when the exact text must appear.","properties":{"query":{"type":"string","description":"A symbol name or a descriptive phrase"},"uses":{"type":"boolean","description":"true = every use, by enclosing symbol; default = the definition"},"kind":{"type":"string","description":"Only this symbol kind (function, class, method, const, …)"},"path":{"type":"string","description":"Only under this directory or file. A library path (\"node_modules/<pkg>\") looks names up in that package, parsed directly (by name, not by meaning)"},"limit":{"type":"number","description":"Max hits (default 15; 30 for uses)"},"offset":{"type":"number","description":"Resume at this position in the ranked list — use the offset a cut-off answer names"}},"required":["query"]},{"name":"code_grep","description":"Search file CONTENTS like ripgrep: every matching line as `line: text`, grouped by file, with optional context lines. Literal text by default (case-insensitive); regex: true for a JavaScript regular expression; glob/paths to restrict which files. Use this INSTEAD of shelling out to grep/rg/sed: it is faster (index-accelerated, and on a remote workspace it searches where the files live), covers exactly the project (no node_modules, .gitignored or secret files), and every hit is a file:line code_read and code_edit accept. code_find/code_ask match by meaning. To search an INSTALLED LIBRARY, pass its path (paths: [\"node_modules/<pkg>\"], or any folder/file inside node_modules, vendor, dist…): searched directly as it is on disk now, not indexed — never fetch library source from elsewhere. Pick the output for the question: 'lines' (default) = WHERE, the lines themselves; 'files' = which files and how many matching lines each (rg -c); 'matches' = the DISTINCT matched strings with counts (rg -o | sort | uniq -c) — use it to ENUMERATE (\"every OKDB_[A-Z_]+ env var\", \"all event names\"): one call, the complete list. Counts in the heading are always exact; a capped answer ends with a line saying what was not shown and the page/offset that continues it — never report a partial list as complete.","properties":{"pattern":{"type":"string","description":"The text to find (literal), or a JS regex when regex is true. One line."},"regex":{"type":"boolean","description":"Treat pattern as a regular expression (default false)"},"case_sensitive":{"type":"boolean","description":"true = match case exactly; default false = ignore case"},"glob":{"type":"array","items":{"type":"string"},"description":"Only files matching these globs: \"*.js\", \"src/**/*.ts\", \"*.{css,html}\"; \"!pattern\" excludes (\"!*.test.js\", \"!dist\"). A glob without \"/\" matches a name at any depth."},"paths":{"type":"array","items":{"type":"string"},"description":"Only under these workspace-relative directories or files (\"src\", \"lib/util.js\"). A path inside node_modules/vendor/dist (\"node_modules/@scope/pkg\") searches that library directly — the only way to reach it"},"context":{"type":"number","description":"Lines of context before and after each match (default 0)"},"output":{"type":"string","enum":["lines","files","matches"],"description":"'lines' (default): matching lines with text; 'files': files with match counts; 'matches': distinct matched strings with occurrence counts — for enumerating names/values"},"page":{"type":"number","description":"Which page of a capped answer (default 1); the answer says when there is a next one"},"offset":{"type":"number","description":"Resume at this position (a matching line, file or distinct match, 0-based) — use the offset a cut-off answer names"},"max_matches":{"type":"number","description":"Page size: matching lines (default 100), or distinct strings for output:'matches' (default 500)"},"max_per_file":{"type":"number","description":"Max matching lines shown per file per page (default 10)"},"max_files":{"type":"number","description":"Max files per page (default unlimited; 500 for output:'files')"}},"required":["pattern"]},{"name":"code_glob","description":"List workspace files whose PATHS match a glob — \"src/**/*.ts\", \"*.test.js\", \"components\", \"!dist\" — with line counts, from the index (no file is read). Use instead of find/ls in a shell to see what exists before reading.","properties":{"pattern":{"type":"array","items":{"type":"string"},"description":"One or more globs; \"!pattern\" excludes. Without \"/\" a glob matches a name at any depth; a glob matching a directory lists everything under it."},"limit":{"type":"number","description":"Max files listed (default 200)"},"offset":{"type":"number","description":"Resume at this position — use the offset a cut-off answer names"}},"required":["pattern"]},{"name":"code_outline","description":"Split ONE file into named pieces (symbols, structural regions, or chunks) with line ranges and the file's \"at\". Ask before code_read when you do not yet know which part of a file you want. An installed library's folder or file (\"node_modules/<pkg>\", \"node_modules/<pkg>/src/x.js\") is parsed directly, not indexed: a folder lists its files with their top-level names.","properties":{"file":{"type":"string","description":"Workspace-relative path (as the other tools report it); a library folder such as \"node_modules/<pkg>\" too"},"limit":{"type":"number","description":"Max pieces (default 60)"},"offset":{"type":"number","description":"Resume at this piece position — use the offset a cut-off answer names"}},"required":["file"]},{"name":"code_read","description":"Read source, verified against the file as it is now: a symbol (\"store.js#openStore.saveFiles\", or a bare name), a whole file, or a range (\"public/dashboard.html:269-339\"). Every read returns at=<token>: the file's version, which code_edit and code_write require. An installed library file is read by its path (\"node_modules/<pkg>/src/x.js\", a range, or \"<file>#name\") — directly from disk, not indexed, read-only.","properties":{"symbol":{"type":"string","description":"A symbol, a file path, or file:from-to"},"from":{"type":"number","description":"First line of a range"},"to":{"type":"number","description":"Last line"}},"required":["symbol"]},{"name":"code_package","description":"A dependency's surface — version, description, entry, exported names — without reading its source. No name = list every declared dependency.","properties":{"name":{"type":"string","description":"Package name, e.g. \"react\" or \"@scope/pkg\""}},"required":[]},{"name":"code_ask","description":"Ask a workspace a question in your own words and get the symbols and files that answer it, with the region around each hit — when you know what the code DOES but not what it is called. Combines names, file text and semantic (embedding) search; each hit says which one found it.","properties":{"question":{"type":"string"},"embedder":{"type":"string","description":"Embedding profile to query (default: the active one)"},"limit":{"type":"number","description":"Max hits (default 10)"}},"required":["question"]},{"name":"code_edit","description":"Change code in ONE file. PREFERRED form: {file, find, body} — `find` is the EXACT current text being replaced, copied verbatim (it is located uniquely; line numbers are never trusted). Also: {symbol, body, at} for a named symbol or a range (\"file.js:10-20\"), or {edits: [...], at?} for several non-overlapping changes to one file. Symbol/range forms need the \"at\" of the code_read that supplied the body; a stale at is refused (with the current at), never rebased. The candidate is validated before the write; one that adds a new syntax/structure error is refused. Answers applied (with the new at and a diff) or refused (with why, and the current text to retry against).","properties":{"at":{"type":"string","description":"\"at\" from the code_read that supplied the body; stale is refused. Optional with `find` — a fresh read anchors it."},"file":{"type":"string","description":"With `find`: the file to edit"},"find":{"type":"string","description":"The EXACT current text being replaced, verbatim from the file (whitespace included). Include a line or two of surrounding context so a twin of the text elsewhere in the file cannot collide; for repetitive files, add `within` too."},"within":{"type":"string","description":"Disambiguator for repetitive files: a short DISTINCTIVE text from ABOVE the edit site — an enclosing signature, a section header — copied verbatim. It must occur exactly once in the file; if `find` has twins, the first match at/after this landmark is edited. Never a line number."},"symbol":{"type":"string","description":"A symbol (store.js#openStore.saveFiles) or a file range (file.html:919-931)"},"body":{"type":"string","description":"The complete replacement for what find matched / the symbol / the range"},"edits":{"type":"array","description":"Batch form: several edits to one file — each {find, body} or {target, body}","items":{"type":"object","properties":{"find":{"type":"string","description":"Exact current text being replaced (preferred)"},"within":{"type":"string","description":"Unique landmark text above the edit site, verbatim"},"target":{"type":"string","description":"A symbol or a file range"},"body":{"type":"string","description":"Complete replacement"}},"required":["body"]}}},"required":[]},{"name":"code_write","description":"Create a new file, or replace an existing one ENTIRELY. Without `at` it creates (refused if the file exists); with `at` (from a code_read of that file) it replaces, refused if the file changed since. Validated before writing; published atomically. For part of a file prefer code_edit.","properties":{"path":{"type":"string","description":"Workspace-relative path"},"content":{"type":"string","description":"The complete new file text"},"at":{"type":"string","description":"Required to replace an existing file: the at from reading it"}},"required":["path","content"]}]"####;

/// schemas() for a given `workspaces()` list: the JSON-schema definitions, in TOOLS order.
pub fn schemas_for(known: &[Value]) -> Value {
    let many = known.len() != 1;
    let ws_desc = format!(
        "Workspace id, its root path, or any path inside it{}{}",
        if known.is_empty() {
            String::new()
        } else {
            format!(
                " — one of: {}",
                known
                    .iter()
                    .map(|w| format!("{}{}", sg(w, "id"), if truthy(get(w, "description")) { format!(" ({})", sg(w, "description")) } else { String::new() }))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
        if many { "" } else { ". Optional: there is only one." }
    );
    let base: Vec<Value> = serde_json::from_str(SCHEMA_BASE).unwrap_or_default();
    Value::Array(
        base.into_iter()
            .map(|d| {
                let mut props = Map::new();
                props.insert("workspace".into(), json!({"type": "string", "description": ws_desc}));
                if let Some(p) = get(&d, "properties").as_object() {
                    for (k, v) in p {
                        props.insert(k.clone(), v.clone());
                    }
                }
                let mut req: Vec<Value> = if many { vec![json!("workspace")] } else { vec![] };
                req.extend(list(&d, "required").iter().cloned());
                json!({
                    "name": get(&d, "name"),
                    "description": get(&d, "description"),
                    "parameters": {"type": "object", "properties": props, "required": req}
                })
            })
            .collect(),
    )
}

/// tools.js's pure exports for the binding: isTool, isLookup, isEdit, subjectOf, hitWithin,
/// uniqueAnchors.
pub fn call_pure(name: &str, args: &[Value]) -> Result<Value> {
    let a = |i: usize| args.get(i).cloned().unwrap_or(Value::Null);
    let s = |i: usize| match args.get(i) {
        None | Some(Value::Null) => String::new(),
        Some(v) => js_str(v),
    };
    let idx = |i: usize| -> Vec<usize> { a(i).as_array().map(|v| v.iter().filter_map(Value::as_u64).map(|x| x as usize).collect()).unwrap_or_default() };
    match name {
        "isTool" => Ok(Value::Bool(is_tool(&s(0)))),
        "isLookup" => Ok(Value::Bool(is_lookup(&s(0)))),
        "isEdit" => Ok(Value::Bool(is_edit(&s(0)))),
        "subjectOf" => Ok(Value::String(subject_of(&s(0), &a(1)))),
        "hitWithin" => Ok(hit_within(&s(0), &idx(1), &s(2))),
        "uniqueAnchors" => Ok(unique_anchors(&s(0), &s(1), &idx(2))),
        other => Err(Error::msg(format!("tools.js exports no {other}"))),
    }
}
