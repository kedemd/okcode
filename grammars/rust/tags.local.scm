; Local override, used IN ADDITION to upstream's tags.scm.
;
; Upstream captures a function inside an `impl` block TWICE: once as
; `@definition.method` (the generic "function inside a declaration_list"
; pattern, which also fires for `mod` and `trait` bodies) and once as
; `@definition.function` (the generic "any function_item" pattern) — the
; same range under two kinds. treesitter.js's dedupe already prefers
; `method` over `function` for an exact range collision, so that half is
; already handled upstream; what upstream has NO way to express is which
; TYPE the impl is for. `@parent` (see grammars/go/tags.local.scm for the
; convention) supplies it directly, so `Circle::new`'s path is `Circle.new`
; rather than a context-free `new`. Combined with the dedupe's "prefers a
; parent-carrying match" rule, this entry wins over both upstream ones.
(impl_item
  type: (type_identifier) @parent
  body: (declaration_list
    (function_item
      name: (identifier) @name) @definition.method))
