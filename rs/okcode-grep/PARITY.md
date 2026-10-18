# okcode-grep parity notes

Regex engine is fancy-regex with a JS-syntax translator, so these differ from V8:

- (a) Variable-length lookbehind, e.g. `(?<=ab+)c`: JS accepts, fancy-regex rejects ("Look-behind assertion without constant size").
- (b) Forward or nonexistent backreferences, e.g. `\1(a)`: JS matches empty; Rust errors.
- (c) Incomplete escapes `\x4`, `\u12`: the JS non-u fallback reads them as literal text; Rust rejects/approximates.
- (d) `\b`/`\B` are translated to ASCII lookarounds (matches JS); `\w`,`\d` are ASCII. Case-insensitive folding of non-ASCII literals is Unicode in Rust vs V8's rules.
- (e) `\s` membership differs slightly (e.g. U+FEFF).
- (f) fancy-regex's backtrack limit surfaces as an error that test()/all() swallow as "no match"; V8 has no such limit.
- Error texts: structural errors (unterminated group/class, unmatched `)`, nothing to repeat) are pre-checked and emit V8's text. Any other compile error carries fancy-regex's own message (positions include an injected `(?i)` prefix), not V8's.
- JS bug not copied: `compileMatcher('a|',{regex:true}).all('😀')` (and tallyText on such input) loops forever in JS u-mode (lastIndex++ lands inside a surrogate pair). Rust terminates.
- `matchText({before:null})` is handled as 0 lines like JS.
