'use strict';
// Text utilities (decoding, EOL handling, line/offset arithmetic, diff hunks), computed by the native core.
const { pure } = require('../native/pure');
const f = pure('analysis/text');
exports.decode = f('decode');
exports.dominantEol = f('dominantEol');
exports.normaliseEol = f('normaliseEol');
exports.lineOffsets = f('lineOffsets');
exports.lineRangeToOffsets = f('lineRangeToOffsets');
exports.lineColAt = f('lineColAt');
exports.offsetAtLineCol = f('offsetAtLineCol');
exports.lineStartOf = f('lineStartOf');
exports.lineEndOf = f('lineEndOf');
exports.widenToContext = f('widenToContext');
exports.hunksFor = f('hunksFor');
