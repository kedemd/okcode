'use strict';
// Symbol extraction. acorn turns source into an exact syntax tree; everything
// below decides what counts as a symbol, an import, an export.
//
// A real parser rather than heuristics, because this indexes ARBITRARY
// projects, not just this unusually regular one. Regex extraction survives
// `function foo()` and dies quietly on arrow-function consts, class fields,
// `export * from`, destructured requires, decorators and minified vendor
// files. Subtly-wrong is the worst possible failure mode for a map that
// drives edits: it does not error, it points somewhere else. When acorn
// cannot parse a file it says so with a reason and a position, and the file
// is recorded as unparsed rather than silently half-indexed.

const acorn = require('acorn');
const { extensionFor } = require('./extensions');
// Registers the 'okjs' extension as a side effect — the only registrant
// today. extract() below never mentions OKJS by name; it only ever asks the
// generic registry whether SOMETHING claims this path.
require('./ext-okjs');

const JS = new Set(['.js', '.mjs', '.cjs', '.jsx']);

// Text the index will happily hold and search. Anything not listed is treated
// as opaque: it still gets a file node (path, hash, size), so it is never
// invisible, but its bytes are not carried around.
const TEXT = new Set([
    '.md',
    '.markdown',
    '.txt',
    '.json',
    '.jsonc',
    '.yml',
    '.yaml',
    '.html',
    '.htm',
    '.css',
    '.scss',
    '.xml',
    '.svg',
    '.csv',
    '.ini',
    '.toml',
    '.env',
    '.sh',
    '.bash',
    '.ps1',
    '.sql',
    '.gitignore',
    '.editorconfig',
    '.ok',
]);
// Big files are read for their hash and skipped for their content: one
// minified bundle would otherwise dominate every search in the workspace.
const TEXT_MAX = 512 * 1024;

function extOf(path) {
    const m = /\.[^./\\]+$/.exec(path);
    return m ? m[0].toLowerCase() : '';
}

const LANGS = {
    '.ts': 'typescript',
    '.tsx': 'typescript',
    '.json': 'json',
    '.jsonc': 'json',
    '.md': 'markdown',
    '.markdown': 'markdown',
    '.txt': 'text',
    '.html': 'html',
    '.htm': 'html',
    '.ok': 'html',
    '.css': 'css',
    '.scss': 'css',
    '.yml': 'yaml',
    '.yaml': 'yaml',
    '.sh': 'shell',
    '.bash': 'shell',
    '.ps1': 'powershell',
    '.sql': 'sql',
    '.xml': 'xml',
    '.svg': 'svg',
    '.csv': 'csv',
    '.toml': 'toml',
    '.ini': 'ini',
};

function langOf(path) {
    const e = extOf(path);
    if (JS.has(e)) return 'javascript';
    return LANGS[e] || 'other';
}

// Files whose CONTENTS must never enter the index.
//
// `.env` is text, and an earlier version of this happily read it: the index
// would have held this project's API keys as a row in the database and as
// tokens in a full-text index that an agent can query, from where they would
// have been copied into observations and prompts. Nothing about the graph
// needs a secret's contents, and a search that can return one is a leak with
// a search box on it. The file still gets a node — path, size, hash — because
// "not readable" and "not there" are different answers.
const SECRET = [
    /(^|\/)\.env($|\.)/i,
    /(^|\/)\.(npmrc|netrc|pgpass|htpasswd)$/i,
    /(^|\/)(id_rsa|id_dsa|id_ecdsa|id_ed25519)(\.pub)?$/i,
    /(^|\/)(credentials|secrets?)(\.[\w-]+)?\.(json|ya?ml|ini|txt)$/i,
    /\.(pem|key|p12|pfx|keystore|jks|license)$/i,
];

function isSecret(path) {
    const p = String(path).replace(/\\/g, '/');
    return SECRET.some((re) => re.test(p));
}

// Is this worth carrying the bytes of? Text we can search; everything else
// gets a file node and nothing more. Always a boolean: the workspace compares
// it with `!==` against a row's stored `indexed` flag, and an `undefined` (or a
// LANGS string) there made every opaque file look re-stale on every sync.
function isTextual(path, size) {
    if (isSecret(path)) return false;
    const e = extOf(path);
    return !!(JS.has(e) || TEXT.has(e) || LANGS[e]) && (!size || size <= TEXT_MAX);
}

// The leading block/line comment above a declaration. This codebase explains
// nearly every function in prose directly above it, which makes that text the
// highest-signal thing in the file for "where is X handled" — worth capturing
// as part of the symbol rather than leaving in the body.
function docAbove(src, node, prevEnd) {
    const before = src.slice(prevEnd, node.start);
    const lines = before.split('\n').map((l) => l.trim());
    const doc = [];
    for (let i = lines.length - 1; i >= 0; i--) {
        const l = lines[i];
        if (!l) {
            if (doc.length) break;
            continue;
        }
        if (l.startsWith('//')) {
            doc.unshift(l.replace(/^\/\/\s?/, ''));
            continue;
        }
        if (l.endsWith('*/') || l.startsWith('*') || l.startsWith('/*')) {
            doc.unshift(l.replace(/^\/\*+\s?|\s?\*+\/$|^\*\s?/g, ''));
            continue;
        }
        break;
    }
    return doc.join(' ').trim().slice(0, 400);
}

function signatureOf(src, node, name) {
    const fn =
        node.type === 'FunctionDeclaration'
            ? node
            : node.init && (node.init.type === 'ArrowFunctionExpression' || node.init.type === 'FunctionExpression')
              ? node.init
              : null;
    if (!fn) return null;
    // Drop DEFAULTS, not everything after the first `=`. Splitting on the
    // character turned `{ kind = null, limit = 20 } = {}` into `{ kind` — a
    // signature that is not merely ugly but wrong about the parameter's shape.
    // An AssignmentPattern already separates the two halves; use its left.
    const params = fn.params
        .map((p) => {
            const bare = p.type === 'AssignmentPattern' ? p.left : p;
            return src.slice(bare.start, bare.end).replace(/\s+/g, ' ').trim();
        })
        .join(', ');
    return `${fn.async ? 'async ' : ''}${name}(${params})`;
}

// Parse once, try script then module. sourceType matters: an .mjs parsed as a
// script fails on `import`, which is exactly the honest failure acorn gives
// and a regex would have papered over.
function parseSource(src) {
    let lastErr;
    for (const sourceType of ['script', 'module']) {
        try {
            return acorn.parse(src, { ecmaVersion: 'latest', locations: true, sourceType, allowHashBang: true });
        } catch (err) {
            lastErr = err;
        }
    }
    throw lastErr;
}

// ── extractor registry ──────────────────────────────────────────────────────
// Adding a language is registering a function here, not editing extract().
// An extractor takes (path, src) and returns { symbols, imports, exports };
// symbols need name, kind, lineStart, lineEnd, and may carry signature + doc.
//
// JavaScript is the only one implemented, and acorn only speaks JavaScript.
// The general answer for Rust, Go, C, C++ and the rest is tree-sitter: one
// interface, a grammar per language, error-tolerant (it parses a file that
// does not compile) and incremental. Its cost is native bindings, which is
// the one dependency that reliably hurts on Windows — so it is worth taking
// deliberately, when a non-JS project actually arrives, rather than
// speculatively. Until then every other language is still INDEXED, just as
// searchable text rather than structure; nothing is invisible.
const EXTRACTORS = new Map();

function registerExtractor(lang, fn) {
    EXTRACTORS.set(lang, fn);
}

function extract(path, src) {
    const lang = langOf(path);
    const lines = src == null ? 0 : src.split('\n').length;
    // Opaque: an image, a binary, something too large to carry. It still gets
    // a record so the project's shape is complete — "not searchable" and "not
    // there" are very different answers to give a caller.
    if (src == null) {
        return {
            path,
            lang,
            lines: 0,
            symbols: [],
            imports: [],
            exports: [],
            parsed: false,
            indexed: false,
            reason: isSecret(path)
                ? 'may hold credentials — hashed, never read'
                : 'binary or oversized — hashed, not read',
        };
    }
    // An extension claiming this path outranks the generic lang-keyed
    // extractor below — a `.ok.js` file matches the plain `.js` extension
    // too, and the acorn extractor would otherwise run on it and find
    // nothing (an `export default { ... }` object has no `.id` for acorn's
    // export handling to key off). `analyzerVersion` is stamped even on
    // failure, so a source that never parses does not get re-analysed on
    // every scan — see the workspace's refresh restale check, which compares
    // this against the extension's CURRENT version and forces a reparse
    // only when they disagree.
    const ext = extensionFor(path);
    if (ext) {
        // Defense in depth: ext-okjs.js already fails soft internally, but
        // this call site must not trust every future or third-party
        // extension to keep that same discipline — a checker that throws
        // must never take the whole scan down with it, same guarantee the
        // built-in EXTRACTORS path below gives itself.
        try {
            const { okAnalysis, diagnostics, coverage, regions, fallback, ...envelope } = ext.analyze({
                path,
                source: src,
            });
            // An extension that cannot do its job (an optional analyser not
            // installed) may ask for the generic extractor instead of leaving
            // the file unparsed: a `.ok.js` file is still JavaScript. Its
            // reason rides along so the degraded result is visible as such.
            const generic = fallback && EXTRACTORS.get(lang);
            if (generic) {
                try {
                    return {
                        path,
                        lang,
                        lines,
                        ...generic(path, src),
                        parsed: true,
                        indexed: true,
                        analyzerVersion: ext.version,
                        reason: envelope.reason,
                    };
                } catch (err) {
                    return {
                        path,
                        lang,
                        lines,
                        symbols: [],
                        imports: [],
                        exports: [],
                        parsed: false,
                        indexed: true,
                        analyzerVersion: ext.version,
                        reason: `${envelope.reason}; ${String(err.message).slice(0, 160)}`,
                    };
                }
            }
            return {
                path,
                lang,
                lines,
                ...envelope,
                indexed: true,
                analyzerVersion: ext.version,
                ...(okAnalysis ? { okAnalysis, diagnostics, coverage, regions } : {}),
            };
        } catch (err) {
            return {
                path,
                lang,
                lines,
                symbols: [],
                imports: [],
                exports: [],
                parsed: false,
                indexed: true,
                analyzerVersion: ext.version,
                reason: String(err.message).slice(0, 160),
            };
        }
    }

    const extractor = EXTRACTORS.get(lang);
    if (!extractor) {
        // Searchable, just not structured: markdown, JSON, a runbook, an HTML
        // template, and for now Rust/Go/C/TypeScript too. refs() searches
        // these, find() does not — there are no symbols to find, and
        // pretending otherwise would misrepresent what the index knows.
        return {
            path,
            lang,
            lines,
            symbols: [],
            imports: [],
            exports: [],
            parsed: false,
            indexed: true,
            reason: `${lang} — searchable, no symbol extractor yet`,
        };
    }
    try {
        return { path, lang, lines, ...extractor(path, src), parsed: true, indexed: true };
    } catch (err) {
        return {
            path,
            lang,
            lines,
            symbols: [],
            imports: [],
            exports: [],
            parsed: false,
            indexed: true,
            reason: String(err.message).slice(0, 160),
        };
    }
}

// How far in to keep naming things.
//
// Top level alone was not enough. The largest units in a codebase like this
// one are factory closures — createIndex is 407 lines and RETURNS the object
// holding find, read and edit — and addressing only the closure is addressing
// nothing: you cannot read `find` without reading all 407 lines, and you
// cannot edit it without sending them all back. Three levels reach the method
// on the object a factory returns, and the helper inside that method; past
// there a function is almost always an inline callback with no name worth
// holding.
const MAX_DEPTH = 3;

function extractJavaScript(path, src) {
    const ast = parseSource(src);

    const symbols = [];
    const imports = [];
    const exports = [];

    // Every symbol carries a PATH: `createIndex.find` for a nested one, its
    // bare name for a top-level one. The path is what makes a nested symbol
    // addressable — a name alone cannot separate the four `execute`s in a
    // file, and guessing between them is the one thing this index must not do.
    const push = (name, kind, node, { parent = null, prevEnd = 0, ...extra } = {}) => {
        const symPath = parent ? `${parent}.${name}` : name;
        symbols.push({
            name,
            kind,
            path: symPath,
            ...(parent ? { parent } : {}),
            // The EXACT bytes of the symbol, character-addressed. For a
            // method inside an object literal this is `plus(by) { ... }` —
            // no leading indentation, no trailing comma — because that is
            // where acorn's own node boundary falls. A line range would own
            // the whitespace before it and the punctuation after it, which
            // is exactly what belongs to the enclosing object, not to the
            // method.
            start: node.start,
            end: node.end,
            lineStart: node.loc.start.line,
            lineEnd: node.loc.end.line,
            doc: docAbove(src, node, prevEnd),
            ...extra,
        });
        return symPath;
    };

    const fnOf = (n) =>
        n &&
        (n.type === 'FunctionDeclaration' || n.type === 'FunctionExpression' || n.type === 'ArrowFunctionExpression')
            ? n
            : null;

    // The function-valued members of an object literal — the shape every
    // factory here returns, and the one that was completely invisible.
    function members(obj, parent, depth) {
        if (!obj || obj.type !== 'ObjectExpression' || depth > MAX_DEPTH) return;
        let prevEnd = obj.start;
        for (const p of obj.properties || []) {
            const name = p.key && (p.key.name || p.key.value);
            const fn = fnOf(p.value);
            if (name && fn) {
                const child = push(name, 'method', p, {
                    parent,
                    prevEnd,
                    signature: signatureOf(src, { init: p.value }, name),
                });
                descend(fn, child, depth + 1);
            }
            prevEnd = p.end;
        }
    }

    // Into a function body: its inner declarations, or the object it returns.
    function descend(fn, parent, depth) {
        if (!fn || !fn.body || depth > MAX_DEPTH) return;
        // const make = () => ({ ... }) — a factory with no block at all.
        if (fn.body.type === 'ObjectExpression') return members(fn.body, parent, depth);
        if (fn.body.type !== 'BlockStatement') return;
        let prevEnd = fn.body.start;
        for (const st of fn.body.body) {
            statement(st, parent, depth, prevEnd);
            prevEnd = st.end;
        }
    }

    // `const a = …, b = () => {}` — one symbol per named declarator. `span` is
    // the node whose bytes the symbol owns: the declaration itself, or the
    // `export …` statement wrapping it, so that editing an exported const by
    // name replaces the `export` keyword along with it (the same way an
    // exported function's span already does). Every declarator of one
    // statement shares that statement's span — splitting a multi-declarator
    // statement into per-declarator byte ranges would hand an edit a range
    // that is not a statement.
    function variables(decl, span, parent, depth, prevEnd, exported = false) {
        for (const d of decl.declarations) {
            if (!d.id || d.id.type !== 'Identifier') continue;
            const init = d.init || {};
            // A top-level require() is an import edge, not a symbol. An
            // EXPORTED one is a binding the file exposes, so it stays a symbol.
            if (
                !parent &&
                !exported &&
                init.type === 'CallExpression' &&
                init.callee &&
                init.callee.name === 'require'
            ) {
                const from = init.arguments[0] && init.arguments[0].value;
                if (from) imports.push({ local: d.id.name, from, line: decl.loc.start.line });
                continue;
            }
            const fn = fnOf(init);
            // Inside a scope only FUNCTIONS earn a name. A local
            // `const rows = []` is not something anyone addresses, and
            // indexing every one of them would bury the things that are.
            if (!fn && parent) continue;
            const child = push(d.id.name, fn ? 'function' : 'const', span, {
                parent,
                prevEnd,
                ...(fn ? { signature: signatureOf(src, d, d.id.name) } : {}),
                ...(exported ? { exported: true } : {}),
            });
            if (fn) descend(fn, child, depth + 1);
        }
    }

    // The names a binding pattern introduces: `{ a, b: c, ...d }` → a, c, d.
    function boundNames(p, out = []) {
        if (!p) return out;
        if (p.type === 'Identifier') out.push(p.name);
        else if (p.type === 'ObjectPattern')
            for (const q of p.properties) boundNames(q.type === 'RestElement' ? q.argument : q.value, out);
        else if (p.type === 'ArrayPattern') for (const q of p.elements) boundNames(q, out);
        else if (p.type === 'RestElement') boundNames(p.argument, out);
        else if (p.type === 'AssignmentPattern') boundNames(p.left, out);
        return out;
    }

    // One statement, at any depth. `parent` null means top level, which is the
    // only place imports, exports and module.exports can appear.
    function statement(node, parent, depth, prevEnd) {
        if (node.type === 'FunctionDeclaration' && node.id) {
            const child = push(node.id.name, 'function', node, {
                parent,
                prevEnd,
                signature: signatureOf(src, node, node.id.name),
            });
            descend(node, child, depth + 1);
            return;
        }
        if (node.type === 'ClassDeclaration' && node.id) {
            const child = push(node.id.name, 'class', node, { parent, prevEnd });
            for (const m of node.body.body || []) {
                const name = m.key && (m.key.name || m.key.value);
                if (!name || depth + 1 > MAX_DEPTH) continue;
                push(name, 'method', m, { parent: child, signature: signatureOf(src, { init: m.value }, name) });
                descend(m.value, `${child}.${name}`, depth + 2);
            }
            return;
        }
        if (node.type === 'VariableDeclaration') {
            variables(node, node, parent, depth, prevEnd);
            return;
        }
        if (parent) {
            // The factory's product: `return { find() {...}, read() {...} }`.
            if (node.type === 'ReturnStatement') members(node.argument, parent, depth);
            return;
        }
        if (node.type === 'ImportDeclaration') {
            imports.push({ local: null, from: node.source.value, line: node.loc.start.line, esm: true });
        } else if (node.type === 'ExportNamedDeclaration' || node.type === 'ExportDefaultDeclaration') {
            const decl = node.declaration;
            const isDefault = node.type === 'ExportDefaultDeclaration';
            if (decl && decl.type === 'VariableDeclaration') {
                // `export const X = …` — the same symbols a bare `const`
                // would produce, spanning the whole export statement.
                variables(decl, node, null, depth, prevEnd, true);
                for (const d of decl.declarations) exports.push(...boundNames(d.id));
            } else if (decl && decl.id) {
                const child = push(decl.id.name, decl.type === 'ClassDeclaration' ? 'class' : 'function', node, {
                    prevEnd,
                    exported: true,
                    ...(decl.type === 'FunctionDeclaration' ? { signature: signatureOf(src, decl, decl.id.name) } : {}),
                });
                descend(decl, child, depth + 1);
                exports.push(isDefault ? 'default' : decl.id.name);
            } else if (isDefault) exports.push('default');
            for (const s of node.specifiers || []) if (s.exported) exports.push(s.exported.name || s.exported.value);
            if (node.source)
                imports.push({
                    local: null,
                    from: node.source.value,
                    line: node.loc.start.line,
                    esm: true,
                    reexport: true,
                });
        } else if (node.type === 'ExportAllDeclaration') {
            // `export * from './m'` / `export * as ns from './m'` — a
            // re-export edge like `export { x } from`; the namespace form
            // also introduces one exported name.
            if (node.exported) exports.push(node.exported.name || node.exported.value);
            imports.push({
                local: null,
                from: node.source.value,
                line: node.loc.start.line,
                esm: true,
                reexport: true,
            });
        } else if (node.type === 'ExpressionStatement') {
            const e = node.expression;
            if (e.type === 'AssignmentExpression' && /^module\.exports/.test(src.slice(e.left.start, e.left.end))) {
                if (e.right.type === 'ObjectExpression') {
                    for (const p of e.right.properties) {
                        const n = p.key && (p.key.name || p.key.value);
                        if (n) exports.push(n);
                    }
                } else if (e.right.type === 'Identifier') exports.push(e.right.name);
                push('module.exports', 'exports', node, { prevEnd, exports: exports.slice() });
            }
        }
    }

    let prevEnd = 0;
    for (const node of ast.body) {
        statement(node, null, 0, prevEnd);
        prevEnd = node.end;
    }

    // Mark the symbols the file actually exposes. Only top-level ones: a
    // method called `find` inside a factory is not the file's exported `find`.
    const exported = new Set(exports);
    for (const s of symbols) if (!s.parent && exported.has(s.name)) s.exported = true;

    return { symbols, imports, exports };
}

registerExtractor('javascript', extractJavaScript);

module.exports = { extract, langOf, extOf, isTextual, isSecret, registerExtractor, TEXT_MAX, parseSource };
