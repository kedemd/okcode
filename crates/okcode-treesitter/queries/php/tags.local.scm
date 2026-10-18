; Local override, used IN ADDITION to upstream's tags.scm. Upstream's own
; query (github's code-nav tags.scm) captures a class method the same as a
; free function (`@definition.function`) and has no idea enums or
; constants exist at all — measured against the extraction fixture
; (test/fixtures/php) mirroring the other languages' own local overrides.

; A class/interface/trait method, re-captured as `@definition.method` so
; treesitter.rs's dedupe (method outranks function for an identical byte
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

; A plain function call, `helper(1)` — upstream's own `function_call_expression`
; pattern only matches a NAMESPACED callee (`function: (qualified_name (name))`)
; or a dynamic one (`function: (variable_name (name))`, `$fn()`); a bare,
; unqualified call's `function:` field is just a `(name)` node directly, which
; neither alternative matches, so `helper(1)` produced no reference at all
; (`$o->m(2)` already works — upstream's own `member_call_expression` pattern
; covers it).
(function_call_expression
  function: (name) @name) @reference.call

; `define('FOO', 1)` — PHP's other constant form, a plain function call
; rather than the `const` keyword `const_element` above already captures.
; `#eq?` pins the callee to literally `define`; the FIRST argument's string
; content becomes the symbol's name (`.` anchors there so a later argument
; that happens to be a string, e.g. `define('FOO', 'bar')`'s value, is
; never mistaken for the name). The whole call expression is the span, same
; as any other top-level statement.
(
  (function_call_expression
    function: (name) @_fn
    arguments: (arguments
      .
      (argument (string (string_content) @name)))) @definition.constant
  (#eq? @_fn "define")
)
