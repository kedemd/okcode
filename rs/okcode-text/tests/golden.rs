use serde_json::Value;

fn run(name: &str) -> (usize, usize) {
    let cases = okcode_golden::cases(name);
    let total = cases.len();
    let mut pass = 0;
    let mut shown = 0;
    for (i, c) in cases.iter().enumerate() {
        let got = okcode_text::golden_call(name, &c.args);
        let diff = match (&got, &c.out, &c.err) {
            (Ok(g), Some(w), _) => okcode_golden::first_diff(w, g, "$"),
            (Ok(g), None, None) => okcode_golden::first_diff(&Value::Null, g, "$"),
            (Err(_), None, Some(_)) => None,
            (Ok(_), None, Some(e)) => Some(format!("want error {e}")),
            (Err(e), _, None) => Some(format!("error {e}")),
            (Err(e), Some(_), _) => Some(format!("error {e}")),
        };
        match diff {
            None => pass += 1,
            Some(d) if shown < 5 => {
                shown += 1;
                eprintln!("  {name} case {i}: {d}");
            }
            _ => {}
        }
    }
    println!("golden {name}: {pass}/{total}");
    (pass, total)
}

#[test]
fn golden() {
    let names = [
        "text.decode", "text.dominantEol", "text.hunksFor", "text.lineColAt", "text.lineOffsets",
        "text.lineRangeToOffsets", "text.normaliseEol", "text.offsetAtLineCol",
        "templateBalance.check", "templateBalance.templateSpans",
        "conventionColor.colorLiterals", "conventionColor.nearest", "conventionColor.paletteFrom", "conventionColor.rgbOf",
    ];
    let mut bad = false;
    for n in names {
        let (p, t) = run(n);
        bad |= p != t;
    }
    assert!(!bad, "some golden cases failed (target 100%)");
}
