//! Ported from test/edges.test.js: file rows are built by hand (the parsers are other crates).
use okcode_edges::edges::*;
use okcode_edges::packages::Access;
use std::collections::HashMap;

#[derive(Default)]
struct Mem(HashMap<String, String>);
impl Access for Mem {
    fn read(&self, paths: &[String]) -> HashMap<String, Vec<u8>> {
        paths.iter().filter_map(|p| self.0.get(p).map(|t| (p.clone(), t.clone().into_bytes()))).collect()
    }
}

fn nm(local: Option<&str>, imported: Option<&str>) -> Name {
    Name { local: local.map(Into::into), imported: imported.map(Into::into) }
}
fn imp(from: &str, kind: &str, line: i64, names: Option<Vec<Name>>) -> Import {
    Import { from: Some(from.into()), line: Some(line), kind: Some(kind.into()), reexport: false, names }
}
fn file(rel: &str, lang: &str, imports: Vec<Import>) -> FileRow {
    FileRow { rel: rel.into(), lang: Some(lang.into()), imports, symbols: vec![] }
}
fn sym(kind: &str, name: &str, parent: Option<&str>) -> Symbol {
    Symbol { kind: kind.into(), name: name.into(), parent: parent.map(Into::into) }
}
fn build(files: &[FileRow], cfg: &[(&str, &str)]) -> EdgeIndex<Mem> {
    let mut m = Mem::default();
    for (k, v) in cfg {
        m.0.insert(k.to_string(), v.to_string());
    }
    let mut ix = EdgeIndex::new(m);
    ix.ensure(files, 1);
    ix
}
fn importers(ix: &EdgeIndex<Mem>, rel: &str) -> Vec<String> {
    ix.importers_of(rel).iter().map(|i| i.file.clone()).collect()
}

#[test]
fn js_cjs_require() {
    let ix = build(&[file("util.js", "javascript", vec![]), file("index.js", "javascript", vec![imp("./util", "require", 1, None)])], &[]);
    assert_eq!(importers(&ix, "util.js"), ["index.js"]);
}

#[test]
fn js_esm_barrel_one_hop() {
    let mut barrel = file("barrel.mjs", "javascript", vec![imp("./util.mjs", "import", 1, None)]);
    barrel.imports[0].reexport = true;
    let files = [
        file("util.mjs", "javascript", vec![]),
        barrel,
        file("index.mjs", "javascript", vec![imp("./barrel.mjs", "import", 1, Some(vec![nm(Some("helper"), Some("helper"))]))]),
    ];
    let ix = build(&files, &[]);
    let r = ix.importers_of("util.mjs");
    let via = r.iter().find(|i| i.file == "index.mjs").expect("index.mjs importer");
    assert_eq!(via.via.as_deref(), Some("barrel.mjs"));
    assert!(r.iter().any(|i| i.file == "barrel.mjs" && i.via.is_none()));
}

#[test]
fn ts_tsconfig_paths() {
    let cfg = r#"{ // comment
      "compilerOptions": { "baseUrl": "./src", "paths": { "@app/*": ["app/*"], }, }, }"#;
    let files = [
        file("tsconfig.json", "json", vec![]),
        file("src/app/widget.ts", "typescript", vec![]),
        file("src/index.ts", "typescript", vec![imp("@app/widget", "import", 1, Some(vec![nm(Some("thing"), Some("thing"))]))]),
    ];
    let ix = build(&files, &[("tsconfig.json", cfg)]);
    assert_eq!(importers(&ix, "src/app/widget.ts"), ["src/index.ts"]);
    // an external package stays unresolved
    let files = [file("tsconfig.json", "json", vec![]), file("src/index.ts", "typescript", vec![imp("react", "import", 1, None)])];
    assert!(build(&files, &[("tsconfig.json", cfg)]).imports_of("src/index.ts").is_empty());
}

#[test]
fn python_relative() {
    let files = [
        file("pkg/__init__.py", "python", vec![]),
        file("pkg/util.py", "python", vec![]),
        file("pkg/main.py", "python", vec![imp(".util", "from", 1, Some(vec![nm(Some("helper"), Some("helper"))]))]),
    ];
    let ix = build(&files, &[]);
    let r = ix.importers_of("pkg/util.py");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].file, "pkg/main.py");
    assert_eq!(ix.imports_of("pkg/main.py")[0].name.as_deref(), Some("helper"));
}

#[test]
fn python_absolute_and_submodule() {
    let files = [
        file("app/__init__.py", "python", vec![]),
        file("app/sub.py", "python", vec![]),
        file("run.py", "python", vec![imp("app", "from", 1, Some(vec![nm(Some("sub"), Some("sub"))])), imp("app.sub", "modimport", 2, None)]),
    ];
    let ix = build(&files, &[]);
    assert_eq!(importers(&ix, "app/sub.py"), ["run.py"]);
    assert_eq!(importers(&ix, "app/__init__.py"), ["run.py"]);
}

#[test]
fn go_module_dir() {
    let files = [
        file("go.mod", "text", vec![]),
        file("sub/util.go", "go", vec![]),
        file("sub/util_test.go", "go", vec![]),
        file("main.go", "go", vec![imp("example.com/app/sub", "import", 3, None)]),
    ];
    let ix = build(&files, &[("go.mod", "module example.com/app\n\ngo 1.21\n")]);
    assert_eq!(importers(&ix, "sub/util.go"), ["main.go"]);
    assert!(ix.importers_of("sub/util_test.go").is_empty());
}

#[test]
fn rust_crate_tree() {
    let modpath = |n: &str| Import { from: None, line: Some(1), kind: Some("modpath".into()), reexport: false, names: Some(vec![nm(Some(n), None)]) };
    let files = [
        file("src/lib.rs", "rust", vec![modpath("util"), modpath("other")]),
        file("src/util.rs", "rust", vec![]),
        file("src/other.rs", "rust", vec![imp("crate::util::Helper", "use", 2, Some(vec![nm(Some("Helper"), Some("Helper"))]))]),
    ];
    let ix = build(&files, &[]);
    assert!(importers(&ix, "src/util.rs").contains(&"src/other.rs".to_string()));
    assert!(ix.imports_of("src/lib.rs").iter().any(|e| e.to == "src/util.rs" && e.kind.as_deref() == Some("modpath")));
}

#[test]
fn okjs_component_tag() {
    let mut def = file("x-widget.ok.js", "javascript", vec![]);
    def.symbols.push(sym("component", "x-widget", None));
    let user = file("page.ok.js", "javascript", vec![imp("x-widget", "component", 4, None)]);
    let ix = build(&[def, user], &[]);
    assert_eq!(importers(&ix, "x-widget.ok.js"), ["page.ok.js"]);
}

#[test]
fn cache_invalidation() {
    let mut ix = build(&[file("a.js", "javascript", vec![]), file("b.js", "javascript", vec![])], &[]);
    assert!(ix.importers_of("a.js").is_empty());
    let files = [file("a.js", "javascript", vec![]), file("b.js", "javascript", vec![imp("./a", "import", 1, None)])];
    ix.ensure(&files, 1); // same generation: cached
    assert!(ix.importers_of("a.js").is_empty());
    ix.ensure(&files, 2);
    assert_eq!(importers(&ix, "a.js"), ["b.js"]);
    ix.invalidate();
    ix.ensure(&files, 2);
    assert_eq!(importers(&ix, "a.js"), ["b.js"]);
}

#[test]
fn php_psr4_and_require() {
    let composer = r#"{"autoload":{"psr-4":{"App\\":"src/"}}}"#;
    let files = [
        file("composer.json", "json", vec![]),
        file("src/Foo/Bar.php", "php", vec![]),
        file("lib/helpers.php", "php", vec![]),
        file("index.php", "php", vec![
            imp("App\\Foo", "import", 1, Some(vec![nm(Some("Bar"), Some("Bar")), nm(Some("Nope"), Some("Nope"))])),
            imp("lib/helpers", "require", 2, None),
        ]),
    ];
    let ix = build(&files, &[("composer.json", composer)]);
    assert_eq!(importers(&ix, "src/Foo/Bar.php"), ["index.php"]);
    assert_eq!(importers(&ix, "lib/helpers.php"), ["index.php"]);
    assert_eq!(ix.imports_of("index.php").len(), 2); // Nope is unresolved
}

#[test]
fn java_import_star_static() {
    let mut a = file("a/b/C.java", "java", vec![]);
    a.symbols = vec![sym("module", "a.b", None), sym("class", "C", None)];
    let user = file("Main.java", "java", vec![
        imp("a.b.C", "import", 1, None),
        imp("a.b", "importstar", 2, None),
        imp("a.b.C.member", "staticimport", 3, None),
        imp("java.util.List", "import", 4, None),
    ]);
    let ix = build(&[a, user], &[]);
    let e = ix.imports_of("Main.java");
    assert_eq!(e.len(), 3);
    assert_eq!(e[2].name.as_deref(), Some("member"));
    assert_eq!(importers(&ix, "a/b/C.java").len(), 3); // one row per (file, line)
}

#[test]
fn c_include_and_sysinclude() {
    let files = [
        file("src/x.h", "c", vec![]),
        file("include/y.h", "c", vec![]),
        file("src/x.c", "c", vec![imp("x.h", "include", 1, None), imp("<y.h>", "sysinclude", 2, None), imp("<stdio.h>", "sysinclude", 3, None)]),
    ];
    let ix = build(&files, &[]);
    let e = ix.imports_of("src/x.c");
    assert_eq!(e.iter().map(|e| e.to.as_str()).collect::<Vec<_>>(), ["src/x.h", "include/y.h"]);
}

#[test]
fn js_multi_name_import_lists_importer_once() {
    let files = [
        file("m.js", "javascript", vec![]),
        file("u.js", "javascript", vec![imp("./m", "import", 1, Some(vec![nm(Some("a"), Some("a")), nm(Some("b"), Some("b"))]))]),
    ];
    let ix = build(&files, &[]);
    assert_eq!(ix.imports_of("u.js").len(), 2);
    let r = ix.importers_of("m.js");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].names.as_ref().unwrap().len(), 2);
}

#[test]
fn jsonc_strips_comments_not_strings() {
    let s = strip_jsonc("{ \"u\": \"http://x/*y*/\", /* c */ \"a\": [1,2,], // t\n }");
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["u"], "http://x/*y*/");
}
