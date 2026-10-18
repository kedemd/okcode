'use strict';
// Convention-colour checks (palette, colour literals, nearest match), computed by the native core.
const { pure } = require('../native/pure');
const f = pure('analysis/convention-color');
exports.paletteFrom = f('paletteFrom');
exports.colorLiterals = f('colorLiterals');
exports.rgbOf = f('rgbOf');
exports.nearest = f('nearest');
