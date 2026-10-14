; Local override, used IN ADDITION to upstream's tags.scm. Upstream's own
; query (github's code-nav tags.scm) captures a class method the same as a
; free function (`@definition.function`) and has no idea enums or
; constants exist at all — measured against the extraction fixture
; (test/fixtures/php) mirroring the other languages' own local overrides.

; A class/interface/trait method, re-captured as `@definition.method` so
; treesitter.js's dedupe (method outranks function for an identical byte
; range) wins over upstream's own `@definition.function` capture of the
; SAME method_declaration node. Its Class.method PATH then falls out of
; the generic "smallest enclosing definition" nesting rule — a
; method_declaration's span is already CONTAINED by its class/interface/
; trait's own span, same as every other PHP definition here, so no
; explicit @parent is needed (unlike Go's receiver, which lives OUTSIDE
; the type it names).
(method_declaration
  name: (name) @name) @definition.method

; PHP 8.1 enums: upstream has no idea they exist.
(enum_declaration
  name: (name) @name) @definition.enum

; A constant — `const FOO = 1;` at module level, or `const FOO = 1;` inside
; a class/interface/trait (PHP uses the SAME node type, `const_declaration`,
; for both; there is no separate "class const" node). `.` anchors on the
; FIRST named child of `const_element` so a value expression that itself
; references another name (`const BAR = FOO;`) is never mistaken for the
; definition's own name. One `const_declaration` can list several
; comma-separated elements (`const A = 1, B = 2;`); each `const_element`
; gets its own capture so each keeps its own span.
(const_element
  . (name) @name) @definition.constant
