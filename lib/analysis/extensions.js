'use strict';
// The generic extension registry. This file must never learn a dialect's
// syntax — that is exactly what it exists to avoid. An extension owns
// recognising its own files (`match`) and producing a structural analysis of
// one (`analyze`); everything else in the code index consumes that analysis
// through the one normalised shape `extract()` already returns
// (symbols/imports/exports), plus whatever extra fields ride along for
// callers that want the richer, dialect-specific result: `diagnostics`,
// `coverage`, the raw analysis itself (opaque to this registry, under
// whatever name the extension chooses — ext-okjs.js uses `okAnalysis`), and
// `regions`, which an extension MUST already have projected down to this
// registry's own shape — `{ name, kind, lineStart, lineEnd, doc }` outline
// pieces only — never a dialect's own region shape. The workspace's
// piecewise outline consumes exactly that shape and nothing dialect-specific;
// an extension that leaks its own region fields (source/contentRange/role/…)
// past itself is a bug in the extension, not something this registry — or
// the workspace — should ever have to know.
//
// First match wins, in REGISTRATION order. okcode itself registers one
// extension (okjs, see ext-okjs.js); when two extensions claim the same file
// (a wrapper format, a preprocessor), "first registered" is a predictable
// answer rather than an accidental one.

const registry = [];
const listeners = [];

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
    for (const fn of listeners) fn();
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

// Called after every registration (the workspace's indexing follows the registry).
function onRegister(fn) {
    listeners.push(fn);
}

function extensions() {
    return registry.slice();
}

function listExtensions() {
    return registry.map((e) => ({ id: e.id, version: e.version }));
}

module.exports = { registerExtension, extensionFor, listExtensions, onRegister, extensions };
