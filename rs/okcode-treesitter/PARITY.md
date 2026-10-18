# Parity notes

Three golden cases (extract a.py, extract x.py, validateSource x.py) were recorded while the JS grammar
for Python was not yet loaded (`preload()` not run): JS returned "no symbol extractor yet" / `:none` / no
diagnostics. That is process state, not behaviour; Rust always has the grammar. Not matched on purpose.

Grammar crates are pinned to the manifest versions (`tree-sitter-rust =0.24.0`, `tree-sitter-c =0.24.1`):
the lock had drifted to rust 0.24.2, which parses `&raw` differently from the vendored wasm.

Not ported: JSX fallback (`extractJsxFallback`/`jsxSyntaxDiagnostics`, needs acorn failing first), `preload`,
and the `#strip!`/`#select-adjacent!`/`#set-adjacent!` predicates, which web-tree-sitter ignores (so they are no-ops in JS too).
validateSource with a non-empty palette (convention-color) is outside this crate.
