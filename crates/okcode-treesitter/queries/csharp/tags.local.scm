; Local override, used IN ADDITION to upstream's tags.scm. Upstream only
; knows class/interface/method/namespace — no struct, no enum, no record,
; no property — measured against the extraction fixture mirroring the
; other languages' own local overrides.

; Upstream's own `namespace_declaration` pattern only matches a
; SINGLE-segment name (`name: (identifier)`) — a dotted one (`namespace
; App.Models { ... }`, by far the common case) parses as `qualified_name`
; instead, which upstream's pattern silently never matches at all.
(namespace_declaration
  name: (qualified_name) @name) @definition.module

(struct_declaration
  name: (identifier) @name) @definition.class

(enum_declaration
  name: (identifier) @name) @definition.enum

; `record Person(string Name);` (positional) or `record Person { ... }`
; (with a body) — same kind as a class, same reason Java's record above
; gets one.
(record_declaration
  name: (identifier) @name) @definition.class

; `public int Bar { get; set; }` — a property is its own thing, not a
; field (C# has no concept of a bare public field worth indexing the way
; a method is), so it gets its own kind rather than being folded into
; `method` or skipped like PHP's plain properties.
(property_declaration
  name: (identifier) @name) @definition.property

; Call sites — upstream has no `@reference.call` at all. A plain function
; call (`helper(1)`, a local/static method) has an `(identifier)` directly
; in `invocation_expression`'s `function:` field. A member call (`o.m(2)`)
; is upstream's own `member_access_expression` shape, already captured as
; `@reference.send` — same node, same range, but `send` is not a kind this
; project's storage layer keeps (`store.js` only persists `kind === 'call'`),
; so it is re-captured here under the kind this project actually looks for.
(invocation_expression
  function: (identifier) @name) @reference.call

(invocation_expression
  function: (member_access_expression
    name: (identifier) @name)) @reference.call

; A `const` field — C#'s own spelling of a compile-time constant, same
; role Java's `static final` field already gets, same reason a plain
; (mutable) field is still left unindexed: a property or an ordinary field
; is not something read('Type#name') needs to resolve, a true constant is.
; C#'s modifiers are separate sibling `modifier` nodes (not Java's single
; bundled one — see the decorators comment on the LANGUAGES table in
; treesitter.rs), so matching one `(modifier)` child against `#eq?` "const"
; finds it regardless of what other modifiers (there are none valid to
; combine with `const` anyway) are also present.
(
  (field_declaration
    (modifier) @_mod
    (variable_declaration
      (variable_declarator
        name: (identifier) @name))) @definition.constant
  (#eq? @_mod "const")
)
