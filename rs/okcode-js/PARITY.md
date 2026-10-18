# okcode-js parity notes

Measured with `cargo test --release -p okcode-js -- --nocapture` (goldens from the JS build).

- langOf 25/25.
- extract 827/836 in scope (98.9%); JS-family only 736/745. Out of scope (reported as `out-of-scope:` and not counted):
  `.ok.js/.ok.mjs/.ok.html` (okcode-okjs), tree-sitter languages incl. the test-registered yaml extractor (okcode-treesitter).
- validateSource, JS-family 84/85 in scope (98.8%). Out of scope: 5 `.ok.js` cases (okjs-analyze, convention-color palette).
  Non-JS-family validateSource cases are not in the target; non-tree-sitter ones return the `none/skipped` shape, tree-sitter and okjs ones return out-of-scope.

## Known differences
- **treesitter-tsx fallback (7 extract cases fail).** JS stamps `extractor:"treesitter-tsx"` and uses the tree-sitter symbol shapes
  (e.g. `export function A()` signatures, imports with `kind`, `module.exports = function` named `exports`). Here oxc parses the JSX
  and the acorn-shaped symbols are returned with the same `extractor`/`reason` fields, so the field values match but symbols differ.
  Wave 2 should route this through okcode-treesitter.
- **Semantic early errors.** After a clean oxc parse, `parse::fail_of` runs `oxc_semantic::SemanticBuilder` with `check_syntax_error(true)`
  and treats its first error as a parse failure, so extract, try_goals and both goal loops reject redeclarations (`let x; let x;`,
  `let`/`var` clashes), duplicate params in strict functions, `break`/`continue` outside loops or with unknown labels, duplicate labels,
  duplicate constructors, `with` in strict code and undefined exports (`export { nope }`). Remaining difference: messages and positions come
  from oxc (redeclaration is rewritten to acorn's `Identifier 'x' has already been declared`, at the redeclaring identifier); other semantic
  errors keep oxc's wording, and oxc may flag a few edge cases acorn accepts or vice versa (not observed in goldens).
- **Error message text.** acorn's `Unexpected token (L:C)` is reproduced for oxc's "Unexpected token"/"Expected ..." errors; acorn's other
  wordings (`Unexpected keyword 'return'`, ...) are not. `lib/brokenbase.js` differs only in `reason`. Error positions come from oxc and
  can differ from acorn's by a token.
- **node --check is emulated with oxc** (goals: commonjs = script with top-level `return` allowed, module). The `nodeCheck*` goals were skipped
  (they need Node). Node's own wording and caret column are approximated: `Unexpected token '<tok>'` / `Unexpected end of input`.
  The one failing validateSource case is `await 1;` under `commonjs`: acorn's diagnostic is at column 7, ours at 1.
- **convention-color** palette check is not ported (only `.ok.js` goldens carry a palette); a non-empty palette returns out-of-scope.
- `analyzerVersion` is the recorded constant `js:3+jsx:0.23.2+tree-sitter-javascript@0.23.1:q6.1` (TODO(wave2): derive from okcode-treesitter).
- Offsets are UTF-16 units and lines follow acorn's line-break rule (`\n`, `\r\n`, `\r`, U+2028/9), matching the JS output.

## C2 wrapper (pure-parse): can't match
- `parseSource(src)` returned an acorn AST. An AST cannot cross the native boundary; the wrapper returns the stub `{ type: 'Program', sourceType, body: [] }` and throws on a syntax error with acorn-flavoured wording (`test/analysis-parse.test.js` only asserts the throw). Nothing in src/ consumes the AST.
- `readPackages`/`scanPackages` derive a `main`-entry surface with the native `extract` only; a user extractor registered through `registerExtractor` is not consulted there (the JS did consult it).
- `parse.extract` with a user-registered extractor for `javascript` that throws no longer tries the tree-sitter TSX fallback (the native path does that only when no JS extractor is registered).
