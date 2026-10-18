//! Port of src/analysis/convention-color.js. Offsets in diagnostics are UTF-16 like JS.
use regex::Regex;
use std::sync::OnceLock;

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}
fn decl() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)--([a-z0-9-]+)\s*:\s*([^;}\n]+)")
}
fn color_prop() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r#"(?i)(^|[;{\s"'`])(color|background|background-color|border-color|outline-color|fill|stroke|caret-color|text-decoration-color)\s*:\s*([^;}\n"'`]+)"#,
    )
}
fn hex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)#([0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})(?-u:\b)")
}
fn rgb_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)(?-u:\b)rgba?\(\s*([0-9]{1,3})\s*[, ]\s*([0-9]{1,3})\s*[, ]\s*([0-9]{1,3})")
}
fn var_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"var\(\s*--")
}
fn grad_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)(?-u:\b)(gradient|url|image-set|cross-fade)\s*\(")
}

#[derive(Debug, Clone)]
pub struct Palette {
    pub name: String,
    pub value: String,
    pub rgb: [i64; 3],
}
#[derive(Debug, Clone)]
pub struct Near {
    pub p: Palette,
    pub d: i64,
}
#[derive(Debug, Clone)]
pub struct Diag {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

pub fn rgb_of(value: &str) -> Option<[i64; 3]> {
    if let Some(h) = hex().captures(value) {
        let s = &h[1];
        let b = s.as_bytes();
        let full: String = if b.len() == 3 || b.len() == 4 {
            s[..3].chars().flat_map(|c| [c, c]).collect()
        } else {
            s[..6].to_string()
        };
        let p = |i: usize| i64::from_str_radix(&full[i..i + 2], 16).unwrap();
        return Some([p(0), p(2), p(4)]);
    }
    let r = rgb_re().captures(value)?;
    let n = |i: usize| r[i].parse::<i64>().unwrap();
    Some([n(1), n(2), n(3)])
}

pub fn palette_from(sources: &[Option<String>]) -> Vec<Palette> {
    let mut out: Vec<Palette> = Vec::new();
    for text in sources.iter().flatten() {
        if text.is_empty() {
            continue;
        }
        for m in decl().captures_iter(text) {
            let name = m[1].to_string();
            let value = m[2].trim().to_string();
            if let Some(rgb) = rgb_of(&value) {
                if !out.iter().any(|p| p.name == name) {
                    out.push(Palette { name, value, rgb });
                }
            }
        }
    }
    out
}

pub fn nearest(rgb: [i64; 3], palette: &[Palette]) -> Option<Near> {
    let mut best: Option<&Palette> = None;
    let mut best_d = i64::MAX;
    for p in palette {
        let d: i64 = (0..3).map(|k| (rgb[k] - p.rgb[k]).pow(2)).sum();
        if d < best_d {
            best_d = d;
            best = Some(p);
        }
    }
    match best {
        Some(p) if best_d <= 3 * 64 * 64 => {
            Some(Near { p: p.clone(), d: ((best_d as f64).sqrt() + 0.5).floor() as i64 })
        }
        _ => None,
    }
}

fn utf16_at(s: &str, byte: usize) -> usize {
    s[..byte].encode_utf16().count()
}

/// Diagnostics fixed fields (source/code/severity) are added by the caller of golden_call.
pub fn color_literals(text: &str, palette: &[Palette]) -> Vec<(String, Diag)> {
    let mut out = Vec::new();
    if palette.is_empty() {
        return out;
    }
    for m in color_prop().captures_iter(text) {
        let whole = m.get(0).unwrap();
        let prop = m.get(2).unwrap().as_str();
        let value = m.get(3).unwrap().as_str();
        if var_re().is_match(value) || grad_re().is_match(value) {
            continue;
        }
        let Some(rgb) = rgb_of(value) else { continue };
        let lit = hex().find(value).or_else(|| rgb_re().find(value)).unwrap();
        let lit_s = lit.as_str();
        let at = whole.start() + whole.as_str().find(value).unwrap_or(0) + value.find(lit_s).unwrap();
        let message = match nearest(rgb, palette) {
            Some(n) => format!(
                "{prop}: {lit_s} is a colour literal — this project defines --{} ({}) for this. Use var(--{}), or add a new variable beside the others if none fits.",
                n.p.name, n.p.value, n.p.name
            ),
            None => format!(
                "{prop}: {lit_s} is a colour literal — this project styles colour through CSS variables ({}…). Add a variable beside the others rather than inlining a value.",
                palette.iter().take(4).map(|p| format!("--{}", p.name)).collect::<Vec<_>>().join(", ")
            ),
        };
        let start = utf16_at(text, at);
        out.push((String::new(), Diag { message, start, end: start + lit_s.encode_utf16().count() }));
    }
    out
}
