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

; `export const f = (...) => ...` / `= function (...) {...}` — the same
; "named closure" shape parse.js's acorn extractor treats as a function.
(variable_declarator
  name: (identifier) @name
  value: [(arrow_function) (function_expression)]) @definition.function
