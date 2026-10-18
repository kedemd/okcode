; Import edges — read generically by treesitter.rs build_imports
; (grouping convention: see queries/typescript/imports.scm; a capture
; prefix before `.stmt`/`.source`/`.name` is its own family — the
; interpreter does not hardcode "import"/"reexport", so `modpath` below
; rides the same machinery with no per-family code).
;
; `@import.source` carries the RAW path text (`crate::a::b`, `super::c`,
; a bare crate name) — splitting `crate::`/`super::`/`self::` from the
; trailing item name, and walking `mod` declarations into a file, is
; edges.js's Rust resolver (DESIGN §12), not this query.

; use a::b::C;  /  use std::collections::HashMap;
(use_declaration
  argument: (scoped_identifier) @import.source) @import.stmt

; use a::b::C as D;
(use_declaration
  argument: (use_as_clause
    path: (_) @import.source
    alias: (identifier) @import.name)) @import.stmt

; use a::b::*;
(use_declaration
  argument: (use_wildcard
    (_) @import.source)) @import.wildcard.stmt

; use a::b::{c, d as e, self};
(use_declaration
  argument: (scoped_use_list
    path: (_) @import.source
    list: (use_list
      [(identifier) @import.name
       (self) @import.name
       (use_as_clause path: (_) @import.importedName alias: (identifier) @import.name)]))) @import.stmt

; mod foo; / pub mod foo; — a child module file, not a text-named
; dependency: `@import.source` is deliberately absent so edges.js can tell
; this family apart from a `use` edge without a separate capture prefix.
(mod_item
  name: (identifier) @modpath.name
  !body) @modpath.stmt
