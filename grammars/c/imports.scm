; Import edges for F3 — read generically by treesitter.js#buildImports
; (grouping convention: see grammars/typescript/imports.scm). Two families,
; because a quoted and an angle-bracket `#include` resolve completely
; differently (edges.js's C/C++ resolver): `include` for `#include "x.h"`
; (relative to the including file, then the workspace root, then a
; directory some OTHER include already proved to be a root), `sysinclude`
; for `#include <x.h>` (left unresolved unless it matches a path under a
; directory literally named `include` — a system/library header is not
; expected to live in this workspace at all).
(preproc_include
  path: (string_literal (string_content) @include.source)) @include.stmt

(preproc_include
  path: (system_lib_string) @sysinclude.source) @sysinclude.stmt
