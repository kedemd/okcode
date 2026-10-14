; Import/export edges for F3 (resolved edges). Two independent families,
; read generically by treesitter.js#buildImports: `import.*` (this file
; depends on another) and `reexport.*` (this file re-exports from another,
; so `importers()` can follow it one hop — see DESIGN §12/WORKPLAN F3).
;
; `@import.stmt`/`@reexport.stmt` is the grouping key: every pattern below
; also fires the plain "just a source, no names" one for the SAME
; statement (`import_clause`/`export_clause` are plain children here, not
; named fields, so there is no field to negate against) — treesitter.js
; groups matches by the statement's byte range and unions every `@name`
; capture across them, so a plain catch-all with zero names is exactly
; `import './x'` (side-effect) or `export * from './x'` (star re-export)
; by construction, with no per-language elimination code needed.
;
; `@import.name` is always the name BOUND in this file (what refs()
; compares a hit against); `@import.importedName` is the pre-alias name
; from the source module, captured only when it differs from the local one.

(import_statement
  source: (string (string_fragment) @import.source)) @import.stmt

; import Foo from './foo'
(import_statement
  (import_clause (identifier) @import.name)
  source: (string (string_fragment) @import.source)) @import.stmt

; import { bar, baz as qux } from './bar'
(import_statement
  (import_clause
    (named_imports
      (import_specifier
        name: (identifier) @import.importedName
        alias: (identifier)? @import.name)))
  source: (string (string_fragment) @import.source)) @import.stmt

; import * as ns from './ns'
(import_statement
  (import_clause (namespace_import (identifier) @import.name))
  source: (string (string_fragment) @import.source)) @import.stmt

(export_statement
  source: (string (string_fragment) @reexport.source)) @reexport.stmt

; export { a, b as c } from './reex' — keeps the SOURCE's own name (what
; the target file defines), not the local alias, since nothing local binds it.
(export_statement
  (export_clause
    (export_specifier
      name: (identifier) @reexport.name))
  source: (string (string_fragment) @reexport.source)) @reexport.stmt

; CommonJS require() — needed so the JSX fallback path (parse.js falls back
; to THIS grammar, via the .tsx wasm, when acorn cannot parse a file because
; of JSX) never loses import edges acorn's own extractor already finds for
; `require()`; also picked up for real .ts/.tsx files that mix require()
; with ESM, which upstream's own tags.scm never covers at all. A predicate
; needs its own enclosing group (the capture and the `#eq?` on it must be
; siblings inside one more pair of parens, same shape csharp's
; tags.local.scm `const`-field override already uses) — without it the
; predicate is a separate top-level pattern, not attached to this one.

; const x = require('./x')
(
  (lexical_declaration
    (variable_declarator
      name: (identifier) @import.name
      value: (call_expression
        function: (identifier) @_require
        arguments: (arguments (string (string_fragment) @import.source))))) @import.stmt
  (#eq? @_require "require")
)

(
  (variable_declaration
    (variable_declarator
      name: (identifier) @import.name
      value: (call_expression
        function: (identifier) @_require
        arguments: (arguments (string (string_fragment) @import.source))))) @import.stmt
  (#eq? @_require "require")
)

; const { a, b: c } = require('./x') — `a` is a shorthand pattern (bound
; name equals its own source name); `b: c` is a pair_pattern (key is the
; source-side name, value is the local alias) — same two shapes acorn's own
; ObjectPattern handling in parse.js#variables already splits.
(
  (lexical_declaration
    (variable_declarator
      name: (object_pattern
        (shorthand_property_identifier_pattern) @import.name)
      value: (call_expression
        function: (identifier) @_require
        arguments: (arguments (string (string_fragment) @import.source))))) @import.stmt
  (#eq? @_require "require")
)

(
  (lexical_declaration
    (variable_declarator
      name: (object_pattern
        (pair_pattern
          key: (property_identifier) @import.importedName
          value: (identifier) @import.name))
      value: (call_expression
        function: (identifier) @_require
        arguments: (arguments (string (string_fragment) @import.source))))) @import.stmt
  (#eq? @_require "require")
)

; a bare `require('./x')` with no binding at all — a side-effect import,
; the same shape a bare `import './x'` already produces no @import.name for.
(
  (expression_statement
    (call_expression
      function: (identifier) @_require
      arguments: (arguments (string (string_fragment) @import.source)))) @import.stmt
  (#eq? @_require "require")
)
