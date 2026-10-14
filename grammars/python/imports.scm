; Import edges for F3 — read generically by treesitter.js#buildImports
; (see grammars/typescript/imports.scm for the grouping convention: a plain
; catch-all shares its statement's byte range with the name-bearing
; patterns below it, so a group with zero `@import.name` captures is
; exactly a whole-module import with nothing more specific to resolve).
; Python has no re-export syntax, so there is no `reexport.*` family here.

; import a.b.c  /  import a.b.c as x — a WHOLE-MODULE import, tagged
; `modimport` (its own family — see grammars/typescript/imports.scm for
; the convention) so edges.js's Python resolver never confuses it with a
; `from X import Y` edge below: this one names a MODULE, that one can name
; either a module or a symbol defined inside one, and only the resolver
; (not this query) can tell which. Binds the FIRST segment ('a') at
; runtime, but what this index needs is the module path, so
; `@modimport.name` is only ever present for the aliased form.
(import_statement
  name: (dotted_name) @modimport.source) @modimport.stmt

(import_statement
  name: (aliased_import
    name: (dotted_name) @modimport.source
    alias: (identifier) @modimport.name)) @modimport.stmt

; from .pkg import x, y as z / from . import sibling / from ..pkg2 import w
; `module_name` is the relative_import or dotted_name node as a whole — its
; text carries the leading dots, which edges.js's resolver counts itself.
(import_from_statement
  module_name: (_) @import.source
  name: (dotted_name) @import.name) @import.stmt

(import_from_statement
  module_name: (_) @import.source
  name: (aliased_import
    name: (dotted_name) @import.importedName
    alias: (identifier) @import.name)) @import.stmt

; from pkg import *
(import_from_statement
  module_name: (_) @import.source
  (wildcard_import)) @import.stmt
