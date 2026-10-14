; Import edges for F3 — identical shape to grammars/c/imports.scm (C++
; inherits the C preprocessor's `#include` verbatim; see that file's
; comment for the quoted-vs-angle-bracket resolution split).
(preproc_include
  path: (string_literal (string_content) @include.source)) @include.stmt

(preproc_include
  path: (system_lib_string) @sysinclude.source) @sysinclude.stmt
