'use strict';
// Shared access helpers, computed by the native core. What stays here is Date/BigInt glue.
// (Locking is native in the local and shell facades.)
const { pure } = require('../native/pure');
const native = require('../native/native');

const f = pure('access/common');

const DEFAULT_SKIP = f('DEFAULT_SKIP')();
const PRUNE_DOT_DIRS = f('PRUNE_DOT_DIRS')();

const sha1 = (buf) => native.facadeSha1(Buffer.isBuffer(buf) ? buf : Buffer.from(buf));
const checkPath = (p, dialect = 'posix') => f('checkPath')(p, dialect);
const pathChecker = (dialect) => (p) => checkPath(p, dialect);
const mtimeOf = (st) => {
    const ns = st.mtimeNs; // BigInt
    return f('mtimeOf')(Number(ns / 1_000_000_000n), Number(ns % 1_000_000_000n));
};
const procStartTime = (pid) => f('procStartTime')(pid);

module.exports = {
    DEFAULT_SKIP,
    PRUNE_DOT_DIRS,
    sha1,
    checkPath,
    pathChecker,
    mtimeOf,
    procStartTime,
};
