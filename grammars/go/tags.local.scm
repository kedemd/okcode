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
