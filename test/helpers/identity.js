'use strict';
// Pipeline names as the native core computes them (docs/DESIGN.md §8), for tests that assert on them.
const { pure } = require('../../lib/native/pure');

module.exports = { pipelineName: pure('identity')('pipelineName') };
