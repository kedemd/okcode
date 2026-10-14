; Local override, used IN ADDITION to upstream's tags.scm. Upstream has no
; idea an enum should be its own kind (it captures `enum_specifier` as a
; bare `type`, same as a typedef) and no idea the preprocessor exists at
; all — measured against the extraction fixture mirroring the other
; languages' own local overrides.

; Same node, same range as upstream's own `enum_specifier` match — this
; just gives it a MORE SPECIFIC kind. `enum` outranks `type` in
; treesitter.js's KIND_PRIORITY, so the merge (same byte range) keeps
; this one, exactly the trick Go's receiver override and PHP's method
; override already use on their own upstream patterns.
(enum_specifier
  name: (type_identifier) @name) @definition.enum

; `#define FOO 10` and `#define SQ(x) ((x)*(x))` — the preprocessor's own
; constant/macro, which upstream's tags.scm does not look at at all. One
; kind for both forms: a function-like macro is not a C *function* (no
; type checking, no scope, textual substitution), so folding it into
; `function` would overstate what it actually is.
(preproc_def
  name: (identifier) @name) @definition.macro

(preproc_function_def
  name: (identifier) @name) @definition.macro
