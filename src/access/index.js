'use strict';
// The access facades okcode ships (contract: docs/ACCESS.md). Anything else —
// a remote API, an in-memory fixture — implements the same shape.

const { localFs } = require('./local-fs');
const shell = require('./shell');
const { DEFAULT_SKIP } = require('./common');

module.exports = { localFs, shell, DEFAULT_SKIP };
