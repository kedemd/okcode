'use strict';
// The okjs extension: the built-in registrant of the extension registry. The okjs analyser runs
// natively; this file keeps the registration and makeAnalyze(load), which builds an analyze()
// over another okjs tooling loader.
const { pure } = require('../native/pure');
const { registerExtension } = require('./extensions');

const f = pure('analysis/ext-okjs');
const MATCH = /\.(ok\.js|ok\.mjs|ok\.html)$/i;

const toolingVersion = () => f('toolingVersion')();
const baseURLFor = (path) => f('baseURLFor')(path);

// The embedded analyser, in place of `require('@kedem/okjs/tooling')`. A sentinel: analyze() below sends
// the source straight to the native core instead of round-tripping the raw analysis through JavaScript.
const NATIVE_TOOLING = Object.freeze({ native: true });
const loadTooling = () => NATIVE_TOOLING;

// `load` returns the okjs tooling, or nothing when it is not installed (the analysis is then skipped).
function makeAnalyze(load = loadTooling) {
    return function analyze({ path, source, baseURL = null, importMap = null } = {}) {
        const t = load();
        if (!t) {
            return {
                symbols: [],
                imports: [],
                exports: [],
                parsed: false,
                fallback: true,
                reason: 'okjs tooling unavailable (not installed) — okjs-specific analysis skipped',
            };
        }
        if (t === NATIVE_TOOLING) return f('analyze')(path, source);
        // A supplied analyser: its analysis is projected natively; a failure degrades softly.
        let analysis;
        try {
            analysis = t.analyzeOKSource({ path, source, baseURL: baseURL || baseURLFor(path), importMap });
            return f('fromAnalysis')(path, source, analysis);
        } catch (err) {
            return { symbols: [], imports: [], exports: [], parsed: false, reason: String(err.message).slice(0, 160) };
        }
    };
}

const analyze = makeAnalyze();
// Marks the default (embedded) analyser: validation lets the native core run the okjs-analyze
// validator itself. A supplied or replaced registrant lacks the marker and runs in JavaScript.
const isNative = (entry) => !!entry && entry.analyze === analyze;

registerExtension({
    id: 'okjs',
    match: (path) => MATCH.test(String(path || '')),
    analyze,
    version: toolingVersion(),
});

module.exports = { analyze, isNative, makeAnalyze, baseURLFor, toolingVersion };
