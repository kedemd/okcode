'use strict';
// Grep matching and path filtering, done natively. The matcher and path filter are closures over
// native calls (the compiled regex lives in the native core, cached by the matcher's public fields).
const { pure } = require('./native/pure');
const f = pure('grep');

const nGlob = f('globToRegExp');
const nCompile = f('compileMatcher');
const nTest = f('matcherTest');
const nAll = f('matcherAll');
const nPathFilters = f('pathFilters');
const nPathMatch = f('pathMatch');
const nMatchText = f('matchText');
const nTally = f('tallyText');

// A matcher as the native side needs it: its public fields, no closures.
const desc = (m) => ({ literal: m.literal, ignoreCase: m.ignoreCase, source: m.source });

function globToRegExp(glob) {
    const r = nGlob(glob);
    return new RegExp(r.$re, r.flags);
}

function pathMatcher({ glob = null, paths = null } = {}) {
    const opts = { glob, paths };
    if (!nPathFilters(opts)) return null;
    return (rel) => nPathMatch(opts, rel);
}

function compileMatcher(pattern, { regex = false, caseSensitive = false } = {}) {
    const d = nCompile(pattern, { regex, caseSensitive });
    const test = (line) => nTest(d, String(line));
    const all = (line) => nAll(d, String(line));
    return { test, all, literal: d.literal, ignoreCase: d.ignoreCase, regex: d.regex, source: d.source };
}

const matchText = (text, matcher, opts = {}) => {
    const o = { ...opts };
    if (o.maxPerFile === Infinity) delete o.maxPerFile;
    return nMatchText(text, desc(matcher), o);
};

function tallyText(text, matcher, tally, file = null) {
    const r = nTally(text, desc(matcher), tally, file);
    for (const [k, v] of r.tally) {
        const t = tally.get(k);
        if (t) Object.assign(t, v);
        else tally.set(k, v);
    }
    return r.count;
}

module.exports = {
    globToRegExp,
    pathMatcher,
    compileMatcher,
    matchText,
    tallyText,
    indexTerms: f('indexTerms'),
    splitList: f('splitList'),
    clip: f('clip'),
};
