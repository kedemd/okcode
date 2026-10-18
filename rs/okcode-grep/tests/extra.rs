use okcode_grep::*;

fn m(p: &str) -> Matcher {
    compile_matcher(p, true, &CaseMode::Sensitive(true)).unwrap()
}
fn err(p: &str) -> String {
    compile_matcher(p, true, &CaseMode::Sensitive(true)).err().unwrap()
}

#[test]
fn unicode_props() {
    assert_eq!(m(r"\p{L}+").all("ab1é"), vec!["ab", "é"]);
    assert_eq!(m(r"\P{Lu}").test("ABc"), 2);
    assert_eq!(m(r"[\p{L}_]+").all("a_b-c"), vec!["a_b", "c"]);
}

#[test]
fn error_texts() {
    assert!(err("[a").ends_with("Unterminated character class"), "{}", err("[a"));
    assert!(err("a)").ends_with("Unmatched ')'"));
    assert!(err("(a").ends_with("Unterminated group"));
    assert!(err("*a").ends_with("Nothing to repeat"));
    assert!(m("(?:a)+(?=b)").test("aab") == 0);
}

#[test]
fn word_boundary_ascii() {
    assert_eq!(m(r"\bfoo\b").test("é foo"), 2);
    assert_eq!(m(r"\bb").test("ab"), -1);
}

#[test]
fn path_matcher_filters() {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let pm = path_matcher(&v(&["*.js", "!test/**"]), &v(&[])).unwrap().unwrap();
    assert!(pm.matches("src/a.js"));
    assert!(!pm.matches("src/a.ts"));
    assert!(!pm.matches("test/a.js"));
    // a directory glob takes its subtree
    let pm = path_matcher(&v(&["src"]), &v(&[])).unwrap().unwrap();
    assert!(pm.matches("src/deep/x.rs"));
    assert!(!pm.matches("lib/x.rs"));
    // roots
    let pm = path_matcher(&v(&[]), &v(&["./src", "docs/"])).unwrap().unwrap();
    assert!(pm.matches("src/a"));
    assert!(pm.matches("docs"));
    assert!(!pm.matches("srcx/a"));
    assert!(path_matcher(&v(&[]), &v(&["."])).unwrap().is_none());
}

#[test]
fn utf16_cols_and_clip() {
    let mt = compile_matcher("b", false, &CaseMode::Sensitive(true)).unwrap();
    let (r, c) = match_text("😀b\n", &mt, &MatchOpts::default());
    assert_eq!((c, r[0].col), (1, 3)); // 1-based, astral char is 2 units
    let long = format!("{}needle{}", "x".repeat(400), "y".repeat(400));
    let mt = compile_matcher("needle", false, &CaseMode::Sensitive(true)).unwrap();
    let (r, _) = match_text(&long, &mt, &MatchOpts::default());
    let t = &r[0].text;
    assert!(t.starts_with('…') && t.ends_with('…') && t.contains("needle"));
    assert_eq!(t.encode_utf16().count(), 302);
}

#[test]
fn tally_map() {
    let mt = compile_matcher("a+", true, &CaseMode::Sensitive(true)).unwrap();
    let mut t = vec![];
    assert_eq!(tally_text("aa a\nb\naa\n", &mt, &mut t, Some("f1")), 2);
    assert_eq!(tally_text("a\n", &mt, &mut t, Some("f2")), 1);
    let get = |k: &str| t.iter().find(|(n, _)| n == k).unwrap().1.clone();
    let aa = get("aa");
    assert_eq!((aa.count, aa.lines, aa.files), (2, 2, 1));
    assert_eq!(aa.first.as_deref(), Some("f1"));
    let a = get("a");
    assert_eq!((a.count, a.lines, a.files), (2, 2, 2));
    assert_eq!(t[0].0, "aa");
}

#[test]
fn empty_match_before_astral_terminates() {
    assert!(m("a|").all("😀").is_empty());
}
