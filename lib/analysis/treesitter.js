'use strict';
// The tree-sitter symbol engine, run by the native core.
// Grammars are compiled into the native binary, so preload/ensureLanguage only resolve.
const { pure } = require('../native/pure');
const f = pure('analysis/treesitter');

const langIdForPath = f('langIdForPath');
const currentVersionFor = f('currentVersionFor');
const extractFor = f('extractFor');
const syntaxDiagnostics = f('syntaxDiagnostics');
const jsxGrammarAvailable = f('jsxGrammarAvailable');
const jsxSyntaxDiagnostics = f('jsxSyntaxDiagnostics');
const jsxFallbackVersionTag = f('jsxFallbackVersionTag');
const extractJsxFallbackNative = f('extractJsxFallback');
const ensureLanguageNative = f('ensureLanguage');

// Constant data, read once.
const LANGUAGES = f('LANGUAGES')();

async function ensureLanguage(id) {
    return ensureLanguageNative(id);
}

async function preload() {}

function extractJsxFallback(path, src) {
    return extractJsxFallbackNative(path, src);
}

// Takes a live tree-sitter tree + query object, which cannot cross into the native core: not available.
function buildImports() {
    throw new Error('treesitter.buildImports takes a JS tree/query and is not available natively');
}

// A no-op: the native tree-sitter runtime cannot be unloaded.
function _resetForTest() {}

module.exports = {
    langIdForPath,
    preload,
    ensureLanguage,
    extractFor,
    syntaxDiagnostics,
    currentVersionFor,
    LANGUAGES,
    buildImports,
    extractJsxFallback,
    jsxSyntaxDiagnostics,
    jsxFallbackVersionTag,
    jsxGrammarAvailable,
    _resetForTest,
};
