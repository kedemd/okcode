//! The composed extract() against every recorded case (JS, tree-sitter, okjs, generic).
use okcode_golden::{cases, first_diff};
use serde_json::Value;

#[test]
fn golden_extract() {
    let (mut pass, mut total) = (0, 0);
    let mut diffs = Vec::new();
    for c in cases("extract") {
        let Some(want) = c.out.as_ref() else { continue };
        let path = c.args.first().and_then(Value::as_str).unwrap_or("").to_string();
        // okjs cases recorded with the tooling unavailable ("0:0") cannot be reproduced (okcode-okjs PARITY.md)
        if okcode_okjs::matches(&path) && okcode_okjs::project_expected(want).is_none() {
            continue;
        }
        total += 1;
        match okcode_analysis::parse::golden_call("extract", &c.args) {
            Ok(got) => match first_diff(want, &got, &path) {
                None => pass += 1,
                Some(d) => diffs.push(d),
            },
            Err(e) => diffs.push(format!("{path}: error {e}")),
        }
    }
    println!("golden extract (composed): {pass}/{total}");
    for d in diffs.iter().take(8) {
        println!("  diff {}", &d[..d.len().min(300)]);
    }
    assert!(pass * 100 >= total * 85, "composed extract pass rate {pass}/{total} below 85%");
}
