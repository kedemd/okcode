'use strict';
// Call a pure function of the native core: `pure('analysis/text')('lineOffsets')(...args)` returns a
// JavaScript function taking and returning plain values (json.js conventions).
const native = require('./native');
const { toJson, fromJson, args } = require('./json');
const { sync } = require('./errors');

const pure = (module) => (name) => (...a) => fromJson(sync(() => native.pureCall(module, name, args(a))));
module.exports = { pure, native, toJson, fromJson };
