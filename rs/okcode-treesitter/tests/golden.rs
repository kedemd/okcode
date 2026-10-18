//! Goldens for extract / validateSource, filtered to paths whose language treesitter.js handles
//! (.ts .mts .cts .tsx .py .go .rs .php .phtml .java .cs .c .cpp .cc .cxx .h .hpp .hh .hxx .ipp).
//! validateSource cases with a non-empty palette are skipped (convention-color is another crate).
use okcode_golden::{cases, first_diff};
use serde_json::Value;

const EXTS: &[&str] = &[".ts", ".mts", ".cts", ".tsx", ".py", ".go", ".rs", ".php", ".phtml", ".java", ".cs", ".c", ".cpp", ".cc", ".cxx", ".h", ".hpp", ".hh", ".hxx", ".ipp"];

fn handled(path: &str) -> bool {
    let p = path.to_lowercase();
    EXTS.iter().any(|e| p.ends_with(e))
}

fn run(name: &str, threshold: f64) {
    let (mut pass, mut total) = (0, 0);
    let mut diffs = Vec::new();
    for c in cases(name) {
        let path = match &c.args[0] {
            Value::String(s) => s.clone(),
            v => v["rel"].as_str().unwrap_or("").to_string(),
        };
        if !handled(&path) {
            continue;
        }
        if name == "validateSource" && c.args[0]["palette"].as_array().map_or(false, |p| !p.is_empty()) {
            continue;
        }
        total += 1;
        let got = okcode_treesitter::golden_call(name, &c.args);
        let d = match (&c.out, got) {
            (Some(want), Ok(got)) => first_diff(want, &got, "$"),
            (Some(_), Err(e)) => Some(format!("error: {e}")),
            (None, _) => Some("expected error".into()),
        };
        match d {
            None => pass += 1,
            Some(d) => diffs.push(format!("{path}: {d}")),
        }
    }
    println!("golden {name}: {pass}/{total}");
    for d in diffs.iter().take(8) {
        println!("  diff {d}");
    }
    assert!(total > 0);
    assert!(pass as f64 / total as f64 >= threshold, "{name}: {pass}/{total}");
}

#[test]
fn extract() {
    run("extract", 0.95);
}

#[test]
fn validate_source() {
    run("validateSource", 0.95);
}
