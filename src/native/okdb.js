'use strict';
// okcode 1.0 depends on @kedem/okdb ^3 (a normal npm dependency, never bundled). Library mode attaches to the HOST's
// instance through okdb's stable C bridge: the table address comes from the host's own addon (`db._native.bridgeTable()`),
// so okcode uses the very okdb image the host loaded. The bridge's ABI/version check refuses a mismatch.
function bridgeOf(db) {
    const n = db && db._native;
    if (!n || typeof n.bridgeTable !== 'function' || typeof n.bridgeHandle !== 'function') {
        const e = new Error('okcode needs a @kedem/okdb v3 instance (native, with the bridge): got ' + (db ? 'an okdb without db._native.bridgeTable()' : 'nothing'));
        e.code = 'OKCODE_OKDB_V3_REQUIRED';
        throw e;
    }
    return { table: BigInt(n.bridgeTable()), handle: BigInt(n.bridgeHandle()), path: typeof n.bridgeLibPath === 'function' ? n.bridgeLibPath() : null };
}

// The okdb .node of okcode's own @kedem/okdb dependency (what the CLI loads; npm dedupes it with the host's).
function okdbBinaryPath() {
    return require('@kedem/okdb').nativeBinaryPath;
}

// okcode's native handle on `db` (one per db object; attached on this JS thread).
const handles = new WeakMap();
function handleOf(db) {
    let h = handles.get(db);
    if (!h) {
        const b = bridgeOf(db);
        const native = require('./native');
        h = native.dbAttach(b.table, b.handle, String(db.path));
        handles.set(db, h);
    }
    return h;
}

module.exports = { bridgeOf, okdbBinaryPath, handleOf };
