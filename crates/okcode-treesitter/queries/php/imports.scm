; Import edges — read generically by treesitter.rs build_imports
; (grouping convention: see queries/typescript/imports.scm). Two families:
; `import.*` for `use` statements (a class/interface/trait/function/const
; brought into this file's namespace) and `require.*` for a literal-string
; `require`/`require_once`/`include`/`include_once` of another file — PHP's
; OWN "import a file directly" construct, which `use` does not do at all
; (`use` only aliases a NAMESPACE path; it never loads a file by itself —
; that is what Composer's autoloader, i.e. edges.js's PSR-4 resolver, is
; for). Only a plain string literal path is captured for `require`/
; `include` — `require __DIR__ . '/x.php'` (a concatenation) cannot be
; resolved without evaluating PHP, so it is simply not matched here and
; produces no edge at all, same "stay silent rather than guess" choice
; Python/Rust/Go's own resolvers make elsewhere in this project.

; use App\Other\Thing;  /  use App\Other\Thing as T; — an UNGROUPED,
; dotted (qualified) name. `@import.source` is the namespace PREFIX
; ("App\Other"), `@import.importedName` is the class's own last segment
; ("Thing") — the same two-piece shape the grouped form below produces,
; so edges.js's PHP resolver only needs ONE way to rebuild the full
; namespace path (`source + '\' + importedName`), not two.
(namespace_use_declaration
  (namespace_use_clause
    (qualified_name
      prefix: (namespace_name) @import.source
      (name) @import.importedName)
    alias: (name)? @import.name)) @import.stmt

; use ArrayAccess;  /  use ArrayAccess as AA; — an UNGROUPED, bare
; (no-namespace) name. No `@import.source` at all: there is no prefix to
; combine with, so the resolver tries the bare name on its own (and,
; correctly, usually fails to resolve — a top-level global class has no
; PSR-4 directory of its own). `.` anchors `@import.importedName` to the
; FIRST named child: `(name)` has no field label here (it is not a field
; of `namespace_use_clause`, just its sole non-alias child), so without
; the anchor the query engine also tries binding it to the ALIAS node
; itself — a second, bogus match with `importedName` holding the alias
; text and no `@import.name` at all.
(namespace_use_declaration
  (namespace_use_clause
    . (name) @import.importedName
    alias: (name)? @import.name)) @import.stmt

; use App\Services\{Mailer, Logger as Log}; — GROUPED form. The shared
; prefix ("App\Services") is a SIBLING of the group, not a field of each
; clause, so it is captured once per match here and treesitter.rs's
; build_imports unions every clause inside the SAME `@import.stmt` range
; (the whole declaration) into one edge with several `names` — exactly
; the shape a non-grouped multi-name `from X import a, b` already has in
; other languages. A clause that is itself qualified inside the group
; (`use App\{Models\User}`) is a deliberate, noted gap: it needs a THIRD
; piece (the clause's own sub-prefix) this shape has no slot for, so it
; is left unmatched — no edge, not a wrong one.
(namespace_use_declaration
  (namespace_name) @import.source
  (namespace_use_group
    (namespace_use_clause
      . (name) @import.importedName
      alias: (name)? @import.name))) @import.stmt

; require 'x.php'; / require_once 'x.php'; / include 'x.php'; /
; include_once 'x.php'; — with or without the optional parens
; (`include('x.php')`), single- or double-quoted (a double-quoted string
; with no interpolation is still `encapsed_string`, same as any OTHER
; string in this grammar — see queries/php/tags.local.scm's sibling
; comment for the same distinction).
(require_expression
  [
    (string (string_content) @require.source)
    (encapsed_string (string_content) @require.source)
    (parenthesized_expression
      [
        (string (string_content) @require.source)
        (encapsed_string (string_content) @require.source)
      ])
  ]) @require.stmt

(require_once_expression
  [
    (string (string_content) @require.source)
    (encapsed_string (string_content) @require.source)
    (parenthesized_expression
      [
        (string (string_content) @require.source)
        (encapsed_string (string_content) @require.source)
      ])
  ]) @require.stmt

(include_expression
  [
    (string (string_content) @require.source)
    (encapsed_string (string_content) @require.source)
    (parenthesized_expression
      [
        (string (string_content) @require.source)
        (encapsed_string (string_content) @require.source)
      ])
  ]) @require.stmt

(include_once_expression
  [
    (string (string_content) @require.source)
    (encapsed_string (string_content) @require.source)
    (parenthesized_expression
      [
        (string (string_content) @require.source)
        (encapsed_string (string_content) @require.source)
      ])
  ]) @require.stmt
