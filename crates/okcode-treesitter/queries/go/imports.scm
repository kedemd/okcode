; Import edges — read generically by treesitter.rs build_imports
; (grouping convention: see queries/typescript/imports.scm). Go has no
; re-export syntax, so there is no `reexport.*` family here. `@import.name`
; is the package's local alias when the import renames it (`m "math/rand"`)
; or blanks it (`_ "net/http/pprof"`); an unaliased import has none — Go's
; own default package name is not known without reading the target
; package, and edges.js's resolver does not need it to match a directory.
(import_spec
  path: (interpreted_string_literal (interpreted_string_literal_content) @import.source)) @import.stmt

(import_spec
  name: (package_identifier) @import.name
  path: (interpreted_string_literal (interpreted_string_literal_content) @import.source)) @import.stmt

(import_spec
  name: (blank_identifier) @import.name
  path: (interpreted_string_literal (interpreted_string_literal_content) @import.source)) @import.stmt
