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

; Call sites — upstream has no `@reference.call` at all. A plain call
; (`helper(1)`) has an `(identifier)` directly in `call_expression`'s
; `function:` field; a call through a struct member (`o.m(2)`, `p->m(2)` —
; the grammar uses the SAME `field_expression` node for both `.` and `->`)
; has it nested one level under `field:`.
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (field_expression
    field: (field_identifier) @name)) @reference.call

; File-scope globals — upstream has no idea a plain variable declaration
; (as opposed to a function prototype, already covered by
; `function_declarator`) is a symbol at all. `(translation_unit
; (declaration ...))` restricts the match to a DIRECT child of the file's
; root, the same bar every other language's top-level rule applies — a
; local inside a function body is nested under `compound_statement`
; instead, never a direct child of `translation_unit`. Two declarator
; shapes: a plain `int counter;` (`declarator:` is the `(identifier)`
; itself) and an initialised `int counter = 0;` (`declarator:` is an
; `init_declarator` wrapping it) — `isExported`'s existing non-`static`
; rule already applies unchanged (same `declaration` node either way).
; Known gap: a pointer/array global (`int *p;`, `int a[3];`) nests the
; identifier one level deeper still and is not matched here.
(translation_unit
  (declaration
    declarator: (identifier) @name) @definition.constant)

(translation_unit
  (declaration
    declarator: (init_declarator
      declarator: (identifier) @name)) @definition.constant)
