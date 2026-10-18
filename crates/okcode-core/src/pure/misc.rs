//! pure/misc: the pure exports of lib/grep.js, identity.js, edges.js, embedders.js (see pure.rs).
//! Matcher / path-matcher closures live in the JS wrappers;
//! they call `matcherTest` / `matcherAll` / `pathMatch` here with the matcher's public fields, and
//! the compiled form is cached per thread so a closure is not recompiled per line.
use super::g;
use crate::error::{Error, Result};
use okcode_analysis::embed::embedders as emb;
use okcode_analysis::grep as gr;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const CACHE_CAP: usize = 256;

thread_local! {
    static MATCHERS: RefCell<HashMap<String, Rc<gr::Matcher>>> = RefCell::new(HashMap::new());
    static PATHS: RefCell<HashMap<String, Rc<Option<gr::PathMatcher>>>> = RefCell::new(HashMap::new());
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        _ => true,
    }
}

fn num(v: Option<&Value>) -> Option<usize> {
    v.and_then(|x| x.as_f64()).map(|f| if f < 0.0 { 0 } else { f as usize })
}

fn matcher_of(desc: &Value) -> std::result::Result<Rc<gr::Matcher>, String> {
    let literal = desc.get("literal").and_then(|x| x.as_str()).map(str::to_string);
    let ic = desc.get("ignoreCase").map_or(false, truthy);
    let src = desc.get("source").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let key = format!("{}\u{0}{}\u{0}{}\u{0}{}", literal.is_some(), ic, literal.as_deref().unwrap_or(""), src);
    if let Some(m) = MATCHERS.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(m);
    }
    let m = Rc::new(gr::matcher_from_parts(literal, ic, &src)?);
    MATCHERS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(key, m.clone());
    });
    Ok(m)
}

fn path_matcher_of(opts: &Value) -> std::result::Result<Rc<Option<gr::PathMatcher>>, String> {
    let key = json!([opts.get("glob"), opts.get("paths")]).to_string();
    if let Some(m) = PATHS.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(m);
    }
    // Same list shaping as okcode-grep's dispatched `pathMatcher`; reuse it for the null/non-null decision and
    // rebuild the matcher from the same inputs.
    let globs: Vec<String> = match opts.get("glob") {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => vec![],
    };
    let paths: Vec<String> = match opts.get("paths") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().map(|x| if x.is_null() { String::new() } else { js_str(x) }).collect(),
        Some(o) => vec![js_str(o)],
    };
    let m = Rc::new(gr::path_matcher(&globs, &paths)?);
    PATHS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(key, m.clone());
    });
    Ok(m)
}

fn js_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        o => o.to_string(),
    }
}

fn grep_call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    let null = Value::Null;
    let arg = |i: usize| args.get(i).unwrap_or(&null);
    match name {
        "compileMatcher" => Some(
            okcode_analysis::grep::dispatch("compileMatcher", args)
                .map_err(|m| Error::code("GREP_BAD_PATTERN", m)),
        ),
        // (opts) -> bool: does the path matcher for these opts admit `rel`? (true when nothing filters)
        "pathMatch" => Some(path_matcher_of(arg(0)).map_err(Error::msg).map(|m| match &*m {
            Some(pm) => json!(pm.matches(&js_str(arg(1)))),
            None => json!(true),
        })),
        // (opts) -> bool: is there anything to filter by at all?
        "pathFilters" => Some(path_matcher_of(arg(0)).map_err(Error::msg).map(|m| json!(m.is_some()))),
        "matcherTest" => Some(matcher_of(arg(0)).map_err(Error::msg).map(|m| json!(m.test(&js_str(arg(1)))))),
        "matcherAll" => Some(matcher_of(arg(0)).map_err(Error::msg).map(|m| json!(m.all(&js_str(arg(1)))))),
        "clip" => Some(Ok(json!(gr::clip(&js_str(arg(0)), num(args.get(1)).unwrap_or(0))))),
        // (text, matcher, tally $map, file) -> { count, tally: $map } — the JS wrapper folds the
        // returned tally back into the caller's Map (a native call cannot mutate it).
        "tallyText" => Some((|| {
            let m = matcher_of(arg(1)).map_err(Error::msg)?;
            let mut tally: Vec<(String, gr::Tally)> = vec![];
            if let Some(Value::Array(entries)) = arg(2).get("$map") {
                for e in entries {
                    if let (Some(k), Some(t)) = (e.get(0).and_then(|k| k.as_str()), e.get(1)) {
                        tally.push((
                            k.to_string(),
                            gr::Tally {
                                count: num(t.get("count")).unwrap_or(0),
                                lines: num(t.get("lines")).unwrap_or(0),
                                files: num(t.get("files")).unwrap_or(0),
                                first: t.get("first").and_then(|x| x.as_str()).map(str::to_string),
                                last_file: t.get("lastFile").and_then(|x| x.as_str()).map(str::to_string),
                            },
                        ));
                    }
                }
            }
            let count = gr::tally_text(&js_str(arg(0)), &m, &mut tally, arg(3).as_str());
            let out: Vec<Value> = tally
                .into_iter()
                .map(|(k, t)| {
                    json!([k, { "count": t.count, "lines": t.lines, "files": t.files, "first": t.first, "lastFile": t.last_file }])
                })
                .collect();
            Ok(json!({ "count": count, "tally": { "$map": out } }))
        })()),
        "globToRegExp" | "matchText" | "indexTerms" | "splitList" => g(gr::dispatch(name, args)),
        _ => None,
    }
}

fn identity_call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    match name {
        "endpointOf" | "partsOf" | "identityOf" | "shortHash" | "pipelineName" | "isOurs" | "findPipeline" => {
            g(okcode_analysis::embed::dispatch(name, args))
        }
        _ => None,
    }
}

fn translated(r: std::result::Result<emb::Translated, String>) -> Option<Result<Value>> {
    Some(r.map(|t| json!({ "profile": t.profile, "record": t.record })).map_err(|m| {
        if m.contains("needs an `id`") {
            Error::code("OKCODE_NEEDS_ID", m)
        } else {
            Error::msg(m)
        }
    }))
}

fn embedders_call(name: &str, args: &[Value]) -> Option<Result<Value>> {
    let null = Value::Null;
    let arg = |i: usize| args.get(i).unwrap_or(&null);
    match name {
        "derivedType" => Some(Ok(json!(emb::derived_type(&js_str(arg(0)))))),
        "partsOfRecord" => Some(Ok(emb::parts_of_record(arg(0)).map_or(Value::Null, |p| p.to_json()))),
        "describe" => Some(Ok(emb::describe(arg(0), arg(1)))),
        "fromRecord" => Some(Ok(emb::from_record(arg(0)).unwrap_or(Value::Null))),
        "modelOf" => Some(Ok(emb::model_of(arg(0)).map_or(Value::Null, |s| json!(s)))),
        // (name, cfg without its functions, keyFn: bool) / (name, cfg without its functions)
        "fromHost" => translated(emb::from_host(&js_str(arg(0)), arg(1), truthy(arg(2)))),
        "fromHostCustom" => translated(emb::from_host_custom(&js_str(arg(0)), arg(1))),
        _ => None,
    }
}

/// None = not handled here (the router reports OKCODE_NOT_IMPLEMENTED).
pub fn call(module: &str, name: &str, args: &[Value]) -> Option<Result<Value>> {
    match module {
        "grep" => grep_call(name, args),
        "identity" => identity_call(name, args),
        "embedders" => embedders_call(name, args),
        "edges" => match name {
            "stripJsonc" => Some(Ok(json!(okcode_analysis::edges::edges::strip_jsonc(&js_str(args.first().unwrap_or(&Value::Null)))))),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(r: Option<Result<Value>>) -> Value {
        r.unwrap().unwrap()
    }

    #[test]
    fn matcher_closure_roundtrip() {
        let d = ok(call("grep", "compileMatcher", &[json!("Foo"), json!({"caseSensitive": "smart"})]));
        assert_eq!(ok(call("grep", "matcherTest", &[d.clone(), json!("a Foo b")])), json!(2));
        assert_eq!(ok(call("grep", "matcherTest", &[d, json!("a foo b")])), json!(-1));
        let e = call("grep", "compileMatcher", &[json!(""), json!({})]).unwrap().unwrap_err();
        assert_eq!(e.code.as_deref(), Some("GREP_BAD_PATTERN"));
    }

    #[test]
    fn path_match_and_tally() {
        let o = json!({"glob": ["*.js", "!test"], "paths": null});
        assert_eq!(ok(call("grep", "pathMatch", &[o.clone(), json!("src/a.js")])), json!(true));
        assert_eq!(ok(call("grep", "pathMatch", &[o, json!("test/a.js")])), json!(false));
        assert_eq!(ok(call("grep", "pathFilters", &[json!({})])), json!(false));
        let d = ok(call("grep", "compileMatcher", &[json!("a+"), json!({"regex": true})]));
        let r = ok(call("grep", "tallyText", &[json!("aa a\nb"), d, json!({"$map": []}), json!("f")]));
        assert_eq!(r["count"], json!(1));
        assert_eq!(r["tally"]["$map"][0][0], json!("aa"));
    }

    #[test]
    fn identity_and_embedders() {
        assert_eq!(ok(call("embedders", "derivedType", &[json!("My Prof")])), json!("okcode-my_prof"));
        let r = ok(call("embedders", "fromHost", &[json!("p"), json!({"type": "ollama", "model": "m", "dims": 3}), json!(false)]));
        assert_eq!(r["profile"]["identity"]["model"], json!("m"));
        let e = call("embedders", "fromHostCustom", &[json!("p"), json!({})]).unwrap().unwrap_err();
        assert_eq!(e.code.as_deref(), Some("OKCODE_NEEDS_ID"));
    }
}
