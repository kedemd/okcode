'use strict';
// Template balance (unclosed / mismatched tags in template literals), computed by the native core.
const { pure } = require('../native/pure');
const f = pure('analysis/template-balance');
exports.check = f('check');
exports.templateSpans = f('templateSpans');
exports.VOID = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr']);
