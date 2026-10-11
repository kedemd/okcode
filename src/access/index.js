'use strict';
// The access facades okcode ships (contract: docs/ACCESS.md). Anything else —
// a remote API, an in-memory fixture — implements the same shape. `fn` and
// `memory` are read-only (no commit).

const { localFs } = require('./local-fs');
const shell = require('./shell');
const { fn, memory } = require('./fn');
const { DEFAULT_SKIP } = require('./common');

module.exports = { localFs, shell, fn, memory, DEFAULT_SKIP };
