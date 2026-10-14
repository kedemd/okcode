'use strict';
// One generic tree-sitter extractor, data per language — see
// docs/WORKPLAN.md Phase F and docs/DESIGN.md §2/§12 for why.
//
// A language runs ONE query over its parse tree: upstream's own `tags.scm`
// (the query GitHub code navigation uses — most grammars ship one) plus a
// small local override where upstream misses something (grammars/<lang>/tags.local.scm,
// each with its own comment on what and why). The query's captures are
// mapped onto the same symbol shape extractJavaScript (parse.js) returns —
// no AST-walking code is written per language.
//
// Lazy and optional, by construction, not by convention:
//   - `web-tree-sitter` (the WASM runtime) is an optionalDependency. It is
//     require()'d only from ensureLanguage() below, inside a try/catch, and
//     only once a file of a tree-sitter language is actually seen.
//   - The grammar `.wasm` + `tags.scm` this module loads are VENDORED into
//     grammars/<lang>/ (scripts/vendor-grammars.js) and committed — using a
//     language needs no npm install of its (native-build-at-install-time)
//     grammar package, only the runtime above.
//   - Either piece missing → every function here degrades to "unavailable":
//     extractFor() returns null, syntaxDiagnostics() returns []. The caller
//     (parse.js) already has a fallback for "no extractor" — this just takes
//     that path, so a workspace with no grammars installed behaves exactly
//     as it does today.
//
// Sync vs async: EXTRACTORS in parse.js are synchronous — `extract(path,
// src)` is called throughout workspace.js as a plain function, never
// awaited, and making it async would ripple through every ingest call site
// (see DESIGN §6/§7 for how many of those there are). web-tree-sitter's
// Parser.init() and Language.load() are async (they stream-compile WASM).
// The chosen seam: workspace.js's fullScan — the one place rows are read off
// disk before being ingested — awaits `preload()` for the languages about to
// be ingested; by the time ingestOne() calls extract() -> the registered
// tree-sitter extractor, synchronously, the grammar is already resident. A
// file seen through a path that never calls preload() (a library scan, an
// own-write ingest for a language not already loaded this process) just
// gets today's "searchable, no symbol extractor yet" for that one call —
// never wrong, only momentarily under-indexed, and corrected the next time
// fullScan runs.

const fs = require('fs');
const path = require('path');

const GRAMMAR_ROOT = path.join(__dirname, '..', '..', 'grammars');

let manifest = null;
function loadManifest() {
    if (manifest) return manifest;
    try {
        manifest = JSON.parse(fs.readFileSync(path.join(GRAMMAR_ROOT, 'manifest.json'), 'utf8'));
    } catch {
        manifest = {};
    }
    return manifest;
}

// Bump a language's `queryVersion` whenever its tags.local.scm changes
// meaning (not merely wording) — it is folded into `analyzerVersion` below
// so an override fix forces a reparse the same way a grammar upgrade does.
const LANGUAGES = {
    typescript: {
        exts: ['.ts', '.mts', '.cts'],
        tsxExts: ['.tsx'],
        wasm: 'tree-sitter-typescript.wasm',
        tsxWasm: 'tree-sitter-tsx.wasm',
        // Bumped for the decorator-span fix (DESIGN §12): a decorated
        // class/method/property's stored span used to start at the class/
        // method/property itself, not its `@decorator`(s) — every row
        // needs a reparse to pick up the wider span. Bumped again for the
        // full-signature fix (DESIGN §12): `signature` used to be just the
        // definition's first source line, truncating a multi-line parameter
        // list.
        queryVersion: 3,
        importQueryVersion: 1,
        comment: { line: '//', blockOpen: '/*', blockClose: '*/' },
        // `export` wraps the declaration one level up (`export class C {}`
        // parses as `export_statement > class_declaration`); the arrow/
        // function-expression override's definition node is the
        // `variable_declarator`, one level under the `lexical_declaration`
        // that `export_statement` wraps instead.
        isExported(defNode) {
            return hasAncestorType(defNode, 'export_statement', 2);
        },
        // A class's own `@decorator`(s) are already children of
        // `class_declaration` (the grammar parses them INTO the class
        // node), so its span is already right without this; a method's or
        // property's `@decorator`(s) are PRECEDING SIBLINGS inside
        // `class_body` instead — those need the span extended backward.
        decorators: { siblingTypes: ['decorator'] },
    },
    python: {
        exts: ['.py'],
        wasm: 'tree-sitter-python.wasm',
        // Bumped for the decorator-span fix, then again for the
        // full-signature fix — see the typescript entry above.
        queryVersion: 3,
        importQueryVersion: 1,
        comment: { line: '#' },
        // PEP 8 convention, not syntax: a single leading underscore means
        // "not part of the public API". Only a MODULE-level name is a
        // Python export at all — a method's own leading underscore is
        // about the class's API, not the module's, and acorn's JS
        // extractor draws the same top-level-only line for `exported`.
        isExported(_defNode, name, parent) {
            return !parent && typeof name === 'string' && name.length > 0 && !name.startsWith('_');
        },
        // `@app.route(...)` / `@login_required` above a `def`/`class` parse
        // as sibling `decorator` nodes under a `decorated_definition`
        // wrapper, with the `function_definition`/`class_definition` as
        // its LAST child — the query still captures the inner node (that
        // is what upstream's/local's patterns target), so the real span is
        // the WRAPPER's.
        decorators: { wrapperTypes: ['decorated_definition'] },
    },
    go: {
        exts: ['.go'],
        wasm: 'tree-sitter-go.wasm',
        // Bumped for the full-signature fix — see the typescript entry
        // above.
        queryVersion: 2,
        importQueryVersion: 1,
        comment: { line: '//' },
        // Go's own export rule IS syntax: capitalised identifiers are
        // package-exported, nothing else marks it.
        isExported(_defNode, name) {
            return typeof name === 'string' && /^[A-Z]/.test(name);
        },
    },
    rust: {
        exts: ['.rs'],
        wasm: 'tree-sitter-rust.wasm',
        // Bumped for the decorator-span fix, then again for the
        // full-signature fix — see the typescript entry above.
        queryVersion: 3,
        importQueryVersion: 1,
        comment: { line: '//' },
        isExported(defNode) {
            // `pub`/`pub(crate)`/… is a direct child of the item node
            // itself (`(visibility_modifier) (fn_item) ...`), not an
            // ancestor — unlike TypeScript's `export`, which wraps.
            for (let i = 0; i < defNode.childCount; i++) {
                if (defNode.child(i).type === 'visibility_modifier') return true;
            }
            return false;
        },
        // `#[derive(Debug)]` / `#[test]` are PRECEDING SIBLINGS of the
        // item they decorate (`attribute_item` nodes in the same
        // `source_file`/`declaration_list`), never a wrapper — unlike
        // Python's `decorated_definition`.
        decorators: { siblingTypes: ['attribute_item'] },
    },
    php: {
        exts: ['.php', '.phtml'],
        wasm: 'tree-sitter-php.wasm',
        // Bumped for the full-signature fix — see the typescript entry
        // above.
        queryVersion: 2,
        importQueryVersion: 1,
        comment: { line: '//', blockOpen: '/*', blockClose: '*/' },
        // PHP's own visibility IS the export rule for a class member: no
        // modifier at all defaults to public, same as an explicit one;
        // only `private`/`protected` hide it. A namespace-level symbol
        // (no parent — function/class/interface/trait/enum/constant) has
        // no visibility concept to hide behind at all, the same reason Go's
        // capitalisation convention and Rust's `pub` check both only apply
        // where there IS a modifier to look for.
        isExported(defNode, _name, parent) {
            if (!parent) return true;
            for (let i = 0; i < defNode.childCount; i++) {
                const c = defNode.child(i);
                if (c.type === 'visibility_modifier') return c.text === 'public';
            }
            return true;
        },
        // `#[Route(...)]` is a FIELD of the class/method/function node
        // itself (`attributes:`), not a sibling or a wrapper — the
        // grammar already parses it INTO the node, so `node.startIndex`
        // sits at the `#[` with nothing to extend. What needs adjusting
        // instead is the SIGNATURE, which must skip the leading
        // `attribute_list` to find where the def itself actually starts.
        decorators: { leadingChildTypes: ['attribute_list'] },
    },
};

const EXT_TO_LANG = new Map();
for (const [id, cfg] of Object.entries(LANGUAGES)) {
    for (const e of cfg.exts) EXT_TO_LANG.set(e, id);
    for (const e of cfg.tsxExts || []) EXT_TO_LANG.set(e, id);
}

function extOf(p) {
    const m = /\.[^./\\]+$/.exec(String(p));
    return m ? m[0].toLowerCase() : '';
}

// The language this path belongs to, or null — independent of parse.js's
// langOf() on purpose: this module must not require parse.js (parse.js is
// the one that requires THIS, to register the extractors).
function langIdForPath(p) {
    return EXT_TO_LANG.get(extOf(p)) || null;
}

function wasmFileFor(cfg, p) {
    const e = extOf(p);
    if (cfg.tsxExts && cfg.tsxExts.includes(e)) return cfg.tsxWasm;
    return cfg.wasm;
}

function hasAncestorType(node, type, maxUp = 4) {
    let n = node.parent;
    for (let i = 0; n && i < maxUp; i++, n = n.parent) {
        if (n.type === type) return true;
    }
    return false;
}

// ── runtime (lazy, memoized, never thrown out of) ──────────────────────────

let runtime; // undefined: not tried yet. null: tried, unavailable. object: loaded.
let initPromise = null;
const grammarCache = new Map(); // langId -> { parser, query, version } | null (tried, unavailable)
const grammarLoading = new Map(); // langId -> in-flight Promise

// Overridable only by tests (_resetForTest below) — there is no runtime
// config for this. Lets a test prove "the optional dependency is absent"
// deterministically, without uninstalling it from this process's
// node_modules.
let runtimeLoader = () => require('web-tree-sitter');

function requireRuntimeOnce() {
    if (runtime !== undefined) return runtime;
    try {
        runtime = runtimeLoader();
    } catch {
        runtime = null;
    }
    return runtime;
}

// Test-only seam: forces every memoized load back to "not tried yet" and
// optionally swaps in a fake runtime loader (e.g. one that throws, to
// simulate `web-tree-sitter` being absent). Never called outside tests.
function _resetForTest({ loader } = {}) {
    runtime = undefined;
    initPromise = null;
    grammarCache.clear();
    grammarLoading.clear();
    runtimeLoader = loader || (() => require('web-tree-sitter'));
}

async function ensureParserInit() {
    const wts = requireRuntimeOnce();
    if (!wts) return null;
    if (!initPromise) initPromise = wts.Parser.init().then(() => wts, () => null);
    return initPromise;
}

// Combined query text for a language: upstream's tags.scm, then the local
// override — later patterns are not given priority BY the query engine
// (every pattern just matches independently); the override's effect comes
// entirely from the merge-by-range step in buildSymbols(), not from file
// order here.
function queryTextFor(id) {
    const dir = path.join(GRAMMAR_ROOT, id);
    const upstream = fs.readFileSync(path.join(dir, 'tags.scm'), 'utf8');
    let local = '';
    try {
        local = fs.readFileSync(path.join(dir, 'tags.local.scm'), 'utf8');
    } catch {
        /* no override for this language */
    }
    return `${upstream}\n${local}`;
}

// F3's import/export query, per language (grammars/<lang>/imports.scm) —
// entirely optional: a language with no file here just gets `imports: []`,
// same "degrade, never error" contract every other missing piece in this
// module already keeps.
function importQueryTextFor(id) {
    try {
        return fs.readFileSync(path.join(GRAMMAR_ROOT, id, 'imports.scm'), 'utf8');
    } catch {
        return null;
    }
}

// Load (once) and cache the parser+query for one language id. Resolves to
// null — never rejects — when the runtime, the vendored wasm or the query
// fails for any reason: a broken grammar must degrade exactly like an
// absent one, not take a scan down.
async function ensureLanguage(id) {
    if (grammarCache.has(id)) return grammarCache.get(id);
    if (grammarLoading.has(id)) return grammarLoading.get(id);
    const cfg = LANGUAGES[id];
    if (!cfg) return null;
    const promise = (async () => {
        const wts = await ensureParserInit();
        if (!wts) return null;
        try {
            const man = loadManifest()[id];
            const wasmFile = cfg.wasm; // the .tsx wasm is loaded lazily per-call below
            const language = await wts.Language.load(path.join(GRAMMAR_ROOT, id, wasmFile));
            const parser = new wts.Parser();
            parser.setLanguage(language);
            const queryText = queryTextFor(id);
            const query = new wts.Query(language, queryText);
            // The imports query is optional per language (importQueryTextFor
            // returns null when grammars/<lang>/imports.scm does not exist)
            // and its OWN version bump (bump cfg.importQueryVersion, not
            // cfg.queryVersion, to avoid forcing a reparse of every file
            // when only the import side changed) folds into analyzerVersion
            // the same way the tags query's does.
            const importQueryText = importQueryTextFor(id);
            const importQuery = importQueryText ? new wts.Query(language, importQueryText) : null;
            const version = `${(man && man.version) || '0'}:q${cfg.queryVersion}.${cfg.importQueryVersion || 0}`;
            let tsxParser = null;
            let tsxQuery = null;
            let tsxImportQuery = null;
            if (cfg.tsxWasm) {
                // A Query is compiled against ONE Language object — reusing
                // `query` (compiled against the plain .ts grammar) on a tree
                // parsed by the .tsx grammar matches nothing: same node TYPE
                // NAMES, different Language instance, different internal ids.
                const tsxLanguage = await wts.Language.load(path.join(GRAMMAR_ROOT, id, cfg.tsxWasm));
                tsxParser = new wts.Parser();
                tsxParser.setLanguage(tsxLanguage);
                tsxQuery = new wts.Query(tsxLanguage, queryText);
                if (importQueryText) tsxImportQuery = new wts.Query(tsxLanguage, importQueryText);
            }
            return {
                parser,
                tsxParser,
                query,
                tsxQuery,
                importQuery,
                tsxImportQuery,
                version,
                Query: wts.Query,
                language,
            };
        } catch {
            return null;
        }
    })();
    grammarLoading.set(id, promise);
    const result = await promise;
    grammarLoading.delete(id);
    grammarCache.set(id, result);
    return result;
}

// Called ahead of a batch of synchronous extract() calls — see the module
// comment for why this is the seam. Distinct language ids only; already-
// loaded (or already-failed) languages resolve immediately.
async function preload(paths) {
    const ids = new Set();
    for (const p of paths || []) {
        const id = langIdForPath(p);
        if (id) ids.add(id);
    }
    await Promise.all([...ids].map(ensureLanguage));
}

// The parser+query pair to use for THIS path — the .tsx wasm (and its own
// compiled Query, see ensureLanguage above) differs from .ts/.mts/.cts even
// though both are the 'typescript' language id.
function parserFor(grammar, cfg, p) {
    if (cfg.tsxExts && cfg.tsxExts.includes(extOf(p)) && grammar.tsxParser) return grammar.tsxParser;
    return grammar.parser;
}
function queryFor(grammar, cfg, p) {
    if (cfg.tsxExts && cfg.tsxExts.includes(extOf(p)) && grammar.tsxQuery) return grammar.tsxQuery;
    return grammar.query;
}
function importQueryFor(grammar, cfg, p) {
    if (cfg.tsxExts && cfg.tsxExts.includes(extOf(p))) return grammar.tsxImportQuery;
    return grammar.importQuery;
}

// ── symbol extraction ───────────────────────────────────────────────────

// Which captured kind wins when two patterns match the exact same byte
// range (Rust's impl methods are matched by three different patterns; Go's
// receiver override duplicates upstream's own method pattern). Earlier in
// this list wins; a kind not listed here ties with everything by position.
const KIND_PRIORITY = ['method', 'function', 'class', 'interface', 'enum', 'type', 'constant', 'module'];
function kindRank(kind) {
    const i = KIND_PRIORITY.indexOf(kind);
    return i === -1 ? KIND_PRIORITY.length : i;
}

// A @doc capture is either a string node (Python's docstring override) or
// one-or-more comment nodes (Go's upstream doc query; a future override for
// another language). Several lines of `//` comment join into one doc, each
// stripped of its own marker first — stripping the JOINED text would eat
// the blank line between unrelated "//" prefixes instead of each one's own.
function cleanDocText(rawTexts, style) {
    const parts = rawTexts.map((raw) => {
        const s = String(raw).trim();
        if (/^["'`]/.test(s)) return stripQuotes(s);
        if (!style) return s;
        return s
            .split('\n')
            .map((l) => {
                const t = l.trim();
                if (style.line && t.startsWith(style.line)) return t.replace(new RegExp(`^${escapeRe(style.line)}!?\\s?`), '');
                if (style.blockOpen && t.startsWith(style.blockOpen))
                    return t
                        .replace(new RegExp(`^${escapeRe(style.blockOpen)}\\*?\\s?`), '')
                        .replace(new RegExp(`\\s?${escapeRe(style.blockClose)}$`), '');
                return t.replace(/^\*\s?/, '');
            })
            .join(' ')
            .trim();
    });
    return parts.join(' ').trim().slice(0, 400);
}

function stripQuotes(text) {
    const s = String(text || '');
    for (const q of ['"""', "'''", '"', "'", '`']) {
        if (s.startsWith(q) && s.endsWith(q) && s.length >= q.length * 2) return s.slice(q.length, -q.length).trim();
    }
    return s.trim();
}

// The comment block directly above a span, in this language's comment
// syntax — the same convention docAbove() in parse.js applies for
// JavaScript, generalised to a per-language `{ line, blockOpen, blockClose }`
// table instead of JS's fixed `//` / `/* */`.
function commentAbove(src, startIndex, prevEnd, style) {
    if (!style) return '';
    const before = src.slice(Math.max(prevEnd, 0), startIndex);
    const lines = before.split('\n').map((l) => l.trim());
    const doc = [];
    for (let i = lines.length - 1; i >= 0; i--) {
        const l = lines[i];
        if (!l) {
            if (doc.length) break;
            continue;
        }
        if (style.line && l.startsWith(style.line)) {
            doc.unshift(l.replace(new RegExp(`^${escapeRe(style.line)}!?/?\\s?`), ''));
            continue;
        }
        if (style.blockClose && (l.endsWith(style.blockClose) || l.startsWith(style.blockOpen) || l.startsWith('*'))) {
            doc.unshift(
                l
                    .replace(new RegExp(`^${escapeRe(style.blockOpen)}+\\s?`), '')
                    .replace(new RegExp(`\\s?${escapeRe(style.blockClose)}+$`), '')
                    .replace(/^\*\s?/, ''),
            );
            continue;
        }
        break;
    }
    return doc.join(' ').trim().slice(0, 400);
}

function escapeRe(s) {
    return String(s).replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// A full signature: the definition's text from its content start up to
// where its BODY begins — not just the first source line, which cuts a
// multi-line parameter list off mid-list (`export async function
// createUser(`). Not a per-language formatter (that is JS-only in parse.js,
// where acorn gives real parameter nodes) — generic across every
// tree-sitter language here: most grammars expose the body as a named
// field (`body:`), so that is tried first; `cfg.bodyTypes` is a small
// per-language data fallback for the rare grammar that does not (checked on
// the node itself, then one level into its children, so a wrapped
// definition — TypeScript's arrow assigned to a `variable_declarator` — is
// still found via the arrow function nested inside it).
const SIGNATURE_MAX = 300;
const DEFAULT_BODY_TYPES = ['statement_block', 'block', 'class_body', 'declaration_list', 'field_declaration_list', 'compound_statement'];

function fieldBody(node) {
    return typeof node.childForFieldName === 'function' ? node.childForFieldName('body') : null;
}

function findBodyStart(node, types) {
    const own = fieldBody(node);
    if (own) return own.startIndex;
    for (let i = 0; i < node.childCount; i++) {
        if (types.includes(node.child(i).type)) return node.child(i).startIndex;
    }
    // One level deeper, for a definition node that is itself a wrapper
    // around the real function/class (TypeScript's `variable_declarator`
    // around an `arrow_function`) — its own body lives on that child, not
    // on this node.
    for (let i = 0; i < node.childCount; i++) {
        const child = node.child(i);
        const nested = fieldBody(child);
        if (nested) return nested.startIndex;
        for (let j = 0; j < child.childCount; j++) {
            if (types.includes(child.child(j).type)) return child.child(j).startIndex;
        }
    }
    return null;
}

// No body found at all (an interface method signature, a bare declaration,
// a type alias) → the whole node, up to `;`/its own end — never a silent
// cut, so a signature that must be cut for length says so with `…`.
function signatureOf(src, contentStart, node, cfg) {
    const bodyStart = findBodyStart(node, (cfg && cfg.bodyTypes) || DEFAULT_BODY_TYPES);
    const end = bodyStart != null ? bodyStart : node.endIndex;
    let text = src
        .slice(contentStart, end)
        .replace(/\s+/g, ' ')
        // A parameter list laid out one per line collapses to `( a, b, )`:
        // close the gaps the line breaks left and drop the trailing comma.
        .replace(/([([]) /g, '$1')
        .replace(/,? ([)\]])/g, '$1')
        .trim()
        // The body's own opener (`{` for a block, `:` for Python) is what
        // stopped the slice above; trim it (and any space before it) so the
        // signature ends cleanly on the parameter list/return type instead.
        .replace(/\s*[{:]\s*$/, '');
    return text.length > SIGNATURE_MAX ? `${text.slice(0, SIGNATURE_MAX)}…` : text;
}

// A decorator/attribute belongs to the symbol it decorates — `read('get_user')`
// on a Python `@app.route(...)` handler should show the route, and an edit
// by symbol name must be able to touch (or must include, when replacing the
// body) the decorator, not just the bare `def`. Three shapes cover every
// language here, all DATA in `cfg.decorators` (see the LANGUAGES table),
// never per-language code:
//   - `wrapperTypes`: the query's own captured node is nested one level
//     under a wrapper whose span is the REAL one (Python's
//     `decorated_definition`, decorator(s) then `function_definition`/
//     `class_definition` as its last child) — climb to the parent.
//   - `siblingTypes`: the decorator(s) are PRECEDING SIBLINGS of the
//     captured node, not an ancestor (TypeScript's `decorator` before a
//     `method_definition`/`public_field_definition` in a `class_body`;
//     Rust's `attribute_item` before a `function_item`/`struct_item` in a
//     `source_file`/`declaration_list`) — walk backward while the
//     immediately preceding sibling matches.
//   - `leadingChildTypes`: the decorator is already INSIDE the captured
//     node's own span (PHP's `attribute_list` is a field of the
//     class/method/function node itself, so `node.startIndex` already
//     sits at the `#[` — nothing to extend), but the SIGNATURE (the def's
//     own first line) must skip past it; this says which leading child(ren)
//     to skip to find where the def itself actually starts.
// A node can combine `siblingTypes`/`wrapperTypes` (adjust `start`) with
// `leadingChildTypes` (adjust `contentStart`, used only for the signature);
// in practice no language needs both today, but nothing here assumes
// otherwise.
function decoratedSpan(node, dec) {
    let start = node.startIndex;
    let contentStart = node.startIndex;
    if (!dec) return { start, contentStart };
    if (dec.wrapperTypes && node.parent && dec.wrapperTypes.includes(node.parent.type)) {
        start = node.parent.startIndex;
    }
    if (dec.siblingTypes) {
        let prev = node.previousSibling;
        while (prev && dec.siblingTypes.includes(prev.type)) {
            start = prev.startIndex;
            prev = prev.previousSibling;
        }
    }
    if (dec.leadingChildTypes) {
        let i = 0;
        let child = node.child(0);
        while (child && dec.leadingChildTypes.includes(child.type)) {
            i++;
            child = node.child(i);
        }
        if (child) contentStart = child.startIndex;
    }
    return { start, contentStart };
}

// tree-sitter marks a syntax error as either an ERROR node (unexpected
// input) or a MISSING node (expected input that never arrived) — both are
// walked here once per parse. Used by both buildSymbols (to report
// `parsed: false` honestly) and validate.js's syntax gate (F2).
function collectSyntaxErrors(tree) {
    const errors = [];
    const cursor = tree.walk();
    function visit() {
        const node = cursor.currentNode;
        if (node.type === 'ERROR' || node.isMissing) {
            errors.push({ start: node.startIndex, end: node.endIndex || node.startIndex + 1, isMissing: !!node.isMissing });
        }
        if (cursor.gotoFirstChild()) {
            do {
                visit();
            } while (cursor.gotoNextSibling());
            cursor.gotoParent();
        }
    }
    visit();
    return errors;
}

// The one generic query -> symbols mapping, run for every tree-sitter
// language alike. `cfg` supplies only data: exts, comment style, the
// `isExported` predicate.
function buildSymbols(id, cfg, src, tree, query) {
    const matches = query.matches(tree.rootNode);

    // Raw per-match rows, keyed later by (start,end) of the DEFINITION node
    // — not by match — because the same definition node is often matched
    // more than once (see KIND_PRIORITY above).
    const raw = [];
    const standaloneDocs = []; // @doc with no definition/reference in its own match (Python's docstring override)
    const refs = [];

    for (const m of matches) {
        let def = null;
        let name = null;
        let doc = null;
        let parent = null;
        let ref = null;
        for (const c of m.captures) {
            if (c.name.startsWith('definition.')) def = c;
            else if (c.name === 'name') name = c;
            else if (c.name === 'doc') doc = c;
            else if (c.name === 'parent') parent = c;
            else if (c.name.startsWith('reference.')) ref = c;
        }
        // A capture name can repeat within one match — `(comment)* @doc`
        // yields one capture per comment line when a doc block spans
        // several lines — so every `doc`-named capture is gathered, not
        // just the last.
        const docCaptures = m.captures.filter((c) => c.name === 'doc');
        if (def) {
            raw.push({
                kind: def.name.slice('definition.'.length),
                name: name ? name.node.text : null,
                node: def.node,
                doc: docCaptures.length ? cleanDocText(docCaptures.map((c) => c.node.text), cfg.comment) : null,
                parent: parent ? parent.node.text : null,
            });
        } else if (ref) {
            refs.push({ name: name ? name.node.text : ref.node.text, kind: ref.name.slice('reference.'.length), line: null, node: ref.node });
        } else if (docCaptures.length && !name) {
            const start = Math.min(...docCaptures.map((c) => c.node.startIndex));
            const end = Math.max(...docCaptures.map((c) => c.node.endIndex));
            standaloneDocs.push({ start, end, text: cleanDocText(docCaptures.map((c) => c.node.text), cfg.comment) });
        }
    }

    // Merge same-range matches: pick the highest-priority kind, but keep
    // whichever match actually carried `doc` / `parent` — Go's receiver
    // override and upstream's doc-carrying pattern are two SEPARATE matches
    // of the identical method_declaration node.
    const byRange = new Map();
    for (const d of raw) {
        if (!d.name) continue; // a definition with no @name capture is not addressable
        const key = `${d.node.startIndex}:${d.node.endIndex}`;
        const existing = byRange.get(key);
        if (!existing) {
            byRange.set(key, d);
            continue;
        }
        if (kindRank(d.kind) < kindRank(existing.kind)) existing.kind = d.kind;
        if (!existing.doc && d.doc) existing.doc = d.doc;
        if (!existing.parent && d.parent) existing.parent = d.parent;
    }
    const defs = [...byRange.values()].map((d) => {
        const { start, contentStart } = decoratedSpan(d.node, cfg.decorators);
        return {
            name: d.name,
            kind: d.kind,
            start,
            contentStart,
            end: d.node.endIndex,
            explicitParent: d.parent,
            doc: d.doc,
            node: d.node,
        };
    });

    // Nesting: an explicit @parent capture (Go's receiver, Rust's impl type)
    // wins outright. Otherwise the smallest OTHER definition whose range
    // contains this one, exactly like parse.js's JS extractor.
    defs.sort((a, b) => a.start - b.start || b.end - a.end);
    for (const d of defs) {
        if (d.explicitParent) {
            d.parent = d.explicitParent;
            continue;
        }
        let parent = null;
        for (const other of defs) {
            if (other === d) continue;
            if (other.start <= d.start && other.end >= d.end) {
                if (!parent || other.end - other.start < parent.end - parent.start) parent = other;
            }
        }
        d.parent = parent ? parent.name : null;
    }

    // Doc fallback, in order: the same-match @doc capture (already merged
    // in above); a standalone @doc capture (Python's "first statement of
    // the body" override) owned by its SMALLEST enclosing definition — a
    // class's own docstring and its first method's docstring are both
    // "contained" by the class, but only the method owns the second one;
    // then the comment block directly above the definition, in this
    // language's own comment syntax.
    for (const sd of standaloneDocs) {
        let owner = null;
        for (const d of defs) {
            if (sd.start >= d.start && sd.end <= d.end) {
                if (!owner || d.end - d.start < owner.end - owner.start) owner = d;
            }
        }
        if (owner && !owner.doc) owner.doc = sd.text;
    }
    let prevEnd = 0;
    for (const d of [...defs].sort((a, b) => a.start - b.start)) {
        if (!d.doc) d.doc = commentAbove(src, d.start, prevEnd, cfg.comment) || null;
        prevEnd = d.end;
    }

    const symbols = defs.map((d) => ({
        name: d.name,
        kind: d.kind,
        path: d.parent ? `${d.parent}.${d.name}` : d.name,
        ...(d.parent ? { parent: d.parent } : {}),
        start: d.start,
        end: d.end,
        lineStart: lineOf(src, d.start),
        lineEnd: lineOf(src, d.end),
        signature: signatureOf(src, d.contentStart, d.node, cfg),
        doc: d.doc || '',
        ...(cfg.isExported(d.node, d.name, d.parent) ? { exported: true } : {}),
    }));

    const resolvedRefs = refs.map((r) => ({ name: r.name, kind: r.kind, line: lineOf(src, r.node.startIndex) }));

    return { symbols, references: resolvedRefs };
}

function lineOf(src, offset) {
    let line = 1;
    for (let i = 0; i < offset && i < src.length; i++) if (src[i] === '\n') line++;
    return line;
}

// F3's import/export edges, from grammars/<lang>/imports.scm — ONE generic
// reading, shared by every language, no per-language code here (the
// per-language work is in each imports.scm's capture shape, same split F1
// already drew for symbols).
//
// Capture convention (see grammars/typescript/imports.scm for the full
// rationale): a capture name is `<family>.<role>`, where `role` is `stmt`
// (the grouping key — one edge per statement, even across several
// specifier matches of it), `source` (the raw specifier text), `name` (a
// name bound in THIS file) or `importedName` (its pre-alias name in the
// source, when different). `family` becomes the edge's `kind` verbatim
// ('import', 'reexport', 'modpath', …) — nothing here hardcodes which
// families exist, so a language can introduce its own (Rust's `modpath`
// for a `mod foo;` child-file declaration, which has no source text at
// all) with no change to this function.
function buildImports(src, tree, query) {
    if (!query) return [];
    const matches = query.matches(tree.rootNode);
    const groups = new Map(); // "<family>:<start>:<end>" -> { family, node, source, names: [] }
    for (const m of matches) {
        let stmt = null;
        let family = null;
        let source = null;
        const names = []; // { localCapture, importedCapture } pairs, one per specifier-shaped match
        let local = null;
        let imported = null;
        for (const c of m.captures) {
            const dot = c.name.lastIndexOf('.');
            const role = dot === -1 ? c.name : c.name.slice(dot + 1);
            const fam = dot === -1 ? c.name : c.name.slice(0, dot);
            if (role === 'stmt') {
                stmt = c;
                family = fam;
            } else if (role === 'source') source = c;
            else if (role === 'name') local = c;
            else if (role === 'importedName') imported = c;
        }
        if (!stmt) continue;
        // A specifier match carries @name when it binds a local name (always
        // true for an un-aliased one too, EXCEPT TypeScript's own pattern,
        // whose `alias:` field is optional and absent for `bar` in `{bar}` —
        // there only @importedName fires. Either capture alone is enough to
        // know a specifier was here at all; the other one just defaults to it.
        if (local) names.push({ local: local.node.text, imported: imported ? imported.node.text : local.node.text });
        else if (imported) names.push({ local: imported.node.text, imported: imported.node.text });
        const key = `${family}:${stmt.node.startIndex}:${stmt.node.endIndex}`;
        let g = groups.get(key);
        if (!g) {
            g = { family, node: stmt.node, source: null, names: [] };
            groups.set(key, g);
        }
        if (source && g.source == null) g.source = source.node.text;
        g.names.push(...names);
    }
    return [...groups.values()].map((g) => ({
        local: null,
        from: g.source,
        line: lineOf(src, g.node.startIndex),
        kind: g.family,
        reexport: g.family === 'reexport',
        names: g.names.length ? g.names : null,
    }));
}

// The one entry point parse.js's EXTRACTORS call, per language id. Returns
// null (never throws) when the grammar is not loaded for this process — the
// caller treats that exactly like "no extractor registered".
function extractFor(id, p, src) {
    const grammar = grammarCache.get(id);
    const cfg = LANGUAGES[id];
    if (!grammar || !cfg) return null;
    const parser = parserFor(grammar, cfg, p);
    const tree = parser.parse(src);
    const { symbols, references } = buildSymbols(id, cfg, src, tree, queryFor(grammar, cfg, p));
    const imports = buildImports(src, tree, importQueryFor(grammar, cfg, p));
    // Syntax errors are NOT stored here — that is validate.js's syntaxDiagnostics()
    // below, run fresh on whatever text is being validated (which is not
    // always this same parse: a candidate edit's text never reaches extract()
    // at all until it is committed). Keeping them out of the stored record
    // also keeps a broken file's row the same size as a healthy one's.
    return {
        symbols,
        imports,
        exports: symbols.filter((s) => s.exported && !s.parent).map((s) => s.name),
        references,
        analyzerVersion: grammar.version,
    };
}

// For validate.js (F2): ERROR/MISSING nodes as diagnostics, in the shared
// shape every other validator produces. [] whenever the grammar for this
// path is not loaded — "no diagnostics" is this gate's version of today's
// "searchable, no symbol extractor", not a false "no errors found".
function syntaxDiagnostics(p, text) {
    const id = langIdForPath(p);
    if (!id) return [];
    const grammar = grammarCache.get(id);
    const cfg = LANGUAGES[id];
    if (!grammar || !cfg) return [];
    const parser = parserFor(grammar, cfg, p);
    const tree = parser.parse(text);
    const errors = collectSyntaxErrors(tree);
    return errors.map((e) => {
        const l1 = lineOf(text, e.start);
        const l2 = lineOf(text, e.end);
        return {
            source: 'treesitter-syntax',
            code: e.isMissing ? 'MISSING' : 'ERROR',
            severity: 'error',
            message: e.isMissing ? 'expected syntax is missing' : 'unexpected syntax',
            start: e.start,
            end: e.end,
            line: l1,
            column: colOf(text, e.start),
            endLine: l2,
        };
    });
}

function colOf(src, offset) {
    let col = 1;
    for (let i = offset - 1; i >= 0 && src[i] !== '\n'; i--) col++;
    return col;
}

// Current grammar+query version for a path, from the vendored manifest
// alone — no runtime load needed. Used by workspace.js's restale check so a
// `npm run vendor-grammars` bump (or a tags.local.scm queryVersion bump)
// forces a reparse even for files whose own bytes never changed, the same
// way an okjs extension upgrade already does for ext-analysed files.
function currentVersionFor(p) {
    const id = langIdForPath(p);
    if (!id) return null;
    const cfg = LANGUAGES[id];
    const man = loadManifest()[id];
    // Without the optional runtime the file is indexed symbol-less; saying so
    // in the version keeps that row from re-staling on every scan, and makes
    // installing the runtime later reparse it.
    const none = requireRuntimeOnce() ? '' : ':none';
    return `${(man && man.version) || '0'}:q${cfg.queryVersion}.${cfg.importQueryVersion || 0}${none}`;
}

module.exports = {
    langIdForPath,
    preload,
    ensureLanguage,
    extractFor,
    syntaxDiagnostics,
    currentVersionFor,
    LANGUAGES,
    buildImports,
    _resetForTest,
};
