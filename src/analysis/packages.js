'use strict';
// Dependency package surfaces. The .d.ts scanning, manifest reading and surface derivation are
// native; this file only awaits the async access facade. The native core runs its sequence of reads
// against what is fetched so far and, at the first batch it lacks, answers `{ need: [paths] }`; this
// file fetches exactly that batch and asks again. So the facade sees a fixed number of batched round
// trips (1 + 3 for a scan).
const { pure } = require('../native/pure');

const f = pure('analysis/packages');
const namesFromDts = f('namesFromDts');
const membersFromDts = f('membersFromDts');
const reExports = f('reExports');
const declared = f('declared');
const nativeRead = f('readPackages');
const nativeScan = f('scanPackages');

// Run `step(cache)` until it stops asking for files; `cache` is [[path, text|null]…].
async function drive(access, step) {
    const cache = [];
    for (;;) {
        const r = step(cache);
        if (!r.need) return r;
        const got = await access.read(r.need);
        for (const p of r.need) {
            const buf = got.get(p);
            cache.push([p, buf ? buf.toString('utf8') : null]);
        }
    }
}

async function readPackages(access, names, sourceOf = () => 'transitive') {
    const list = [...names];
    const sources = {};
    for (const n of list) sources[n] = sourceOf(n);
    const r = await drive(access, (cache) => nativeRead(list, sources, cache));
    return r.packages;
}

async function scanPackages(access) {
    const r = await drive(access, (cache) => nativeScan(cache));
    return r.result;
}

module.exports = { scanPackages, readPackages, namesFromDts, membersFromDts, reExports, declared };
