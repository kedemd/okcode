; Import edges for F3 — read generically by treesitter.js#buildImports
; (grouping convention: see grammars/typescript/imports.scm). A C# `using`
; binds a NAMESPACE, not a file — edges.js's resolver maps `@import.source`
; to every file that declares a `namespace` of that exact name (already a
; stored `module`-kind symbol, upstream's own `definition.module` pattern),
; not to a single target. `using A = B.C.D;` is an ALIAS — a local binding
; with nothing upstream's `namespace_declaration` index can resolve it
; against (B.C.D may well be a single TYPE, not a namespace) — kept as its
; own family so edges.js's resolver can leave it alone rather than mis-plug
; it into the namespace index, per the deliverable's documented fallback.
((using_directive [(qualified_name) (identifier)] @import.source) @import.stmt
 (#not-match? @import.stmt "="))

(using_directive
  (identifier) @alias.name
  "="
  [(qualified_name) (identifier)] @alias.source) @alias.stmt
