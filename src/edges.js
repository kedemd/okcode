'use strict';
// F3: resolved import/export edges — turning the `imports` rows every file
// already carries (parse.js, ext-okjs.js, treesitter.js) into an actual
// FILE (and named symbol, when the import names one), so `refs()` can rank
// a hit and `importers(file)` can answer "who uses this" without a text
// scan. DESIGN §12 / WORKPLAN Phase F3.
//
// Everything here is DERIVED from stored rows (files + their `imports` +
// `symbols`) — the one exception is a handful of small project config
// files (tsconfig.json, go.mod, Cargo.toml, pyproject.toml/setup.py) read
// straight off the access facade: they are either not part of the text
// index at all (go.mod's extension is not in parse.js's TEXT set — it is
// metadata about the BUILD, not source a caller searches) or are read far
// less often than every other file would need them re-read for. No new
// okdb type: this module owns a cache, not a store.
//
// Unresolvable stays unresolved, never guessed — an external package
// (`react`, `lodash/debounce`) is exactly as absent from the result as a
// genuinely broken import, because this index cannot tell them apart and
// must not pretend to.

const pathPosix = require('path').posix;

// ── path helpers ────────────────────────────────────────────────────────

function isRelativeSpecifier(spec) {
    return typeof spec === 'string' && (spec.startsWith('.') || spec.startsWith('/'));
}

function normJoin(dir, spec) {
    const raw = spec.startsWith('/') ? spec.slice(1) : pathPosix.join(dir, spec);
    return pathPosix.normalize(raw).replace(/^(\.\.\/)+/, ''); // never escape the workspace root
}

// JS/TS resolution: extension list (DESIGN item 1), then index files —
// package.json "main"/"exports" is deliberately NOT read (the deliverable
// says "package.json-free"): a workspace file is found by name, not by
// another package's own redirection rules.
const JS_EXTS = ['', '.ts', '.tsx', '.d.ts', '.mts', '.cts', '.js', '.mjs', '.cjs', '.jsx'];
const JS_INDEXES = ['index.ts', 'index.tsx', 'index.d.ts', 'index.mts', 'index.cts', 'index.js', 'index.mjs', 'index.cjs', 'index.jsx'];

function resolveJsPath(norm, index) {
    for (const ext of JS_EXTS) if (index.has(norm + ext)) return norm + ext;
    for (const f of JS_INDEXES) {
        const cand = norm ? `${norm}/${f}` : f;
        if (index.has(cand)) return cand;
    }
    return null;
}

// ── JSONC (tsconfig.json) ──────────────────────────────────────────────
// Strip // and /* */ comments without breaking a string that happens to
// contain `//` or `/*` — adcode's string-aware stripper is the reference
// (WORKPLAN F3). Trailing commas (common in hand-edited tsconfigs, invalid
// JSON) are dropped too, after comments are gone.
function stripJsonc(text) {
    let out = '';
    let i = 0;
    const n = text.length;
    while (i < n) {
        const c = text[i];
        if (c === '"' || c === "'") {
            const quote = c;
            out += c;
            i++;
            while (i < n) {
                out += text[i];
                if (text[i] === '\\') {
                    i++;
                    if (i < n) out += text[i++];
                    continue;
                }
                if (text[i] === quote) {
                    i++;
                    break;
                }
                i++;
            }
            continue;
        }
        if (c === '/' && text[i + 1] === '/') {
            while (i < n && text[i] !== '\n') i++;
            continue;
        }
        if (c === '/' && text[i + 1] === '*') {
            i += 2;
            while (i < n && !(text[i] === '*' && text[i + 1] === '/')) i++;
            i += 2;
            continue;
        }
        out += c;
        i++;
    }
    return out.replace(/,(\s*[}\]])/g, '$1');
}

function parseTsconfig(text) {
    try {
        return JSON.parse(stripJsonc(text));
    } catch {
        return null;
    }
}

// Nearest ancestor tsconfig for a file's directory, among the ones this
// workspace parsed — nested tsconfigs apply to their own subtree, so the
// closest one wins, never the root one. `extends` is honoured only when it
// is a single, local, relative string (a "trivial" extends per the
// deliverable) — one hop, no further chain, no package extends.
function nearestTsconfig(fileDir, tsconfigs) {
    let dir = fileDir;
    for (;;) {
        const cfg = tsconfigs.get(dir);
        if (cfg) return cfg;
        if (dir === '' || dir === '.') return null;
        const up = pathPosix.dirname(dir);
        if (up === dir) return null;
        dir = up === '.' ? '' : up;
    }
}

function effectiveCompilerOptions(dir, parsed, tsconfigs) {
    let opts = (parsed && parsed.compilerOptions) || {};
    const ext = parsed && parsed.extends;
    if (typeof ext === 'string' && ext.startsWith('.')) {
        const extDir = pathPosix.dirname(normJoin(dir, ext.replace(/\.json$/, '') + '.json'));
        const extPath = normJoin(dir, ext.endsWith('.json') ? ext : `${ext}.json`);
        const parent = tsconfigs.get(pathPosix.dirname(extPath));
        // Only a direct hit on an already-parsed tsconfig counts as
        // "trivial" — anything requiring its own resolution chase is left
        // unresolved rather than half-guessed.
        if (parent && parent.raw && pathPosix.basename(extPath) === pathPosix.basename(parent.path || '')) {
            opts = { ...parent.raw.compilerOptions, ...opts };
        }
    }
    return opts;
}

// ── workspace-wide config, parsed once per generation ───────────────────

const CONFIG_BASENAMES = new Set(['tsconfig.json', 'go.mod', 'Cargo.toml', 'pyproject.toml', 'setup.py', 'composer.json']);

// Minimal TOML reads — just the one key each format needs. A real TOML/
// go.mod parser is not worth a dependency for "the first `module` line" /
// "is there a [project] or [tool.poetry] table at all".
function goModuleName(text) {
    const m = /^\s*module\s+(\S+)/m.exec(String(text || ''));
    return m ? m[1] : null;
}
function hasProjectTable(text) {
    return /^\s*\[(project|tool\.poetry|build-system)\]/m.test(String(text || ''));
}

// composer.json IS plain JSON (unlike go.mod/Cargo.toml/pyproject.toml) —
// just `autoload.psr-4`, the one table PHP resolution needs: NAMESPACE
// PREFIX -> one directory or several, relative to this composer.json's
// own folder. `autoload-dev` (test-only autoloading) is deliberately not
// read here — the same "only what resolution needs" scope every other
// config reader above keeps.
function psr4RootsFrom(dir, body) {
    const out = [];
    let json;
    try {
        json = JSON.parse(body);
    } catch {
        return out; // malformed composer.json — no PSR-4 roots from it, not a crash
    }
    const psr4 = (json && json.autoload && json.autoload['psr-4']) || {};
    for (const [prefix, target] of Object.entries(psr4)) {
        const normPrefix = prefix.endsWith('\\') ? prefix : `${prefix}\\`;
        for (const t of Array.isArray(target) ? target : [target]) {
            if (typeof t !== 'string') continue;
            const targetDir = normJoin(dir, t).replace(/\/$/, '');
            out.push({ prefix: normPrefix, dir: targetDir });
        }
    }
    return out;
}

async function readConfigs(filesList, access) {
    const wanted = filesList.filter((f) => CONFIG_BASENAMES.has(pathPosix.basename(f.rel)));
    const text = new Map();
    if (wanted.length) {
        let got;
        try {
            got = await access.read(wanted.map((f) => f.rel));
        } catch {
            got = new Map();
        }
        for (const f of wanted) {
            const buf = got.get(f.rel);
            if (buf != null) text.set(f.rel, buf.toString('utf8'));
        }
    }
    const tsconfigs = new Map(); // dir -> { raw, options, path }
    const goModules = []; // { dir, name } — dir is the module root
    const pyRoots = new Set(); // directories treated as import roots
    const phpRoots = []; // { prefix, dir } — PSR-4 namespace prefix -> directory
    for (const [rel, body] of text) {
        const dir = pathPosix.dirname(rel) === '.' ? '' : pathPosix.dirname(rel);
        const base = pathPosix.basename(rel);
        if (base === 'tsconfig.json') {
            const raw = parseTsconfig(body);
            if (raw) tsconfigs.set(dir, { raw, path: rel });
        } else if (base === 'go.mod') {
            const name = goModuleName(body);
            if (name) goModules.push({ dir, name });
        } else if ((base === 'pyproject.toml' && hasProjectTable(body)) || base === 'setup.py') {
            pyRoots.add(dir);
            const src = `${dir ? `${dir}/` : ''}src`;
            if (filesList.some((f) => f.rel.startsWith(`${src}/`))) pyRoots.add(src);
        } else if (base === 'composer.json') {
            phpRoots.push(...psr4RootsFrom(dir, body));
        }
    }
    // Resolve each tsconfig's `compilerOptions` (with its own one-hop
    // `extends`) now that every tsconfig in the workspace is loaded.
    for (const [dir, cfg] of tsconfigs) cfg.options = effectiveCompilerOptions(dir, cfg.raw, tsconfigs);
    // Python package roots: the parent of every TOP-LEVEL `__init__.py`
    // directory (a dir with `__init__.py` whose parent does not also have
    // one) — plus the pyproject/setup.py project dirs and their `src/`
    // above, plus the workspace root itself as the universal fallback.
    const initDirs = new Set();
    for (const f of filesList) if (pathPosix.basename(f.rel) === '__init__.py') initDirs.add(pathPosix.dirname(f.rel));
    for (const dir of initDirs) {
        let d = dir;
        while (d && initDirs.has(pathPosix.dirname(d) === '.' ? '' : pathPosix.dirname(d))) {
            d = pathPosix.dirname(d) === '.' ? '' : pathPosix.dirname(d);
        }
        const parent = pathPosix.dirname(d) === '.' ? '' : pathPosix.dirname(d);
        pyRoots.add(parent);
    }
    pyRoots.add('');
    // Longest prefix first (see resolvePhpFqn's "most specific root" rule) —
    // sorted once here rather than on every resolveFile call.
    phpRoots.sort((a, b) => b.prefix.length - a.prefix.length);
    return { tsconfigs, goModules, pyRoots: [...pyRoots], phpRoots };
}

// ── TypeScript: baseUrl / paths ─────────────────────────────────────────

// `paths` patterns carry one optional `*`; a match substitutes it into
// every target pattern's own `*` and tries each candidate in order —
// TypeScript's own resolution order, simplified to what a workspace file
// lookup needs (no node_modules fallback).
function resolveTsPaths(spec, fileDir, tsconfigs, index) {
    const cfg = nearestTsconfig(fileDir, tsconfigs);
    if (!cfg || !cfg.options) return null;
    const baseDir = cfg.options.baseUrl ? normJoin(pathPosix.dirname(cfg.path), cfg.options.baseUrl) : pathPosix.dirname(cfg.path);
    const paths = cfg.options.paths || {};
    for (const [pattern, targets] of Object.entries(paths)) {
        const star = pattern.indexOf('*');
        let captured = null;
        if (star === -1) {
            if (spec !== pattern) continue;
            captured = '';
        } else {
            const prefix = pattern.slice(0, star);
            const suffix = pattern.slice(star + 1);
            if (!spec.startsWith(prefix) || !spec.endsWith(suffix) || spec.length < prefix.length + suffix.length) continue;
            captured = spec.slice(prefix.length, spec.length - suffix.length);
        }
        for (const target of targets || []) {
            const candidate = target.replace('*', captured);
            const found = resolveJsPath(normJoin(baseDir, candidate), index);
            if (found) return found;
        }
    }
    // `baseUrl` alone (no matching `paths` entry) also lets a NON-relative
    // specifier resolve against it — TypeScript's own fallback.
    if (cfg.options.baseUrl && !isRelativeSpecifier(spec)) {
        const found = resolveJsPath(normJoin(baseDir, spec), index);
        if (found) return found;
    }
    return null;
}

// ── Python ───────────────────────────────────────────────────────────────

function pyDottedToPath(root, parts) {
    return root ? `${root}/${parts.join('/')}` : parts.join('/');
}

// A dotted module path resolved to a FILE, trying (in order) "it's a
// plain module", "it's a package" — never both, a workspace cannot have
// `a/b.py` and `a/b/__init__.py` mean the same import at once in practice,
// and trying the file form first matches Python's own precedence.
function resolvePyModule(root, parts, index) {
    if (!parts.length) return index.has(`${root || ''}${root ? '/' : ''}__init__.py`) ? `${root ? `${root}/` : ''}__init__.py` : null;
    const base = pyDottedToPath(root, parts);
    if (index.has(`${base}.py`)) return `${base}.py`;
    if (index.has(`${base}/__init__.py`)) return `${base}/__init__.py`;
    return null;
}

function parsePyRelative(text) {
    const m = /^(\.+)(.*)$/.exec(text);
    if (!m) return null; // absolute — no leading dot
    const parts = m[2] ? m[2].split('.').filter(Boolean) : [];
    return { level: m[1].length, parts };
}

// `from X import a, b as c` — for EACH name, try "a is a symbol defined in
// module X" first (the common case), then "a is itself a submodule of
// package X" (X/a.py) — the two things a trailing name in a from-import
// can mean, and only the resolver, not the query, can tell them apart.
function resolvePyFromImport(fromRel, fromText, names, pyRoots, index) {
    const rel = parsePyRelative(fromText);
    let moduleFile = null;
    let moduleDir = null;
    if (rel) {
        // Level 1 = the importing file's OWN package directory; each extra
        // dot climbs one more directory, same as Python's own semantics.
        let dir = pathPosix.dirname(fromRel);
        for (let i = 1; i < rel.level; i++) dir = pathPosix.dirname(dir) === '.' ? '' : pathPosix.dirname(dir);
        if (dir === '.') dir = '';
        moduleFile = resolvePyModule(dir, rel.parts, index);
        moduleDir = rel.parts.length ? pyDottedToPath(dir, rel.parts) : dir;
    } else {
        const parts = fromText.split('.').filter(Boolean);
        for (const root of pyRoots) {
            const found = resolvePyModule(root, parts, index);
            if (found) {
                moduleFile = found;
                moduleDir = pyDottedToPath(root, parts);
                break;
            }
        }
    }
    const out = [];
    if (!names) {
        // `from X import *` — one edge for the module itself, no per-name
        // resolution possible (the wildcard's own names are not listed here).
        if (moduleFile) out.push({ to: moduleFile, name: null });
        return out;
    }
    for (const { local, imported } of names) {
        if (moduleFile) {
            out.push({ to: moduleFile, name: imported });
            continue;
        }
        // The module itself didn't resolve as a file — try the name as a
        // SUBMODULE of it instead (`from pkg import sub` where sub is
        // pkg/sub.py, not a symbol inside pkg/__init__.py).
        if (moduleDir != null) {
            const sub = resolvePyModule(moduleDir, [imported], index);
            if (sub) out.push({ to: sub, name: null });
        }
    }
    return out;
}

// ── Go ───────────────────────────────────────────────────────────────────

function resolveGoImport(fromSpec, goModules, index) {
    let best = null;
    for (const mod of goModules) {
        if (fromSpec === mod.name || fromSpec.startsWith(`${mod.name}/`)) {
            if (!best || mod.name.length > best.name.length) best = mod;
        }
    }
    if (!best) return { toFiles: [] }; // not this workspace's module — external
    const rest = fromSpec.slice(best.name.length).replace(/^\//, '');
    const dir = best.dir ? (rest ? `${best.dir}/${rest}` : best.dir) : rest;
    const files = [...index].filter(
        (p) => pathPosix.dirname(p) === (dir || '.') && p.endsWith('.go') && !p.endsWith('_test.go'),
    );
    return { toFiles: files };
}

// ── Rust ───────────────────────────────────────────────────────────────
// A crate's module tree, built from `mod` declarations (the `modpath`
// family in grammars/rust/imports.scm) starting at its root file
// (`src/main.rs` or `src/lib.rs`) — the same shape rustc itself resolves,
// simplified to what mapping `crate::a::b` to a FILE needs: no `#[path]`
// attribute overrides, no `mod a { mod b {...} }` inline nesting (those
// already live in their declaring file, so `super::`/`self::` inside them
// resolve to that file, not a child — a conservative choice, noted in
// docs/DESIGN.md).
function findRustCrates(filesList) {
    const index = new Set(filesList.map((f) => f.rel));
    const crates = [];
    for (const root of ['src/main.rs', 'src/lib.rs']) {
        if (index.has(root)) crates.push({ root, dir: 'src' });
    }
    // A workspace with crates nested under subdirectories (`crates/foo/src/lib.rs`).
    for (const f of filesList) {
        if (/\/src\/(main|lib)\.rs$/.test(f.rel) && f.rel !== 'src/main.rs' && f.rel !== 'src/lib.rs') {
            crates.push({ root: f.rel, dir: pathPosix.dirname(f.rel) });
        }
    }
    return crates;
}

function buildRustModuleTree(crate, byRel, index) {
    // file -> module path segments from the crate root; modpath -> file.
    const fileToPath = new Map([[crate.root, []]]);
    const pathToFile = new Map([['', crate.root]]);
    const queue = [crate.root];
    while (queue.length) {
        const file = queue.shift();
        const segs = fileToPath.get(file);
        const row = byRel.get(file);
        const children = ((row && row.imports) || []).filter((i) => i.kind === 'modpath');
        // A non-root module file's OWN children live under a directory
        // named after it (`a.rs` -> `a/`), unless it IS already that
        // directory's `mod.rs`/root — both forms are supported.
        const base = pathPosix.basename(file).replace(/\.rs$/, '');
        const childDir =
            file === crate.root || base === 'mod'
                ? pathPosix.dirname(file)
                : `${pathPosix.dirname(file)}/${base}`;
        for (const c of children) {
            for (const { local: name } of c.names || []) {
                if (!name) continue;
                const childFile = index.has(`${childDir}/${name}.rs`)
                    ? `${childDir}/${name}.rs`
                    : index.has(`${childDir}/${name}/mod.rs`)
                      ? `${childDir}/${name}/mod.rs`
                      : null;
                if (!childFile || fileToPath.has(childFile)) continue;
                const childSegs = [...segs, name];
                fileToPath.set(childFile, childSegs);
                pathToFile.set(childSegs.join('::'), childFile);
                queue.push(childFile);
            }
        }
    }
    return { fileToPath, pathToFile };
}

function resolveRustPath(fromFile, text, tree) {
    const segs = text.split('::').filter(Boolean);
    if (!segs.length) return null;
    const head = segs[0];
    let base;
    let rest;
    if (head === 'crate') {
        base = [];
        rest = segs.slice(1);
    } else if (head === 'self') {
        base = tree.fileToPath.get(fromFile) || [];
        rest = segs.slice(1);
    } else if (head === 'super') {
        const own = tree.fileToPath.get(fromFile) || [];
        base = own.slice(0, -1);
        rest = segs.slice(1);
    } else {
        return null; // an external crate / std — not this workspace
    }
    // Try the full remaining path as a module first (`use crate::a::b;`
    // importing the module itself), then drop the last segment (the
    // common case: `use crate::a::b::C;` where C is an ITEM in module
    // `a::b`, not a module of its own).
    for (const n of [rest.length, rest.length - 1]) {
        if (n < 0) continue;
        const key = [...base, ...rest.slice(0, n)].join('::');
        const file = tree.pathToFile.get(key);
        if (file) return { to: file, name: n < rest.length ? rest[rest.length - 1] : null };
    }
    return null;
}

// ── PHP ──────────────────────────────────────────────────────────────────

// PSR-4 (Composer's autoloading standard, `composer.json`'s
// `autoload.psr-4`): a NAMESPACE PREFIX — always matched WITH its trailing
// `\`, so `App\` does not also claim `AppFoo\` — maps to a DIRECTORY,
// relative to that composer.json's own folder. Longest prefix wins, the
// same "most specific root" rule Go's module-path matching already uses
// above; a bare class with no `use`d namespace at all, or one under no
// configured prefix, is left unresolved — not every PHP class lives under
// Composer's autoloader (a plain `require`-only project has none at all).
function resolvePhpFqn(fqn, psr4Roots, index) {
    let best = null;
    for (const r of psr4Roots) {
        if (fqn === r.prefix.slice(0, -1) || fqn.startsWith(r.prefix)) {
            if (!best || r.prefix.length > best.prefix.length) best = r;
        }
    }
    if (!best) return null;
    const rest = fqn.slice(best.prefix.length);
    if (!rest) return null; // the FQN *is* the bare prefix — no class name to resolve
    const relPath = rest.split('\\').filter(Boolean).join('/');
    const candidate = best.dir ? `${best.dir}/${relPath}.php` : `${relPath}.php`;
    return index.has(candidate) ? candidate : null;
}

// `require`/`include` of a literal string path resolves relative to the
// INCLUDING file's own directory — PHP's own default (there is no
// "index file" convention here, unlike JS) — with `.php` appended when
// the literal omits it (`require 'config'`, legal but unusual).
function resolvePhpRequire(fromFile, spec, index) {
    const norm = normJoin(pathPosix.dirname(fromFile), spec);
    if (index.has(norm)) return norm;
    if (!norm.endsWith('.php') && index.has(`${norm}.php`)) return `${norm}.php`;
    return null;
}

// ── per-file resolution ─────────────────────────────────────────────────

function resolveFile(f, index, byRel, configs, rustTrees) {
    const out = [];
    for (const imp of f.imports || []) {
        const from = imp.from;
        const line = imp.line;
        const reexport = !!imp.reexport;
        const push = (to, name, extra = {}) => {
            if (to) out.push({ from, to, name, line, kind: imp.kind || null, reexport, names: imp.names, ...extra });
        };
        if (from == null) {
            // Rust `modpath` (`mod foo;`) — resolved structurally below via
            // rustTrees, not through this generic specifier path.
            continue;
        }
        // okjs component-tag dependency (F4 item 3): `lang` for a `.ok.js`/
        // `.ok.html` file is still 'javascript'/'html' (the extension rides
        // ALONGSIDE the language, it does not change it — see parse.js's
        // langOf()), so this has to be checked BEFORE the language dispatch
        // below, or a tag like "x-widget" would be treated as a bare JS
        // specifier (and left unresolved, same as any external package).
        // `from` here is a component TAG, not a path — it resolves against
        // the workspace's component symbols, never the file tree.
        if (imp.kind === 'component' || imp.kind === 'component-tag') {
            const owner = (byRel.ownerOfComponentTag && byRel.ownerOfComponentTag(from)) || null;
            if (owner) push(owner, from);
            continue;
        }
        const lang = f.lang;
        if (lang === 'javascript' || lang === 'typescript') {
            if (isRelativeSpecifier(from)) {
                const found = resolveJsPath(normJoin(pathPosix.dirname(f.rel), from), index);
                push(found, imp.names && imp.names.length === 1 ? imp.names[0].imported : null);
            } else {
                const found = resolveTsPaths(from, pathPosix.dirname(f.rel), configs.tsconfigs, index);
                if (found) push(found, imp.names && imp.names.length === 1 ? imp.names[0].imported : null);
                // else: a bare specifier with no paths match is an external
                // package — left unresolved, never guessed.
            }
            continue;
        }
        if (lang === 'python') {
            if (imp.kind === 'modimport') {
                const parts = from.split('.').filter(Boolean);
                for (const root of configs.pyRoots) {
                    const found = resolvePyModule(root, parts, index);
                    if (found) {
                        push(found, null);
                        break;
                    }
                }
            } else {
                for (const r of resolvePyFromImport(f.rel, from, imp.names, configs.pyRoots, index)) push(r.to, r.name);
            }
            continue;
        }
        if (lang === 'go') {
            const { toFiles } = resolveGoImport(from, configs.goModules, index);
            for (const to of toFiles) push(to, null);
            continue;
        }
        if (lang === 'rust') {
            const tree = rustTrees.get(f.rel);
            if (tree) {
                const r = resolveRustPath(f.rel, from, tree);
                if (r) push(r.to, r.name);
            }
            continue;
        }
        if (lang === 'php') {
            if (imp.kind === 'require') {
                push(resolvePhpRequire(f.rel, from, index), null);
            } else {
                // `imp.kind === 'import'` (a `use` statement): one push per
                // name — a grouped `use A\{B, C}` already carries both in
                // `imp.names` (grammars/php/imports.scm), same shape a
                // multi-name Python `from X import a, b` already has.
                for (const { imported } of imp.names || []) {
                    const fqn = imported ? `${from ? `${from}\\` : ''}${imported}` : from;
                    push(resolvePhpFqn(fqn, configs.phpRoots, index), imported);
                }
            }
            continue;
        }
        // A `style` dependency (a stylesheet an .ok.js/.ok.html pulls in) is
        // otherwise an ordinary relative specifier.
        if (isRelativeSpecifier(from)) {
            const found = resolveJsPath(normJoin(pathPosix.dirname(f.rel), from), index) || (index.has(normJoin(pathPosix.dirname(f.rel), from)) ? normJoin(pathPosix.dirname(f.rel), from) : null);
            if (found) push(found, null);
        }
    }
    // Rust `mod foo;` declarations resolve structurally via the module
    // tree already built for this file's crate, independent of the
    // generic specifier loop above (they carry no `from` text at all).
    const tree = rustTrees.get(f.rel);
    if (tree) {
        for (const imp of f.imports || []) {
            if (imp.kind !== 'modpath') continue;
            for (const { local: name } of imp.names || []) {
                const childKey = [...(tree.fileToPath.get(f.rel) || []), name].join('::');
                const file = tree.pathToFile.get(childKey);
                if (file) out.push({ from: null, to: file, name: null, line: imp.line, kind: 'modpath', reexport: false, names: null });
            }
        }
    }
    return out;
}

// ── component-tag lookup (okjs, F4 item 3) ──────────────────────────────
// "Who uses <setting-group>" is a resolved ref, not a grep for the tag
// string: a file's `component`-kind symbol IS the tag's definition, so the
// map is just name -> defining file, built once per generation.
function buildComponentTagIndex(filesList) {
    const byTag = new Map();
    for (const f of filesList) {
        for (const s of f.symbols || []) {
            if (s.kind === 'component' && !s.parent && typeof s.name === 'string') byTag.set(s.name, f.rel);
        }
    }
    return byTag;
}

// ── the index ────────────────────────────────────────────────────────────

function createEdgeIndex({ access }) {
    let gen = -1;
    let forward = null; // rel -> resolved edges []
    let reverse = null; // rel -> importers []

    async function ensure(filesList, currentGen) {
        if (forward && gen === currentGen) return;
        const index = new Set(filesList.map((f) => f.rel));
        const byRel = new Map(filesList.map((f) => [f.rel, f]));
        const configs = await readConfigs(filesList, access);
        const tagIndex = buildComponentTagIndex(filesList);
        byRel.ownerOfComponentTag = (tag) => tagIndex.get(tag) || null;
        const rustTrees = new Map();
        for (const crate of findRustCrates(filesList)) {
            const tree = buildRustModuleTree(crate, byRel, index);
            for (const file of tree.fileToPath.keys()) rustTrees.set(file, tree);
        }

        forward = new Map();
        for (const f of filesList) forward.set(f.rel, resolveFile(f, index, byRel, configs, rustTrees));

        reverse = new Map();
        const addImporter = (target, entry) => {
            if (!reverse.has(target)) reverse.set(target, []);
            reverse.get(target).push(entry);
        };
        for (const [rel, edges] of forward) {
            for (const e of edges) {
                if (!e.to) continue;
                addImporter(e.to, { file: rel, line: e.line, kind: e.kind, names: e.names, via: null });
                // Follow a re-export ONE level: if e.to itself re-exports
                // e.name from a further file, whoever imports THIS file's
                // name is also, transitively, an importer of that further
                // file — but no deeper than one hop (DESIGN §12/F3).
                if (!e.reexport) continue;
            }
        }
        // Second pass: a re-export edge `A -> B` (A re-exports name N from
        // B) means every importer of A for N is ALSO an importer of B,
        // one hop — computed after `reverse` has every direct edge, so it
        // sees A's own importers.
        for (const [rel, edges] of forward) {
            for (const e of edges) {
                if (!e.reexport || !e.to) continue;
                const direct = reverse.get(rel) || [];
                for (const imp of direct) {
                    if (e.name && imp.names && !imp.names.some((n) => n.imported === e.name)) continue;
                    addImporter(e.to, { file: imp.file, line: imp.line, kind: imp.kind, names: imp.names, via: rel });
                }
            }
        }
        gen = currentGen;
    }

    return {
        ensure,
        importsOf(rel) {
            return (forward && forward.get(rel)) || [];
        },
        importersOf(rel) {
            return (reverse && reverse.get(rel)) || [];
        },
        invalidate() {
            gen = -1;
        },
    };
}

module.exports = { createEdgeIndex, stripJsonc };
