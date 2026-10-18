'use strict';
// Symbol-aware chunking for embeddings, computed by the native core.
const { pure } = require('../native/pure');
const f = pure('analysis/chunk');
exports.STRATEGY = 'okcode-symbols';
exports.PREPARER = 'okcode-content';
exports.MARK = '//@ ';
exports.MIN_EMBED_CHARS = 120;
exports.CHUNK = 1400;
exports.CHUNK_MAX = 2600;
exports.REGION_LINES = 80;
exports.LOG_LINES = 200;
for (const n of ['slug', 'boundaries', 'paragraphBoundaries', 'markdownBoundaries', 'htmlBoundaries', 'jsonBoundaries', 'boundariesFor', 'fromBoundaries', 'chunkCode', 'regions', 'sectionOf', 'headedChunks', 'prepareText', 'markLength']) exports[n] = f(n);
// walkInner took a live JavaScript AST node, which cannot cross into the native core: not available.
exports.walkInner = () => {
    throw new Error('chunk.walkInner is not available in the native build (takes an acorn AST node)');
};
