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
