; Local override, used IN ADDITION to upstream's tags.scm. Upstream has no
; idea a namespace exists, files an enum under the bare `type` kind, and
; (for an out-of-line `Foo::bar() {}` definition) finds the method but not
; its class — measured against the extraction fixture mirroring the other
; languages' own local overrides.

(namespace_definition
  name: (namespace_identifier) @name) @definition.module

; A method declared (or defined) INSIDE a class/struct body — upstream's
; own `field_identifier` pattern files this under the bare `function`
; kind, same as a free function; same node, same range as that match —
; `method` outranks `function` in treesitter.js's KIND_PRIORITY, so the
; merge keeps this kind, same trick as the enum override above.
(function_declarator
  declarator: (field_identifier) @name) @definition.method

; Same node, same range as upstream's own `enum_specifier` match — see
; grammars/c/tags.local.scm's identical comment (`enum` outranks `type` in
; treesitter.js's KIND_PRIORITY, so the merge keeps this kind).
(enum_specifier
  name: (type_identifier) @name) @definition.enum

; `#define` — C++ inherits the C preprocessor verbatim; see
; grammars/c/tags.local.scm for why this is its own kind, not `function`.
(preproc_def
  name: (identifier) @name) @definition.macro

(preproc_function_def
  name: (identifier) @name) @definition.macro

; An out-of-line method (`ReturnType Foo::bar() { ... }`, defined outside
; the class body it belongs to — the class itself may be declared in a
; completely different file, e.g. the matching header). SAME node, SAME
; range as upstream's own qualified-identifier `@definition.method` match
; (the `function_declarator` wrapping a `qualified_identifier`) — this
; just adds the `@parent` upstream's own pattern leaves uncaptured (it
; names the capture `@local.scope`, which this project's generic engine
; does not look for). `@parent`'s TEXT becomes the parent name directly —
; same "no containment needed" shape Go's receiver override already uses,
; since the class genuinely may not even be IN this file's own parse tree.
(function_declarator
  declarator: (qualified_identifier
    scope: (namespace_identifier) @parent
    name: (identifier) @name)) @definition.method

; Call sites — upstream has no `@reference.call` at all. Same three shapes
; as grammars/c/tags.local.scm (a plain call's callee is an `(identifier)`;
; a member call's is a `field_expression`'s `field:`), plus C++'s own
; namespace-qualified callee (`ns::fn()`), which parses as a
; `qualified_identifier` the same way an out-of-line method's NAME does
; above — here it is the whole call's callee, not a definition's.
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (field_expression
    field: (field_identifier) @name)) @reference.call

(call_expression
  function: (qualified_identifier
    name: (identifier) @name)) @reference.call

; File-scope globals (incl. `constexpr`, which this grammar files as just
; another leading qualifier on the same `declaration` node, so no separate
; pattern is needed for it) — see grammars/c/tags.local.scm's identical
; pair for the two declarator shapes and the known pointer/array gap.
; Restricted to a DIRECT child of `translation_unit`: a namespace's own
; globals (`namespace ns { int y = 1; }`) are one level deeper still and
; are not matched here, the same conservative, file-root-only scope
; Rust's crate-root `const`/`static` override above draws.
(translation_unit
  (declaration
    declarator: (identifier) @name) @definition.constant)

(translation_unit
  (declaration
    declarator: (init_declarator
      declarator: (identifier) @name)) @definition.constant)
