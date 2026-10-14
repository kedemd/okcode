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
