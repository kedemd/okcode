'use strict';
// The one registrant, today, of the generic extension registry (extensions.js).
// Everything OKJS-specific lives HERE and nowhere else in the code index —
// the workspace, parse.js and validate.js only ever see the normalised envelope
// this file produces (symbols/imports/exports, plus regions/diagnostics/
// coverage/the raw analysis riding along), never OKJS syntax itself.
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
    return `${pkgVersion}:${schema}`;
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
        return {
            symbols: translateSymbols(analysis),
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
            regions: analysis.regions,
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
