; Local override, used IN ADDITION to upstream's tags.scm. Upstream only
; knows class/interface/method — no enum, no record, no constructor, and
; no idea a `static final` field is this language's closest thing to a
; constant — measured against the extraction fixture mirroring the other
; languages' own local overrides.

; PHP 8.1-style namespace capture, Java's own flavour: `package a.b.c;` —
; `scoped_identifier` for a dotted package, plain `identifier` for the
; (rare) single-segment one. Captured as `module` so edges.js's Java
; resolver can read it straight off the stored symbols the same way it
; already reads every other kind, no separate config file.
(package_declaration
  [(scoped_identifier) (identifier)] @name) @definition.module

(enum_declaration
  name: (identifier) @name) @definition.enum

; `record Point(int x, int y) {}` — a record is a restricted class; same
; kind, same Class.method containment for the methods a record body adds.
(record_declaration
  name: (identifier) @name) @definition.class

; A constructor shares its class's own name, but upstream has no pattern
; for `constructor_declaration` at all — not even as a bare function.
(constructor_declaration
  name: (identifier) @name) @definition.method

; `static final` field — Java's spelling of a constant (there is no
; separate `const` keyword). Plain instance/static fields are not
; indexed as symbols at all, same choice PHP makes for properties.
; `modifiers` lists `static`/`final` as bare children in SOURCE ORDER
; (conventionally `static` before `final`); matching both, unordered-
; adjacency, is enough — a field missing either keyword is not a match.
(field_declaration
  (modifiers
    "static"
    "final")
  declarator: (variable_declarator
    name: (identifier) @name)) @definition.constant
