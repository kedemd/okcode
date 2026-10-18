'use strict';
// Import-edge helpers. stripJsonc (comments and trailing commas out of a JSONC text) is native.
// createEdgeIndex is not available: the native workspace keeps its own edge index.
const { pure } = require('./native/pure');

function createEdgeIndex() {
    const err = new Error('createEdgeIndex is internal to the JS workspace; the native workspace has its own edge index');
    err.code = 'OKCODE_NOT_IMPLEMENTED';
    throw err;
}

module.exports = { createEdgeIndex, stripJsonc: pure('edges')('stripJsonc') };
