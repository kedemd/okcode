'use strict';
// The analysis layer: pure functions on text (language detection, symbol
// extraction, validation, text/EOL/diff utilities) plus the dependency
// package surface, which reads through an access facade. Nothing here opens
// a file or a connection on its own.

const parse = require('./parse');
const validate = require('./validate');
const text = require('./text');
const extensions = require('./extensions');
const templateBalance = require('./template-balance');
const conventionColor = require('./convention-color');
const okjs = require('./ext-okjs');
const packages = require('./packages');

module.exports = {
    // parse
    extract: parse.extract,
    langOf: parse.langOf,
    extOf: parse.extOf,
    isTextual: parse.isTextual,
    isSecret: parse.isSecret,
    registerExtractor: parse.registerExtractor,
    parseSource: parse.parseSource,
    TEXT_MAX: parse.TEXT_MAX,
    // validate
    validateSource: validate.validateSource,
    validateSourceAsync: validate.validateSourceAsync,
    compareDiagnostics: validate.compareDiagnostics,
    acornGoalsFor: validate.acornGoalsFor,
    nodeCheckGoalsFor: validate.nodeCheckGoalsFor,
    // text
    ...text,
    // extensions
    registerExtension: extensions.registerExtension,
    extensionFor: extensions.extensionFor,
    listExtensions: extensions.listExtensions,
    // template balance (namespaced: `check` alone says nothing)
    templateBalance,
    checkTemplateBalance: templateBalance.check,
    // convention colour
    paletteFrom: conventionColor.paletteFrom,
    colorLiterals: conventionColor.colorLiterals,
    rgbOf: conventionColor.rgbOf,
    nearest: conventionColor.nearest,
    // okjs extension
    okjs,
    // packages
    scanPackages: packages.scanPackages,
    readPackages: packages.readPackages,
    namesFromDts: packages.namesFromDts,
    membersFromDts: packages.membersFromDts,
    reExports: packages.reExports,
    declared: packages.declared,
};
