; Local override, used IN ADDITION to upstream's tags.scm. Upstream captures
; classes, functions and module-level assignments but no docstrings — the
; single highest-signal piece of documentation a Python symbol carries.
;
; `@doc` here is NOT tied to the same match as `@definition.class` /
; `@definition.function` above (tree-sitter queries match independently);
; treesitter.js links a standalone `@doc` capture to a definition by range
; containment — the innermost definition whose body's first statement this
; string is. That containment rule is also why this only needs "first
; statement of the block", not "first statement AND not already documented
; by a capture in the same match": nothing else captures `@doc` for Python.
(class_definition
  body: (block . (expression_statement (string) @doc)))

(function_definition
  body: (block . (expression_statement (string) @doc)))
