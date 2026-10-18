//! Look-up commands: structure, find, refs, importers, grep, glob, outline, ask, packages, read, read-at.
//! Plus the helpers edit.rs shares: the workspace context (`Ws`), flag values as the JS passes
//! them, paged-array access and JS text formatting.

use crate::args::{Args, Flag};
use crate::open::truthy;
use crate::render::{err, j, js_str, out, pad, pad_str};
use crate::{need, Exit};
use okcode_core::host::{ToolHost, ToolWorkspace};
use okcode_core::tools::{ToolOptions, Tools};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub type R = Result<(), Exit>;

/// The workspace a command runs against: the okcode tool ws (its `ask` follows the active
/// profile, as the okcode ws the JS CLI holds) plus the root for the tool layer.
pub struct Ws<'a> {
    pub tw: Arc<dyn ToolWorkspace>,
    pub root: String,
    pub a: &'a Args,
}

impl Ws<'_> {
    pub fn call(&self, method: &str, args: Vec<Value>) -> Result<Value, Exit> {
        self.tw.call(method, &args).map_err(Exit::from_core)
    }
    pub fn json(&self) -> bool {
        self.a.on("json")
    }
}

/// createTools({ workspace: async () => ws, workspaces: () => [{ id: ws.id, root }] })
struct CliHost {
    tw: Arc<dyn ToolWorkspace>,
    root: String,
}

impl ToolHost for CliHost {
    fn workspace(&self, _hint: Option<&str>) -> okcode_core::Result<Option<Arc<dyn ToolWorkspace>>> {
        Ok(Some(self.tw.clone()))
    }
    fn workspaces(&self) -> Vec<Value> {
        vec![json!({ "id": self.tw.id(), "root": self.root })]
    }
}

/// lookupVia: --json prints the workspace call's result; otherwise the tool's rendering, and a
/// tool failure (text starting "(<tool>") exits 1.
fn lookup_via(w: &Ws, tool: &str, args: Value, json_fn: impl FnOnce() -> Result<Value, Exit>) -> R {
    if w.json() {
        j(&plain(&json_fn()?));
        return Ok(());
    }
    let tools = Tools::new(Arc::new(CliHost { tw: w.tw.clone(), root: w.root.clone() }), ToolOptions::default());
    let text = tools.render(tool, &args);
    out(&text);
    if text.starts_with(&format!("({tool}")) {
        return Err(Exit { code: 1, message: String::new() });
    }
    Ok(())
}

// ── values as the JS sees them ──────────────────────────────────────────

/// flags[k] as the JS value: a bare flag is `true`, otherwise the string; None = undefined.
pub fn fv(a: &Args, k: &str) -> Option<Value> {
    a.flags.get(k).map(|f| match f {
        Flag::On => Value::Bool(true),
        Flag::Val(s) => Value::String(s.clone()),
    })
}

/// An object literal whose undefined members are dropped (what reaches the native side).
pub fn obj(entries: Vec<(&str, Option<Value>)>) -> Value {
    let mut m = Map::new();
    for (k, v) in entries {
        if let Some(v) = v {
            m.insert(k.to_string(), v);
        }
    }
    Value::Object(m)
}

/// Number(s) for a CLI string (a bare flag is Number(true) = 1); None = undefined → NaN.
fn number(f: Option<&Flag>) -> f64 {
    match f {
        None => f64::NAN,
        Some(Flag::On) => 1.0,
        Some(Flag::Val(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                u64::from_str_radix(h, 16).map(|n| n as f64).unwrap_or(f64::NAN)
            } else if t.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E')) {
                t.parse().unwrap_or(f64::NAN)
            } else if t == "Infinity" || t == "+Infinity" {
                f64::INFINITY
            } else if t == "-Infinity" {
                f64::NEG_INFINITY
            } else {
                f64::NAN
            }
        }
    }
}

/// `Number(flags[k]) || d`
pub fn num_or(a: &Args, k: &str, d: f64) -> Value {
    let n = number(a.flags.get(k));
    jnum(if n == 0.0 || n.is_nan() { d } else { n })
}

/// Number(s) of a positional (undefined → NaN, which reaches the native side as null)
fn num_pos(s: Option<&String>) -> Value {
    let n = number(s.map(|s| Flag::Val(s.clone())).as_ref());
    if n.is_nan() { Value::Null } else { jnum(n) }
}

fn jnum(f: f64) -> Value {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        json!(f as i64)
    } else {
        serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
    }
}

/// A paged result's items: `{"$arr": [...], "$props": {...}}` or a plain array.
pub fn items(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("$arr").and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// A paged result's extra property (`hits.total`, `hits.next`, …), else the object's own key.
pub fn prop<'v>(v: &'v Value, k: &str) -> &'v Value {
    static NULL: Value = Value::Null;
    match v {
        Value::Object(o) if o.contains_key("$arr") => o.get("$props").and_then(|p| p.get(k)).unwrap_or(&NULL),
        Value::Object(o) => o.get(k).unwrap_or(&NULL),
        _ => &NULL,
    }
}

/// What JSON.stringify prints for a result: a paged array is just its items (array properties
/// are not serialized), recursively.
pub fn plain(v: &Value) -> Value {
    match v {
        Value::Object(o) if o.contains_key("$arr") && o.keys().all(|k| k == "$arr" || k == "$props") => {
            Value::Array(items(v).iter().map(plain).collect())
        }
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), plain(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(plain).collect()),
        other => other.clone(),
    }
}

/// `${o.k}` — a missing key prints 'undefined'
pub fn u(o: &Value, k: &str) -> String {
    o.get(k).map(js_str).unwrap_or_else(|| "undefined".into())
}

/// `${v}` of a value read through `prop` / `??` chains (null prints 'null')
fn s(v: &Value) -> String {
    js_str(v)
}

/// `a || b` for display
fn or<'v>(a: &'v Value, b: &'v Value) -> &'v Value {
    if truthy(a) { a } else { b }
}

/// str.slice(0, n) in UTF-16 units
fn slice16(t: &str, n: usize) -> String {
    let u: Vec<u16> = t.encode_utf16().take(n).collect();
    String::from_utf16_lossy(&u)
}

/// Number#toLocaleString() for en-US: thousands separators, at most 3 decimals.
fn locale(v: &Value) -> String {
    let Some(f) = v.as_f64() else { return s(v) };
    if !f.is_finite() {
        return if f.is_nan() { "NaN".into() } else if f > 0.0 { "∞".into() } else { "-∞".into() };
    }
    let r = (f * 1000.0).round() / 1000.0;
    let neg = r < 0.0;
    let txt = format!("{}", r.abs());
    let (int, frac) = txt.split_once('.').map(|(i, f)| (i.to_string(), Some(f.to_string()))).unwrap_or((txt.clone(), None));
    let mut g = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            g.push(',');
        }
        g.push(c);
    }
    format!("{}{g}{}", if neg { "-" } else { "" }, frac.map(|f| format!(".{f}")).unwrap_or_default())
}

// ── commands ────────────────────────────────────────────────────────────

pub fn structure(w: &Ws) -> R {
    let st = w.call("structure", vec![json!({ "dir": fv(w.a, "dir").filter(truthy).unwrap_or(Value::Null) })])?;
    if w.json() {
        j(&plain(&st));
        return Ok(());
    }
    out(&format!("\n{} ({})", u(&st, "id"), w.root));
    out(&format!("  {} files · {} lines · {} symbols\n", u(&st, "files"), locale(&st["lines"]), u(&st, "symbols")));
    out("  DIRECTORY                          FILES   LINES  SYMBOLS");
    for d in items(&st["dirs"]) {
        out(&format!("  {} {} {} {}", pad(&d["dir"], 32), pad(&d["files"], 7), pad_str(&locale(&d["lines"]), 7), u(&d, "symbols")));
    }
    out("\n  LARGEST SYMBOLS (the shape a file listing hides)");
    for l in items(&st["largest"]) {
        out(&format!("  {} {} {}", pad_str(&format!("{} lines", u(&l, "lines")), 12), pad(&l["name"], 22), u(&l, "file")));
    }
    let unparsed = items(&st["unparsed"]);
    if !unparsed.is_empty() {
        out("\n  UNPARSED");
        for x in unparsed {
            out(&format!("  {} {}", pad(&x["file"], 34), u(&x, "reason")));
        }
    }
    out("");
    Ok(())
}

/// The paging footer find and refs print: `[showing N of M — continue with --offset K]`.
fn footer(hits: &Value, n: usize, at_least_key: &str) {
    if truthy(prop(hits, "truncated")) && !prop(hits, "next").is_null() {
        let of = if truthy(prop(hits, "capped")) {
            let x = prop(hits, at_least_key);
            format!("at least {}", if x.is_null() { "?".into() } else { s(x) })
        } else {
            s(prop(hits, "total"))
        };
        out(&format!("  [showing {n} of {of} — continue with --offset {}]", s(prop(hits, "next"))));
        out("");
    }
}

pub fn find(w: &Ws) -> R {
    let q = need(w.a.positional.first().cloned(), "find needs a query")?;
    let opts = json!({ "kind": fv(w.a, "kind").filter(truthy).unwrap_or(Value::Null), "limit": num_or(w.a, "limit", 20.0), "offset": num_or(w.a, "offset", 0.0) });
    let hits = w.call("find", vec![json!(q), opts])?;
    if w.json() {
        j(&plain(&hits));
        return Ok(());
    }
    let list = items(&hits);
    if list.is_empty() {
        out(&format!("no match for \"{q}\""));
        return Ok(());
    }
    out("");
    for h in &list {
        out(&format!(
            "  {} {} {} {} lines{}",
            pad(&h["kind"], 9),
            pad(or(&h["path"], &h["name"]), 30),
            pad_str(&format!("{}:{}", u(h, "file"), u(h, "lines")), 42),
            u(h, "span"),
            if truthy(&h["exported"]) { "  [exported]" } else { "" }
        ));
        if truthy(&h["signature"]) {
            out(&format!("  {} {}", " ".repeat(9), s(&h["signature"])));
        }
        if truthy(&h["doc"]) {
            out(&format!("  {} {}", " ".repeat(9), slice16(&s(&h["doc"]), 120)));
        }
        out("");
    }
    // find's capped form names `total` (refs names `atLeast`)
    footer(&hits, list.len(), "total");
    Ok(())
}

pub fn refs(w: &Ws) -> R {
    let q = need(w.a.positional.first().cloned(), "refs needs a query")?;
    let hits = w.call("refs", vec![json!(q), json!({ "limit": num_or(w.a, "limit", 40.0), "offset": num_or(w.a, "offset", 0.0) })])?;
    if w.json() {
        j(&plain(&hits));
        return Ok(());
    }
    let list = items(&hits);
    if list.is_empty() {
        out(&format!("no uses of \"{q}\""));
        return Ok(());
    }
    // new Map(): files in first-seen order
    let mut by_file: Vec<(String, Vec<&Value>)> = Vec::new();
    for h in &list {
        let f = u(h, "file");
        match by_file.iter_mut().find(|(k, _)| *k == f) {
            Some((_, v)) => v.push(h),
            None => by_file.push((f, vec![h])),
        }
    }
    out(&format!("\n{} use(s) of \"{q}\" (ranked: local, imported, textual; [call] = an actual call site)\n", list.len()));
    for (file, hs) in by_file {
        out(&format!("  {file}"));
        for h in hs {
            let tag = format!("{}{}", u(h, "label"), if truthy(&h["call"]) { ", call" } else { "" });
            out(&format!("    {} {} [{tag}]  {}", pad(&h["line"], 6), pad_str(&format!("in {}", u(h, "symbol")), 26), u(h, "text")));
        }
        out("");
    }
    footer(&hits, list.len(), "atLeast");
    Ok(())
}

pub fn importers(w: &Ws) -> R {
    let file = need(w.a.positional.first().cloned(), "importers needs <file>")?;
    let r = w.call("importers", vec![json!(file), json!({ "limit": num_or(w.a, "limit", 40.0), "offset": num_or(w.a, "offset", 0.0) })])?;
    if w.json() {
        j(&plain(&r));
        return Ok(());
    }
    if !truthy(&r["ok"]) {
        out(&u(&r, "reason"));
        return Err(Exit { code: 1, message: String::new() });
    }
    let list = items(&r["importers"]);
    if list.is_empty() {
        out(&format!("nothing in {} imports it (resolved edges only — see refs for a text scan)", u(&r, "file")));
        return Ok(());
    }
    let more = if truthy(&r["truncated"]) {
        format!(" (showing {} from offset {} — continue with --offset {})", list.len(), u(&r, "offset"), u(&r, "next"))
    } else {
        String::new()
    };
    out(&format!("\n{} importer(s) of {}{more}\n", u(&r, "total"), u(&r, "file")));
    for i in &list {
        let kind = if truthy(&i["kind"]) { format!("  {}", s(&i["kind"])) } else { String::new() };
        let via = if truthy(&i["via"]) { format!("  (via {})", s(&i["via"])) } else { String::new() };
        let names = if truthy(&i["names"]) {
            format!("  {}", items(&i["names"]).iter().map(|n| if n["local"].is_null() { String::new() } else { s(&n["local"]) }).collect::<Vec<_>>().join(", "))
        } else {
            String::new()
        };
        out(&format!("  {}:{}{kind}{via}{names}", u(i, "file"), u(i, "line")));
    }
    Ok(())
}

fn split_list(v: Value) -> Value {
    match okcode_core::analysis::grep::split_list(&v) {
        Some(l) => json!(l),
        None => Value::Null,
    }
}

pub fn grep(w: &Ws) -> R {
    let a = w.a;
    let pattern = need(a.positional.first().cloned(), "grep needs a pattern")?;
    let kase = if a.on("case-sensitive") { json!(true) } else if a.on("smart-case") { json!("smart") } else { json!(false) };
    let glob = split_list(fv(a, "glob").unwrap_or(Value::Null));
    let paths = split_list(fv(a, "path").unwrap_or(Value::Null));
    let args = obj(vec![
        ("pattern", Some(json!(pattern))),
        ("regex", Some(json!(a.on("regex")))),
        ("case_sensitive", Some(kase.clone())),
        ("glob", Some(glob.clone())),
        ("paths", Some(paths.clone())),
        ("context", fv(a, "context")),
        ("before", fv(a, "before")),
        ("after", fv(a, "after")),
        ("max_matches", fv(a, "max")),
        ("max_per_file", fv(a, "max-per-file")),
        ("limit", fv(a, "limit")),
        ("output", fv(a, "output")),
        ("page", fv(a, "page")),
        ("offset", fv(a, "offset")),
    ]);
    lookup_via(w, "code_grep", args, || {
        let opts = obj(vec![
            ("regex", Some(json!(a.on("regex")))),
            ("caseSensitive", Some(kase)),
            ("glob", Some(glob)),
            ("paths", Some(paths)),
            ("context", fv(a, "context")),
            ("before", fv(a, "before")),
            ("after", fv(a, "after")),
            ("maxMatches", fv(a, "max")),
            ("maxPerFile", fv(a, "max-per-file")),
            ("maxFiles", fv(a, "limit")),
            ("output", fv(a, "output")),
            ("page", fv(a, "page")),
            ("offset", fv(a, "offset")),
        ]);
        w.call("grep", vec![json!(pattern), opts])
    })
}

pub fn glob(w: &Ws) -> R {
    let a = w.a;
    let pats = split_list(json!(a.positional));
    if pats.is_null() {
        return Err(Exit::fail("glob needs a pattern".into()));
    }
    let args = obj(vec![("pattern", Some(pats.clone())), ("limit", fv(a, "limit")), ("offset", fv(a, "offset"))]);
    lookup_via(w, "code_glob", args, || {
        w.call("glob", vec![pats, json!({ "limit": num_or(a, "limit", 0.0), "offset": num_or(a, "offset", 0.0) })])
    })
}

pub fn outline(w: &Ws) -> R {
    let a = w.a;
    let file = need(a.positional.first().cloned(), "outline needs <file>")?;
    let args = obj(vec![("file", Some(json!(file))), ("limit", fv(a, "limit")), ("offset", fv(a, "offset"))]);
    lookup_via(w, "code_outline", args, || {
        w.call("outline", vec![json!(file), json!({ "limit": num_or(a, "limit", 60.0), "offset": num_or(a, "offset", 0.0) })])
    })
}

pub fn ask(w: &Ws) -> R {
    let a = w.a;
    let q = need(Some(a.positional.join(" ")), "ask needs a question")?;
    let profile = match a.flags.get("embedder") {
        Some(Flag::Val(s)) if !s.is_empty() => json!(s),
        _ => Value::Null,
    };
    let args = obj(vec![("question", Some(json!(q))), ("embedder", Some(profile.clone())), ("limit", fv(a, "limit"))]);
    lookup_via(w, "code_ask", args, || {
        w.call("ask", vec![json!(q), json!({ "limit": num_or(a, "limit", 10.0), "profile": profile, "embedder": profile })])
    })
}

pub fn packages(w: &Ws) -> R {
    if let Some(name) = w.a.positional.first() {
        let p = w.call("package", vec![json!(name)])?;
        if w.json() {
            j(&plain(&p));
            return Ok(());
        }
        if truthy(&p["error"]) {
            return Err(Exit::fail(s(&p["error"])));
        }
        let ver = if truthy(&p["version"]) { s(&p["version"]) } else { "?".into() };
        out(&format!("\n{}@{ver}  ({})", u(&p, "name"), u(&p, "source")));
        if truthy(&p["description"]) {
            out(&format!("  {}", s(&p["description"])));
        }
        let main = if truthy(&p["main"]) { s(&p["main"]) } else { "(none)".into() };
        let types = if truthy(&p["types"]) { format!("   types: {}", s(&p["types"])) } else { String::new() };
        out(&format!("  entry: {main}{types}"));
        let keys = items(&p["exportKeys"]);
        if !keys.is_empty() {
            out(&format!("  exports map: {}", join(&keys, ", ")));
        }
        let from = if truthy(&p["surfaceFrom"]) { s(&p["surfaceFrom"]) } else { "not found".into() };
        let sfile = if truthy(&p["surfaceFile"]) { format!(" · {}", s(&p["surfaceFile"])) } else { String::new() };
        out(&format!("\n  SURFACE ({from}{sfile})"));
        let surface = items(&p["surface"]);
        let joined = join(&surface, ", ");
        let partial = if truthy(&p["surfaceTruncated"]) { format!("\n  [PARTIAL: cut at {} names]", surface.len()) } else { String::new() };
        out(&format!("  {}{partial}", if joined.is_empty() { "(none)".into() } else { joined }));
        let members = items(&p["members"]);
        if !members.is_empty() {
            out(&format!("\n  MEMBERS ({}{})", members.len(), if truthy(&p["membersTruncated"]) { ", PARTIAL — cut here" } else { "" }));
            out(&format!("  {}", join(&members, ", ")));
        }
        out("");
        return Ok(());
    }
    let list = w.call("packages", vec![json!({ "refresh": w.a.on("refresh") })])?;
    if w.json() {
        j(&plain(&list));
        return Ok(());
    }
    if truthy(prop(&list, "error")) {
        return Err(Exit::fail(s(prop(&list, "error"))));
    }
    let rows = items(&list);
    out(&format!("\n{} declared dependencies\n", rows.len()));
    out("  PACKAGE                          VERSION       SOURCE       SURFACE  MEMBERS");
    for p in &rows {
        out(&format!(
            "  {} {} {} {} {}",
            pad(&p["name"], 32),
            pad(&p["version"], 13),
            pad(&p["source"], 12),
            pad(&json!(items(&p["surface"]).len()), 8),
            items(&p["members"]).len()
        ));
    }
    out("");
    Ok(())
}

/// Array#join: null members join as ''
fn join(v: &[Value], sep: &str) -> String {
    v.iter().map(|x| if x.is_null() { String::new() } else { s(x) }).collect::<Vec<_>>().join(sep)
}

/// The FULL token, not a truncated preview: `--at` is mandatory for every existing-file edit,
/// so what is printed here must be usable verbatim on the next command line.
pub fn read(w: &Ws) -> R {
    let q = need(w.a.positional.first().cloned(), "read needs a symbol, file, or file:a-b")?;
    let r = w.call("read", vec![json!(q)])?;
    if w.json() {
        j(&plain(&r));
        return if truthy(&r["ok"]) { Ok(()) } else { Err(Exit { code: 1, message: String::new() }) };
    }
    if !truthy(&r["ok"]) {
        err(&u(&r, "reason"));
        for c in items(&r["candidates"]) {
            err(&format!("  {}", s(&c)));
        }
        return Err(Exit { code: 1, message: String::new() });
    }
    let what = if truthy(&r["kind"]) {
        format!("  ({} {})", s(&r["kind"]), s(or(&r["path"], &r["name"])))
    } else if truthy(&r["lines"]) {
        format!("  of {} lines", s(&r["lines"]))
    } else {
        String::new()
    };
    err(&format!("# {}:{}-{}{what}", u(&r, "file"), u(&r, "lineStart"), u(&r, "lineEnd")));
    err(&format!("# at={}", u(&r, "at")));
    if truthy(&r["note"]) {
        err(&format!("# {}", s(&r["note"])));
    }
    out(&u(&r, "body"));
    Ok(())
}

pub fn read_at(w: &Ws) -> R {
    let a = w.a;
    let file = need(a.positional.first().cloned(), "read-at needs <file> <a> <b>")?;
    let from = num_pos(a.positional.get(1));
    // Number(positional[2] || positional[1])
    let to = num_pos(a.positional.get(2).filter(|x| !x.is_empty()).or(a.positional.get(1)));
    let at = fv(a, "at").filter(truthy).unwrap_or(Value::Null);
    let r = w.call("readAt", vec![json!(file), from, to, at])?;
    if w.json() {
        j(&plain(&r));
        return if truthy(&r["ok"]) { Ok(()) } else { Err(Exit { code: 1, message: String::new() }) };
    }
    if !truthy(&r["ok"]) {
        err(&u(&r, "reason"));
        if truthy(&r["at"]) {
            err(&format!("# current at={}", s(&r["at"])));
        }
        return Err(Exit { code: 1, message: String::new() });
    }
    err(&format!("# {}:{}-{}", u(&r, "file"), u(&r, "lineStart"), u(&r, "lineEnd")));
    err(&format!("# at={}", u(&r, "at")));
    out(&u(&r, "body"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_like_en_us() {
        assert_eq!(locale(&json!(1234567)), "1,234,567");
        assert_eq!(locale(&json!(999)), "999");
        assert_eq!(locale(&json!(1234.5678)), "1,234.568");
    }

    #[test]
    fn plain_drops_array_props() {
        let v = json!({ "$arr": [1, { "$arr": [2], "$props": { "x": 1 } }], "$props": { "total": 9 } });
        assert_eq!(plain(&v), json!([1, [2]]));
        assert_eq!(prop(&v, "total"), &json!(9));
    }
}
