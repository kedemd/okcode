'use strict';
// Embedder identities and pipeline names (docs/DESIGN.md §8), computed by the native core.
const { pure } = require('./native/pure');
const f = pure('identity');

module.exports = {
    partsOf: f('partsOf'),
    identityOf: f('identityOf'),
    pipelineName: f('pipelineName'),
    findPipeline: f('findPipeline'),
    shortHash: f('shortHash'),
    isOurs: f('isOurs'),
    endpointOf: f('endpointOf'),
};
