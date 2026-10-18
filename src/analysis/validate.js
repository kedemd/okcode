'use strict';
// Edit-time validation (syntax, template balance, convention colours, okjs), run by the native core.
// JavaScript is checked with oxc, so the validator is synchronous; the async variant resolves to the
// same result. The extension registry lives in JavaScript (./extensions, as for extract()): a
// host-registered extension's analyze runs here, and its outcome is mapped natively.
const { pure } = require('../native/pure');
const { extensionFor } = require('./extensions');
const { isNative } = require('./ext-okjs');
const f = pure('analysis/validate');

const validateNative = f('validateSource');
const acornGoalsFor = f('acornGoalsFor');
const nodeCheckGoalsFor = f('nodeCheckGoalsFor');
const compareNative = f('compareDiagnostics');

// A host-registered extension the registry matched holds user functions: run its analyze here and hand
// native the outcome ({id, skipped} | {id, diagnostics}); native maps it like its own okjs step.
// The built-in okjs analyser stays native (no extOutcome).
function extOutcomeOf(ext, rel, text) {
    try {
        const result = ext.analyze({ path: rel, source: text });
        if (result.fallback) return { id: ext.id, skipped: String(result.reason || 'analyser unavailable') };
        return { id: ext.id, diagnostics: result.diagnostics || [] };
    } catch (err) {
        return { id: ext.id, skipped: String(err.message).slice(0, 120) };
    }
}

function validateSource(opts) {
    const ext = extensionFor(opts.rel);
    const extOutcome = ext && !isNative(ext) ? extOutcomeOf(ext, opts.rel, opts.text) : null;
    return validateNative({ ...opts, extOutcome });
}

async function validateSourceAsync(opts) {
    return validateSource(opts);
}

// mapOffset is a JavaScript function: evaluate it once per baseline start here, match natively.
function compareDiagnostics(baseline, candidate, { mapOffset, targets = [] } = {}) {
    const mapped = mapOffset ? baseline.map((d) => [d.start, mapOffset(d.start) ?? null]) : null;
    return compareNative(baseline, candidate, { mapped, targets });
}

module.exports = { validateSource, validateSourceAsync, compareDiagnostics, acornGoalsFor, nodeCheckGoalsFor };
