'use strict';
// The local facade over the native local filesystem facade. The object keeps the member names
// the access bridge reads (list/stat/read/commit/lock/remove/exec/checkPath) and carries the
// okcode.native.localFs tag itself, so a workspace over it uses the native facade directly.
const path = require('path');
const native = require('../native/native');
const { pathChecker } = require('./common');
const { methods } = require('./_facade');

const TAG = Symbol.for('okcode.native.localFs');

function localFs(root) {
    if (typeof root !== 'string' || !root) throw new Error('localFs(root): root is required');
    const base = path.resolve(root);
    const dialect = process.platform === 'win32' ? 'windows' : 'posix';
    const checkPath = pathChecker(dialect);
    const m = methods(native.facadeLocalFs(root), checkPath);
    const facade = { kind: 'localFs', dialect, root: base, checkPath };
    for (const k of ['list', 'stat', 'read', 'commit', 'lock', 'remove', 'exec']) facade[k] = m[k];
    const fns = {};
    for (const k of ['list', 'stat', 'read', 'commit', 'lock', 'remove', 'exec', 'checkPath']) fns[k] = facade[k];
    // configurable, so a wrapper may re-tag it.
    Object.defineProperty(facade, TAG, { value: { root: base, fns }, enumerable: false, configurable: true });
    return facade;
}

module.exports = { localFs };
