use okcode_edges::packages::*;
use std::collections::HashMap;

struct Mock(HashMap<String, Vec<u8>>);
impl Access for Mock {
    fn read(&self, paths: &[String]) -> HashMap<String, Vec<u8>> {
        paths.iter().filter_map(|p| self.0.get(p).map(|b| (p.clone(), b.clone()))).collect()
    }
}
fn mock(files: &[(&str, &str)]) -> Mock {
    Mock(files.iter().map(|(k, v)| (k.to_string(), v.as_bytes().to_vec())).collect())
}
fn names(n: &[&str]) -> Vec<String> {
    n.iter().map(|s| s.to_string()).collect()
}
fn ex(_p: &str, t: &str) -> Vec<String> {
    // stand-in for parse.js extract: `exports.X =` names
    t.lines().filter_map(|l| l.strip_prefix("exports.").and_then(|r| r.split(' ').next()).map(String::from)).collect()
}
fn read(m: &Mock, n: &[&str]) -> Vec<Package> {
    read_packages(m, &names(n), &|_| "transitive".into(), &ex)
}

#[test]
fn scoped_and_missing() {
    let m = mock(&[("node_modules/@scope/deep/package.json", r#"{"name":"@scope/deep","version":"1.0.0","types":"./t/a.d.ts"}"#),
        ("node_modules/@scope/deep/t/a.d.ts", "export declare function foo(): void;\nexport class Bar {\n  baz(): void;\n}\n")]);
    let p = read(&m, &["@scope/deep", "nope"]);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].surface, ["foo", "Bar"]);
    assert_eq!(p[0].members, ["Bar.baz"]);
    assert_eq!(p[0].surface_file.as_deref(), Some("t/a.d.ts"));
    assert_eq!(p[0].surface_from.as_deref(), Some("types"));
}

#[test]
fn broken_manifest_skipped() {
    let m = mock(&[("node_modules/bare/package.json", "{}"), ("node_modules/broken/package.json", "{nope")]);
    let p = read(&m, &["bare", "broken"]);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].name, "bare");
    assert!(p[0].surface.is_empty() && p[0].surface_from.is_none());
}

#[test]
fn caps_with_dedupe_first() {
    let mut dts = String::new();
    for i in 0..600 {
        dts += &format!("export const n{i} = 1;\nexport const n{i} = 2;\n");
    }
    dts += "export interface I {\n";
    for i in 0..1600 {
        dts += &format!("  m{i}(): void;\n  m{i}(x: 1): void;\n");
    }
    dts += "}\n";
    let m = mock(&[("node_modules/big/package.json", r#"{"types":"index.d.ts"}"#), ("node_modules/big/index.d.ts", &dts)]);
    let p = &read(&m, &["big"])[0];
    assert_eq!(p.surface.len(), 500);
    assert!(p.surface_truncated);
    assert_eq!(p.members.len(), 1500);
    assert!(p.members_truncated);
    // exactly at the cap after dedupe: not truncated
    let mut d2 = String::new();
    for i in 0..250 {
        d2 += &format!("export const a{i} = 1;\nexport const a{i} = 1;\n");
    }
    let m = mock(&[("node_modules/ok/package.json", "{}"), ("node_modules/ok/index.d.ts", &d2)]);
    let p = &read(&m, &["ok"])[0];
    assert_eq!(p.surface.len(), 250);
    assert!(!p.surface_truncated && !p.members_truncated);
}

#[test]
fn scan_no_root_and_bad_root() {
    let s = scan_packages(&mock(&[]), &ex);
    assert!(s.manifest.is_none() && s.packages.is_empty());
    assert_eq!(s.reason.as_deref(), Some("no package.json at the workspace root"));
    let s = scan_packages(&mock(&[("package.json", "{oops")]), &ex);
    assert!(s.manifest.is_none());
    assert!(s.reason.unwrap().starts_with("package.json does not parse"));
}

#[test]
fn fallback_order() {
    // types missing -> index.d.ts wins
    let m = mock(&[("node_modules/a/package.json", r#"{"types":"gone.d.ts","main":"m.js"}"#),
        ("node_modules/a/index.d.ts", "export const fromIdx = 1;"), ("node_modules/a/m.js", "exports.fromMain = 1")]);
    let p = &read(&m, &["a"])[0];
    assert_eq!((p.surface.clone(), p.surface_file.as_deref()), (names(&["fromIdx"]), Some("index.d.ts")));
    // no .d.ts at all -> main via extract_exports
    let m = mock(&[("node_modules/b/package.json", r#"{"main":"./lib/m.js"}"#), ("node_modules/b/lib/m.js", "exports.fromMain = 1")]);
    let p = &read(&m, &["b"])[0];
    assert_eq!(p.surface, ["fromMain"]);
    assert_eq!(p.surface_from.as_deref(), Some("main"));
    assert_eq!(p.surface_file.as_deref(), Some("lib/m.js"));
    assert!(p.members.is_empty());
    // main missing -> index.js
    let m = mock(&[("node_modules/c/package.json", r#"{"main":"gone.js"}"#), ("node_modules/c/index.js", "exports.fromIndex = 1")]);
    let p = &read(&m, &["c"])[0];
    assert_eq!((p.surface.clone(), p.surface_file.as_deref()), (names(&["fromIndex"]), Some("index.js")));
}

#[test]
fn one_hop_reexports() {
    let m = mock(&[("node_modules/r/package.json", r#"{"types":"dist/index.d.ts"}"#),
        ("node_modules/r/dist/index.d.ts", "export * from './x';\nexport * from \"./y\";\nexport * from './z.d.ts';\nexport const top = 1;\n"),
        ("node_modules/r/dist/x.d.ts", "export const fromX = 1;\nexport * from './deeper';"),
        ("node_modules/r/dist/y/index.d.ts", "export const fromY = 1;"),
        ("node_modules/r/dist/z.d.ts", "export const fromZ = 1;"),
        ("node_modules/r/dist/deeper.d.ts", "export const tooDeep = 1;")]);
    let p = &read(&m, &["r"])[0];
    assert_eq!(p.surface, ["top", "fromX", "fromZ", "fromY"]);
}

#[test]
fn escaping_reexport_dropped_and_no_panic() {
    let m = mock(&[("node_modules/e/package.json", r#"{"types":"index.d.ts"}"#),
        ("node_modules/e/index.d.ts", "export * from '../../../../../x';\nexport const k = 1;"),
        ("x.d.ts", "export const leaked = 1;")]);
    assert_eq!(read(&m, &["e"])[0].surface, ["k"]);
    // types climbing out of the package: no panic
    let m = mock(&[("node_modules/longpackagename/package.json", r#"{"types":"../../x.d.ts"}"#), ("x.d.ts", "export const out = 1;")]);
    let p = &read(&m, &["longpackagename"])[0];
    assert_eq!(p.surface, ["out"]);
    assert_eq!(p.surface_file.as_deref(), Some(""));
}

#[test]
fn sources_first_wins() {
    let root = r#"{"dependencies":{"d":"1","both":"1"},"devDependencies":{"v":"1","both":"1"},"optionalDependencies":{"o":"1"},"peerDependencies":{"p":"1","d":"1"}}"#;
    let m = mock(&[("package.json", root), ("node_modules/d/package.json", "{}"), ("node_modules/v/package.json", "{}"),
        ("node_modules/o/package.json", "{}"), ("node_modules/p/package.json", "{}"), ("node_modules/both/package.json", "{}")]);
    let s = scan_packages(&m, &ex);
    let got: Vec<(String, String)> = s.packages.iter().map(|p| (p.name.clone(), p.source.clone())).collect();
    let w = |a: &str, b: &str| (a.to_string(), b.to_string());
    assert_eq!(got, vec![w("d", "dependency"), w("both", "dependency"), w("v", "dev"), w("o", "optional"), w("p", "peer")]);
}
