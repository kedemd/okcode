; Local override, used IN ADDITION to upstream's tags.scm. Upstream captures
; a method's @name but never its receiver, so every method's path would
; collapse to its bare name — the four `Area`s of four different structs in
; one file would be indistinguishable. `@parent` is treesitter.js's generic
; "use this name as the symbol's parent instead of computed containment"
; capture: receiver and method live in the same match here, so the receiver
; type is known without any Go-specific code in the extractor itself.
;
; This duplicates upstream's (method_declaration ...) @definition.method
; pattern on purpose — treesitter.js dedupes same-range matches and prefers
; the one that carries a `@parent` capture.
(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: [
        (pointer_type (type_identifier) @parent)
        (type_identifier) @parent
      ]))
  name: (field_identifier) @name) @definition.method

; Package-level `const`/`var` — upstream's own tags.scm already HAS
; `(var_declaration (var_spec name: (identifier) @name))` and the `const`
; equivalent, but neither carries a `@definition.*` capture, so buildSymbols
; (which only looks at `definition.`-prefixed captures) silently drops both;
; they produced no symbol at all. `(source_file (var_declaration ...))`
; restricts the match to a DIRECT child of the file's root — the same node
; type is used for a LOCAL `var x = 1` inside a function body, which must
; stay unaddressed, same rule every other language's "top-level only" bar
; applies. A grouped `const ( A = 1; B = 2 )` is one `const_declaration`
; with several `const_spec` children, each captured (and spanned)
; independently, same as PHP's comma-separated `const_element`s.
(source_file
  (const_declaration
    (const_spec
      name: (identifier) @name) @definition.constant))

(source_file
  (var_declaration
    (var_spec
      name: (identifier) @name) @definition.constant))
