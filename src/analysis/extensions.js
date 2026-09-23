'use strict';
// The generic extension registry. This file must never learn a dialect's
// syntax — that is exactly what it exists to avoid. An extension owns
// recognising its own files (`match`) and producing a structural analysis of
// one (`analyze`); everything else in the code index consumes that analysis
// through the one normalised shape parse.js's `extract()` already returns
// (symbols/imports/exports), plus whatever extra fields ride along for
// callers that want the richer, dialect-specific result (regions,
// diagnostics, coverage, the raw analysis itself).
//
// First match wins, in REGISTRATION order — there is currently exactly one
// registrant (see ext-okjs.js) and no ordering policy has ever been needed,
// but a later file extension owned by two extensions is a real possibility
// (a wrapper format, a preprocessor) and "first registered" is at least a
// predictable answer rather than an accidental one.

const registry = [];

function registerExtension({ id, match, analyze, version = null } = {}) {
    if (!id) throw new Error('registerExtension needs an id');
    if (typeof match !== 'function') throw new Error(`registerExtension(${id}) needs a match(path) function`);
    if (typeof analyze !== 'function') throw new Error(`registerExtension(${id}) needs an analyze(options) function`);
    // Re-registering the same id replaces it in place rather than growing a
    // duplicate — a module that gets require()'d twice under two different
    // relative paths (a real hazard with mixed require/import graphs) must
    // not end up matching twice.
    const existingIndex = registry.findIndex((e) => e.id === id);
    const entry = { id, match, analyze, version };
    if (existingIndex >= 0) registry[existingIndex] = entry;
    else registry.push(entry);
    return entry;
}

function extensionFor(path) {
    const p = String(path || '');
    for (const entry of registry) {
        try {
            if (entry.match(p)) return entry;
        } catch {
            /* a broken match() disqualifies its own extension, not the lookup */
        }
    }
    return null;
}

function listExtensions() {
    return registry.map((e) => ({ id: e.id, version: e.version }));
}

module.exports = { registerExtension, extensionFor, listExtensions };
