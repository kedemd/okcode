; Import edges for F3 — read generically by treesitter.js#buildImports
; (grouping convention: see grammars/typescript/imports.scm). Java's own
; `import_declaration` node shape is IDENTICAL for a plain import, a
; wildcard (`import a.b.*;`) and a static import (`import static
; a.b.C.member;`) — only the TEXT tells them apart (a trailing `*`, or a
; leading `static` keyword) — so each family's pattern re-captures the
; SAME node shape and uses a `#match?`/`#not-match?` text predicate on
; the whole statement to pick the right one, rather than guess at the
; grammar's (unlabelled) child structure. `@import.source` always carries
; the FULL dotted path; splitting the trailing class/member segment off
; the package prefix is edges.js's Java resolver, not this query — it
; already has the text, and doing it here would need the two-piece
; prefix/name shape PHP's query uses, for no benefit.

((import_declaration (scoped_identifier) @import.source) @import.stmt
 (#not-match? @import.stmt "\\*\\s*;?\\s*$")
 (#not-match? @import.stmt "^import\\s+static\\b"))

; A single-segment import (`import Foo;` — legal, if unusual, when Foo
; lives in the unnamed/default package) has no dot at all, so upstream's
; `scoped_identifier` never matches it; `identifier` is the bare form.
((import_declaration (identifier) @import.source) @import.stmt
 (#not-match? @import.stmt "\\*\\s*;?\\s*$")
 (#not-match? @import.stmt "^import\\s+static\\b"))

; import a.b.*;  — a whole-package wildcard: edges.js resolves this to
; EVERY class this workspace's package index has under that prefix, not
; one file, the same "no single target" shape Go's resolver already
; returns for a directory import.
((import_declaration (scoped_identifier) @importstar.source) @importstar.stmt
 (#match? @importstar.stmt "\\*\\s*;?\\s*$"))

; import static a.b.C.member;  — `@import.source`'s LAST dotted segment
; is the member, not a class; edges.js's Java resolver has to peel one
; more segment off than a plain import does.
((import_declaration (scoped_identifier) @staticimport.source) @staticimport.stmt
 (#match? @staticimport.stmt "^import\\s+static\\b"))
