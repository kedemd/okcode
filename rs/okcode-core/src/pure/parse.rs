//! pure/parse: the pure exports of analysis/parse, analysis/ext-okjs and analysis/packages (see
//! pure.rs). Slice-owned (rs/tools/slices.conf). extensions.js and index.js are JS-only (the
//! registry holds user functions; index re-exports).
use crate::error::{Error, Result};
use okcode_analysis as an;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashMap;

fn s(args: &[Value], i: usize) -> Result<&str> {
    args.get(i).and_then(Value::as_str).ok_or_else(|| Error::msg(format!("argument {i} must be a string")))
}
fn opt_s(args: &[Value], i: usize) -> Option<&str> {
    args.get(i).and_then(Value::as_str)
}

/// None = not handled here (the router reports OKCODE_NOT_IMPLEMENTED).
pub fn call(module: &str, name: &str, args: &[Value]) -> Option<Result<Value>> {
    match module {
        "analysis/parse" => parse(name, args),
        "analysis/ext-okjs" => ext_okjs(name, args),
        "analysis/packages" => packages(name, args),
        _ => None,
    }
}

fn parse(name: &str, a: &[Value]) -> Option<Result<Value>> {
    Some((|| -> Result<Value> {
        Ok(match name {
            // the whole dispatcher, registry-blind (the JS wrapper consults its registry first)
            "extract" => an::extract(s(a, 0)?, opt_s(a, 1)),
            // extract() minus the extension step: tree-sitter, JavaScript, no-extractor record
            "extractGeneric" => an::parse::extract_generic(s(a, 0)?, opt_s(a, 1)),
            "langOf" => json!(an::lang_of(s(a, 0)?)),
            "extOf" => json!(an::ext_of(s(a, 0)?)),
            "isSecret" => json!(an::is_secret(s(a, 0)?)),
            "isTextual" => {
                let size = a.get(1).and_then(Value::as_f64).filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64);
                json!(an::is_textual(s(a, 0)?, size))
            }
            "analyzerVersionFor" => an::analyzer_version_for(s(a, 0)?).map_or(Value::Null, Value::from),
            "extractJavaScript" => an::js::parse::extract_javascript_source(s(a, 1)?).map_err(Error::msg)?,
            // an acorn AST cannot cross; callers get the verdict (a throw on a syntax error) and the goal
            "parseSource" => {
                let kind = an::js::parse::check_parse_source(s(a, 0)?).map_err(Error::msg)?;
                json!({"type": "Program", "sourceType": kind, "body": []})
            }
            _ => return Err(Error::todo(&format!("analysis/parse.{name}"))),
        })
    })())
}

thread_local! {
    static OKJS: RefCell<Option<an::okjs::Analyzer>> = const { RefCell::new(None) };
}

fn ext_okjs(name: &str, a: &[Value]) -> Option<Result<Value>> {
    Some((|| -> Result<Value> {
        Ok(match name {
            "toolingVersion" => json!(an::okjs::tooling_version()),
            "baseURLFor" => json!(an::okjs::base_url_for(s(a, 0)?)),
            "matches" => json!(an::okjs::matches(s(a, 0)?)),
            // the embedded analyser end to end: the normalised envelope (symbols nested, regions, okAnalysis)
            "analyze" => {
                let (path, src) = (s(a, 0)?, s(a, 1)?);
                let v = an::parse::okcode_okjs_with_generic(path, src);
                if v.is_null() {
                    return Err(Error::msg("okjs analyser unavailable"));
                }
                v
            }
            // an analysis produced elsewhere (injected tooling), projected to the same envelope
            "fromAnalysis" => an::parse::okjs_from_analysis(s(a, 0)?, s(a, 1)?, a.get(2).cloned().unwrap_or(Value::Null)),
            _ => return Err(Error::todo(&format!("analysis/ext-okjs.{name}"))),
        })
    })())
}

// ── packages ──────────────────────────────────────────────────────────────

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Reads answered from what the JS side has already fetched. The first `read` with a path the cache
/// lacks records the whole batch as `need` (exactly the batch the JS would have asked for) and
/// answers nothing, so the caller is replayed once the JS has awaited the real facade.
struct Replay {
    cache: HashMap<String, Option<Vec<u8>>>,
    need: RefCell<Option<Vec<String>>>,
}
impl an::edges::packages::Access for Replay {
    fn read(&self, paths: &[String]) -> HashMap<String, Vec<u8>> {
        if self.need.borrow().is_some() {
            return HashMap::new();
        }
        if paths.iter().any(|p| !self.cache.contains_key(p)) {
            *self.need.borrow_mut() = Some(paths.to_vec());
            return HashMap::new();
        }
        paths.iter().filter_map(|p| self.cache.get(p).and_then(|b| b.clone()).map(|b| (p.clone(), b))).collect()
    }
}

fn replay(cache: Option<&Value>) -> Replay {
    let mut m = HashMap::new();
    for e in cache.and_then(Value::as_array).into_iter().flatten() {
        if let Some(p) = e.get(0).and_then(Value::as_str) {
            m.insert(p.to_string(), e.get(1).and_then(Value::as_str).map(|t| t.as_bytes().to_vec()));
        }
    }
    Replay { cache: m, need: RefCell::new(None) }
}

fn extract_exports(path: &str, text: &str) -> Vec<String> {
    str_list(an::extract(path, Some(text)).get("exports"))
}

fn package_json(p: &an::edges::packages::Package) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), json!(p.name));
    m.insert("version".into(), json!(p.version));
    m.insert("description".into(), json!(p.description));
    m.insert("main".into(), json!(p.main));
    m.insert("module".into(), json!(p.module));
    m.insert("types".into(), json!(p.types));
    m.insert("exportKeys".into(), json!(p.export_keys));
    m.insert("bin".into(), json!(p.bin));
    m.insert("source".into(), json!(p.source));
    m.insert("surface".into(), json!(p.surface));
    m.insert("surfaceFrom".into(), json!(p.surface_from));
    m.insert("members".into(), json!(p.members));
    if let Some(f) = &p.surface_file {
        m.insert("surfaceFile".into(), json!(f));
    }
    if p.surface_truncated {
        m.insert("surfaceTruncated".into(), json!(true));
    }
    if p.members_truncated {
        m.insert("membersTruncated".into(), json!(true));
    }
    Value::Object(m)
}

fn packages_map(ps: &[an::edges::packages::Package]) -> Value {
    json!({"$map": ps.iter().map(|p| json!([p.name, package_json(p)])).collect::<Vec<_>>()})
}

fn packages(name: &str, a: &[Value]) -> Option<Result<Value>> {
    Some((|| -> Result<Value> {
        use an::edges::packages as p;
        Ok(match name {
            "namesFromDts" => json!(p::names_from_dts(opt_s(a, 0).unwrap_or(""))),
            "membersFromDts" => json!(p::members_from_dts(opt_s(a, 0).unwrap_or(""))),
            "reExports" => json!(p::re_exports(opt_s(a, 0).unwrap_or(""))),
            "declared" => {
                let m = p::declared(a.first().unwrap_or(&Value::Null));
                json!({"$map": m.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()})
            }
            // (names, sources {name: source}, cache [[path, text|null]…]) -> {need:[paths]} | {packages: Map}
            "readPackages" => {
                let names = str_list(a.first());
                let sources = a.get(1).cloned().unwrap_or(Value::Null);
                let r = replay(a.get(2));
                let got = p::read_packages(
                    &r,
                    &names,
                    &|n| sources.get(n).and_then(Value::as_str).unwrap_or("transitive").to_string(),
                    &extract_exports,
                );
                match r.need.into_inner() {
                    Some(need) => json!({"need": need}),
                    None => json!({"packages": packages_map(&got)}),
                }
            }
            // (cache) -> {need} | {manifest, packages: Map, reason?}
            "scanPackages" => {
                let r = replay(a.first());
                let scan = p::scan_packages(&r, &extract_exports);
                match r.need.into_inner() {
                    Some(need) => json!({"need": need}),
                    None => {
                        let mut m = Map::new();
                        m.insert("manifest".into(), scan.manifest.unwrap_or(Value::Null));
                        m.insert("packages".into(), packages_map(&scan.packages));
                        if let Some(reason) = scan.reason {
                            m.insert("reason".into(), json!(reason));
                        }
                        json!({"result": Value::Object(m)})
                    }
                }
            }
            _ => return Err(Error::todo(&format!("analysis/packages.{name}"))),
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(module: &str, name: &str, args: &[Value]) -> Value {
        call(module, name, args).unwrap().unwrap()
    }

    #[test]
    fn parse_basics() {
        assert_eq!(c("analysis/parse", "langOf", &[json!("a.mjs")]), json!("javascript"));
        let r = c("analysis/parse", "extractJavaScript", &[json!("a.js"), json!("export function f() {}\n")]);
        assert_eq!(r["symbols"][0]["name"], "f");
        assert!(call("analysis/parse", "parseSource", &[json!("function {{{")]).unwrap().is_err());
        assert_eq!(c("analysis/parse", "parseSource", &[json!("import x from 'y'")])["sourceType"], "module");
    }

    #[test]
    fn packages_replay_batches() {
        let manifest = json!([["package.json", "{\"dependencies\":{\"a\":\"1\"}}"]]);
        let first = c("analysis/packages", "scanPackages", &[json!([])]);
        assert_eq!(first["need"], json!(["package.json"]));
        let second = c("analysis/packages", "scanPackages", &[manifest.clone()]);
        assert_eq!(second["need"], json!(["node_modules/a/package.json"]));
        let mut cache = manifest.as_array().unwrap().clone();
        cache.push(json!(["node_modules/a/package.json", null]));
        let done = c("analysis/packages", "scanPackages", &[Value::Array(cache)]);
        assert_eq!(done["result"]["packages"], json!({"$map": []}));
    }
}
