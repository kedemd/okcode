'use strict';
// The one registrant, today, of the generic extension registry (extensions.js).
// Everything OKJS-specific lives HERE and nowhere else in the code index —
// the workspace, parse.js and validate.js only ever see the normalised envelope
// this file produces: symbols/imports/exports (symbols include the component
// tag AND, nested under it, the generic acorn extractor's own symbols for the
// same JavaScript — F4 item 1), plus diagnostics/coverage/the raw analysis
// riding along under `okAnalysis`, and `regions` already projected down to
// `{ name, kind, lineStart, lineEnd, doc }` outline pieces (F4 item 2) — never
// the analyser's own region shape (`source`, `contentRange`/`valueRange`,
// `role`, `format`/`language`) and never OKJS syntax itself.
//
// @kedem/okjs/tooling is a pure, non-executing static analyser: it never
// fetches, imports component modules, executes component code or creates a
// DOM. It is deliberately conservative about what it claims — binding,
// event, primitive and scope semantics are reported as coverage gaps, not
// guessed — which is exactly the posture this index wants from an
// extension: whatever the analyser does NOT know, it says so, rather than
// this file inventing an answer on its behalf.
const { registerExtension } = require('./extensions');

const MATCH = /\.(ok\.js|ok\.mjs|ok\.html)$/i;

// Bumped whenever THIS FILE changes what analyze() returns for the SAME
// input — a new generic-symbol nesting pass (F4 item 1) or a different
// region shape (F4 item 2) are both exactly that, and neither the installed
// @kedem/okjs package's own version nor its SCHEMA_VERSION moved to say so.
// Folded into the schema segment (not a third colon-separated field) so the
// version string's shape — "pkgVersion:schema" — stays the same for anyone
// already parsing it; see the workspace's analyzerVersion restale check.
const EXTENSION_CONTRACT_VERSION = 2;

let tooling = null;
let toolingError = null;
function loadTooling() {
    if (tooling || toolingError) return tooling;
    try {
        tooling = require('@kedem/okjs/tooling');
    } catch (err) {
        toolingError = err;
    }
    return tooling;
}

function toolingVersion() {
    let pkgVersion = '0';
    try {
        pkgVersion = require('@kedem/okjs/package.json').version || '0';
    } catch {
        /* unversioned */
    }
    const t = loadTooling();
    const schema = t && t.SCHEMA_VERSION != null ? t.SCHEMA_VERSION : '0';
    return `${pkgVersion}:${schema}+ext${EXTENSION_CONTRACT_VERSION}`;
}

// A synthetic identifier, never a filesystem lookup — analyzeOKSource only
// uses baseURL to CLASSIFY a specifier (relative/root/absolute/bare) and
// join it against an import map, never to read anything.
function baseURLFor(path) {
    return `file:///${String(path).replace(/\\/g, '/').replace(/^\/+/, '')}`;
}

// One component-definition symbol per file, addressed by its tag — the one
// piece of an OKJS source a caller would plausibly name directly
// (`resolve('x-card')`), unlike a template or style region. `range` (the
// whole definition), not `selectionRange` (just the tag string), is what
// backs start/end/lineStart/lineEnd — every other symbol in this index
// addresses a symbol's full body, and a "component" symbol should be no
// different.
function translateSymbols(analysis) {
    return (
        (analysis.symbols || [])
            .filter((s) => s.kind === 'component' && s.range)
            // The tag is a name, and a name is indexed: it goes out as a string
            // whatever the analyser handed over (okdb rejects a non-scalar
            // indexed value, aborting the write it rides in).
            .map((s) => ({ s, name: s.name == null ? '' : String(s.name) }))
            .filter(({ name }) => name)
            .map(({ s, name }) => ({
                name,
                kind: s.kind,
                path: name,
                start: s.range.start,
                end: s.range.end,
                lineStart: s.range.loc.start.line,
                lineEnd: s.range.loc.end.line,
            }))
    );
}

// F4 item 1: a component file's ONLY symbol used to be the tag — `find('rows')`
// could not reach a member without falling back to grep/FTS, because the okjs
// extension outranks the generic extractor in parse.js#extract() instead of
// adding to it. The fix runs BOTH: the okjs analyser for the component symbol
// (above) and regions, and the plain acorn extractor (parse.js#extractJavaScript,
// the same one every `.js` file gets) for the file's JavaScript, then nests
// the latter's symbols under the component they fall inside (below).
//
// Required lazily, never at module load: parse.js requires ext-okjs.js (this
// file) at ITS load time to register the extension, so requiring parse.js
// back from here at OUR load time would see parse.js's module only
// half-built (`module.exports` not yet assigned). By the time analyze()
// actually runs, both modules have finished loading and this is safe.
let parseMod = null;
function requireParse() {
    if (!parseMod) parseMod = require('./parse');
    return parseMod;
}

// The `<script>` block's region for a `.ok.html` file — the only part of
// the file that is JavaScript at all; `.ok.js`/`.ok.mjs` have no template or
// style markup around the code, so the whole source already IS the script.
function scriptRegionOf(analysis) {
    return (analysis.regions || []).find((r) => r.role === 'script' && r.contentRange);
}

// 1-based line number of a byte offset, for remapping a `.ok.html` script
// block's acorn line numbers (which start counting from the block's own
// line 1) back into whole-file line numbers.
function lineAt(source, offset) {
    return source.slice(0, offset).split('\n').length;
}

function shiftSymbols(symbols, byteOffset, baseLine) {
    if (!byteOffset) return symbols;
    return symbols.map((s) => ({
        ...s,
        start: s.start + byteOffset,
        end: s.end + byteOffset,
        lineStart: s.lineStart + baseLine - 1,
        lineEnd: s.lineEnd + baseLine - 1,
    }));
}

// The generic extractor's symbols for this file's JavaScript. For
// `.ok.js`/`.ok.mjs` that is `source` itself (one JS module); for
// `.ok.html` it is only the `<script>` block's text, run on its own and
// shifted back into whole-file byte/line coordinates — the okjs analyser's
// OWN ranges are already whole-file coordinates (its `analyzeJavaScriptRegion`
// takes the block's offset for exactly this reason), so this mirrors that
// rather than inventing a second coordinate scheme.
//
// No `<script>` block found on a `.ok.html` file (or acorn cannot parse
// what it finds — should not happen, since the okjs analyser already
// parsed this same JavaScript, but a checker that throws must never take
// the whole file down): falls back to tag-only, same as before this fix.
function genericSymbolsFor(path, source, analysis) {
    let jsSource = source;
    let byteOffset = 0;
    if (/\.ok\.html$/i.test(String(path || ''))) {
        const region = scriptRegionOf(analysis);
        if (!region) return [];
        byteOffset = region.contentRange.start;
        jsSource = source.slice(region.contentRange.start, region.contentRange.end);
    }
    try {
        const { extractJavaScript } = requireParse();
        const { symbols } = extractJavaScript(path, jsSource);
        return shiftSymbols(symbols, byteOffset, lineAt(source, byteOffset));
    } catch {
        return [];
    }
}

// Nests a generic symbol (and everything already nested under it, e.g.
// `context.refresh` under `context`) under the component tag whose
// definition range contains it, by byte-range containment — the acorn
// extractor already worked out the inner nesting; this adds one more name
// segment in front (`setting-group.context`, `setting-group.context.refresh`).
// A top-level generic symbol OUTSIDE every component's range (a helper
// function above the `export default`) is left exactly as acorn reported
// it: "top-level module functions outside it stay top-level."
function nestComponentMembers(componentSymbols, genericSymbols) {
    if (!componentSymbols.length || !genericSymbols.length) return genericSymbols;
    const claimed = new Set();
    const nested = [];
    for (const tag of componentSymbols) {
        const roots = genericSymbols.filter(
            (s) => !s.parent && !claimed.has(s) && s.start >= tag.start && s.end <= tag.end,
        );
        if (!roots.length) continue;
        const rootPaths = roots.map((s) => s.path);
        for (const s of genericSymbols) {
            if (claimed.has(s)) continue;
            if (!rootPaths.some((r) => s.path === r || s.path.startsWith(`${r}.`))) continue;
            claimed.add(s);
            nested.push({
                ...s,
                path: `${tag.name}.${s.path}`,
                parent: s.parent ? `${tag.name}.${s.parent}` : tag.name,
            });
        }
    }
    const leftover = genericSymbols.filter((s) => !claimed.has(s));
    return [...nested, ...leftover];
}

// F4 item 2: workspace.js used to know the okjs analyser's OWN region shape
// (`source: 'definition'|'block'`, `contentRange`/`valueRange`, `role`,
// `format`/`language`) — exactly the thing "everything OKJS-specific lives
// HERE" (file header) promised it would never have to. This is that
// projection, moved here: `{ name, kind, lineStart, lineEnd, doc }` only,
// which is all carveRegions() (workspace.js) and the rest of the piecewise
// outline ever needed. Only `definition`/`block` regions are outline
// pieces — `source: 'script'` regions (one import statement) are
// reference-sized, not a piece a caller reads or edits by itself.
function normalizeRegions(analysis) {
    return (analysis.regions || [])
        .filter((r) => r.source === 'definition' || r.source === 'block')
        .map((r) => ({ region: r, span: r.contentRange || r.valueRange || r.range }))
        .filter(({ span }) => span && span.end > span.start)
        .map(({ region, span }) => ({
            name: region.role || region.kind,
            kind: region.kind,
            lineStart: span.loc.start.line,
            lineEnd: Math.max(span.loc.start.line, span.loc.end.line),
            doc: [region.role, region.format || region.language].filter(Boolean).join(' ') || null,
        }));
}

// Dependency edges, straight from the analyser's own resolution — never a
// regex scan of the source. `kind` rides through unmodified (esm-static,
// esm-dynamic, esm-export, component, component-tag, style) so a caller can
// tell an ESM import from a component dependency from a stylesheet link.
function translateImports(analysis) {
    return (analysis.dependencies || []).map((d) => {
        const at = d.statementRange || d.range;
        return {
            local: null,
            from: d.specifier != null ? d.specifier : d.tag || null,
            line: at && at.loc ? at.loc.start.line : null,
            esm: typeof d.kind === 'string' && d.kind.startsWith('esm'),
            reexport: d.kind === 'esm-export',
            kind: d.kind,
        };
    });
}

// `load` is injectable so the tooling-absent branch can be exercised on a
// machine where @kedem/okjs IS installed (it is a dev dependency here).
function makeAnalyze(load = loadTooling) {
    return function analyze({ path, source, baseURL = null, importMap = null } = {}) {
        const t = load();
        if (!t) {
            // Not a parse failure: the file is still JavaScript (or HTML), and
            // the generic extractor for its language knows how to read that.
            // `fallback` asks parse.js#extract() to run it; the reason says
            // what was skipped, so the degraded result is never mistaken for
            // a full okjs analysis.
            const why =
                toolingError && load === loadTooling ? String(toolingError.message).slice(0, 160) : 'not installed';
            return {
                symbols: [],
                imports: [],
                exports: [],
                parsed: false,
                fallback: true,
                reason: `okjs tooling unavailable (${why}) — okjs-specific analysis skipped`,
            };
        }
        return analyzeWith(t, { path, source, baseURL, importMap });
    };
}

function analyzeWith(t, { path, source, baseURL, importMap }) {
    try {
        const analysis = t.analyzeOKSource({
            path,
            source,
            baseURL: baseURL || baseURLFor(path),
            importMap,
        });
        const componentSymbols = translateSymbols(analysis);
        // F4 item 1: the component symbol ALONE, plus the generic extractor's
        // symbols for the same file's JavaScript, nested under the component
        // they fall inside of. Imports stay the analyser's own
        // (translateImports, above) — acorn's import list is never merged in.
        const generic = genericSymbolsFor(path, source, analysis);
        return {
            symbols: [...componentSymbols, ...nestComponentMembers(componentSymbols, generic)],
            imports: translateImports(analysis),
            exports: [],
            // `parsed` means "the analyser could walk this source," same as
            // acorn's meaning for a plain JS file — it says nothing about
            // whether the source is CORRECT. A diagnostic ("missing tag",
            // "invalid definition") is a finding about well-formed source,
            // not a parse failure; those live in `diagnostics`, not here.
            parsed: true,
            // Riding along, verbatim — the normalised symbols/imports above
            // are a PROJECTION of this, not a replacement for it.
            okAnalysis: analysis,
            diagnostics: analysis.diagnostics,
            coverage: analysis.coverage,
            // Normalized outline pieces (F4 item 2), not the analyser's own
            // region shape — see normalizeRegions() above.
            regions: normalizeRegions(analysis),
        };
    } catch (err) {
        // Fail soft, same contract parse.js#extract() already keeps for
        // every other extractor: a checker that throws must never make a
        // file invisible to the index, only unparsed.
        return { symbols: [], imports: [], exports: [], parsed: false, reason: String(err.message).slice(0, 160) };
    }
}

const analyze = makeAnalyze();

registerExtension({
    id: 'okjs',
    match: (path) => MATCH.test(String(path || '')),
    analyze,
    version: toolingVersion(),
});

module.exports = { analyze, makeAnalyze, baseURLFor, toolingVersion };
