; Local override, used IN ADDITION to upstream's tags.scm (concatenated by
; treesitter.js). Upstream's query is written for code navigation and only
; captures INTERFACES and SIGNATURE-only methods/functions — a plain class,
; a function declaration, an exported `const` arrow function, a type alias,
; an enum and a class's own methods are all invisible to it. Measured against
; spike/fixtures/sample.ts: without this file, Circle/Shape/helper/makeCircle
; etc. produced zero symbols.

(class_declaration
  name: (type_identifier) @name) @definition.class

(function_declaration
  name: (identifier) @name) @definition.function

(method_definition
  name: (property_identifier) @name) @definition.method

(type_alias_declaration
  name: (type_identifier) @name) @definition.type

(enum_declaration
  name: (identifier) @name) @definition.enum

; Every top-level `const`/`let`/`var` binding is an addressable symbol, the
; same rule parse.js's JS extractor already applies — a plain
; `export const CHART_COLORS = {...}`, `const config = {...}` with no
; `export` of its own line, or `const Card = forwardRef(...)`/`memo(...)`
; (a component built by a CALL, not a bare arrow/function) all produced
; nothing before this. Kind defaults to `constant` here and is overridden to
; `function` by buildSymbols' same-range merge (KIND_PRIORITY) whenever the
; value IS an arrow/function — JS's own layered-in pattern above already
; captures that case at every depth, this one only needs to add the rest,
; restricted to `program`'s own direct children (plus one `export_statement`
; wrapper) so a LOCAL `const rows = []` inside a function body stays
; unaddressed, same as the JS extractor's "inside a scope only functions
; earn a name" rule. `(identifier) @name` already excludes a destructuring
; binding (`const { a, b } = x`) — nothing there to turn into one name.
(program
  (lexical_declaration
    (variable_declarator name: (identifier) @name) @definition.constant))

(program
  (variable_declaration
    (variable_declarator name: (identifier) @name) @definition.constant))

(program
  (export_statement
    declaration: (lexical_declaration
      (variable_declarator name: (identifier) @name) @definition.constant)))

(program
  (export_statement
    declaration: (variable_declaration
      (variable_declarator name: (identifier) @name) @definition.constant)))

; `export const f = (...) => ...` / `= function (...) {...}` — the same
; "named closure" shape parse.js's acorn extractor treats as a function.
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function
