'use strict';
// Language detection and symbol extraction, done by the native core. This file keeps only what
// cannot cross into it as data:
//   - the registry of user extractors (registerExtractor) and the extension registry's
//     consultation order in extract(): extension, then a registered extractor for the language,
//     then native (tree-sitter / JavaScript / searchable text);
//   - the constant TEXT_MAX.
const { pure } = require('../native/pure');
const { extensionFor } = require('./extensions');
// Requiring this module registers the okjs extension.
require('./ext-okjs');

const f = pure('analysis/parse');
const langOf = f('langOf');
const extOf = f('extOf');
const isTextual = f('isTextual');
const isSecret = f('isSecret');
const extractJavaScript = f('extractJavaScript');
const extractGeneric = f('extractGeneric');
const nativeVersionFor = f('analyzerVersionFor');
const nativeParseSource = f('parseSource');

const TEXT_MAX = 512 * 1024;

// The languages with a native extractor (JavaScript, then the tree-sitter ones): what a fallback from
// an extension may run for the same path.
const NATIVE_LANGS = new Set(['javascript', 'typescript', 'python', 'go', 'rust', 'php', 'java', 'csharp', 'c', 'cpp']);

// User-registered extractors only; every built-in language is native.
const EXTRACTORS = new Map();

function registerExtractor(lang, fn) {
    EXTRACTORS.set(lang, fn);
}

// The generic extractor for a language: (path, src) => { symbols, imports, exports, … }, throwing when it
// cannot parse (the shape every registered extractor has).
function extractorFor(lang) {
    const own = EXTRACTORS.get(lang);
    if (own) return own;
    if (!NATIVE_LANGS.has(lang)) return null;
    return (path, src) => {
        const r = extractGeneric(path, src);
        if (!r.parsed) throw new Error(r.reason);
        const { path: _p, lang: _l, lines: _n, parsed: _ok, indexed: _i, analyzerVersion: _v, reason: _r, extractor: _e, ...rest } = r;
        return rest;
    };
}

function extract(path, src) {
    if (src == null) return extractGeneric(path, null);
    const lang = langOf(path);
    const lines = src.split('\n').length;
    const ext = extensionFor(path);
    if (ext) {
        try {
            const { okAnalysis, diagnostics, coverage, regions, fallback, ...envelope } = ext.analyze({
                path,
                source: src,
            });
            const generic = fallback && extractorFor(lang);
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

    const registered = EXTRACTORS.get(lang);
    if (!registered) return extractGeneric(path, src);
    const analyzerVersion = analyzerVersionFor(path);
    try {
        return {
            path,
            lang,
            lines,
            ...registered(path, src),
            analyzerVersion,
            parsed: true,
            indexed: true,
            ...(lang === 'javascript' ? { extractor: 'acorn' } : {}),
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
            analyzerVersion,
            reason: String(err.message).slice(0, 160),
        };
    }
}

function analyzerVersionFor(path) {
    const ext = extensionFor(path);
    if (ext) return ext.version;
    return nativeVersionFor(path);
}

// Throws on a syntax error with acorn's wording. The AST itself cannot cross the native boundary:
// the result is a stub { type: 'Program', sourceType, body: [] }.
const parseSource = (src) => nativeParseSource(src);

module.exports = {
    extract,
    langOf,
    extOf,
    isTextual,
    isSecret,
    registerExtractor,
    TEXT_MAX,
    parseSource,
    analyzerVersionFor,
    extractJavaScript: (path, src) => extractJavaScript(path, src),
};
