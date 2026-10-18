//! Tiny text helpers (UTF-16 offsets, like the JS).

/// 1-indexed line/column for a UTF-16 offset; only `\n` breaks a line (text.js lineColAt).
pub fn line_col_at(src: &[u16], offset: usize) -> (usize, usize) {
    let stop = offset.min(src.len());
    let (mut line, mut col) = (1, 1);
    for &c in &src[..stop] {
        if c == 10 {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Inverse of `line_col_at` (text.js offsetAtLineCol).
pub fn offset_at_line_col(src: &[u16], line: usize, column: usize) -> usize {
    let (mut idx, mut ln) = (0usize, 1usize);
    while ln < line && idx < src.len() {
        if src[idx] == 10 {
            ln += 1;
        }
        idx += 1;
    }
    (idx + column.max(1) - 1).min(src.len())
}

/// JS `\s` / String.prototype.trim whitespace.
pub fn is_js_ws(c: u32) -> bool {
    matches!(
        c,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| is_js_ws(c as u32))
}

/// `s.slice(0, n)` in UTF-16 units (a split surrogate pair is dropped rather than emitted lone).
pub fn slice_units(s: &str, n: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        let l = c.len_utf16();
        if units + l > n {
            break;
        }
        units += l;
        out.push(c);
    }
    out
}
