'use strict';
// Golden recorder (tier 2). Preloaded into a JS test run:
//
//   GOLDEN_OUT=<file.jsonl> node --require ./rs/golden/record.js --test test/*.test.js
//
// Wraps the pure analysis entry points BEFORE analysis/index.js copies them,
// and appends one JSON line per distinct call: { fn, args, out } (or { fn,
// args, err }). The Rust ports are checked against these lines — no Node at
// Rust test time. `node rs/golden/build.js` dedupes and packs them.
const fs = require('fs');
const path = require('path');

const OUT = process.env.GOLDEN_OUT;
if (OUT) {
    const SRC = path.resolve(__dirname, '../../src/analysis');
    const seen = new Set();
    const fd = fs.openSync(OUT, 'a');
    const crypto = require('crypto');

    // Buffers/Maps/Sets/RegExps flattened so a line round-trips through JSON.
    const plain = (v) =>
        JSON.parse(
            JSON.stringify(v, (k, x) => {
                if (x instanceof Map) return { $map: [...x.entries()] };
                if (x instanceof Set) return { $set: [...x] };
                if (x instanceof RegExp) return { $re: x.source, flags: x.flags };
                if (x && x.type === 'Buffer' && Array.isArray(x.data)) return { $buf: Buffer.from(x.data).toString('base64') };
                if (typeof x === 'function') return undefined;
                return x;
            }),
        );

    const record = (fn, args, run) => {
        let line;
        let result;
        let error = null;
        try {
            result = run();
        } catch (e) {
            error = e;
        }
        try {
            const a = plain(args);
            const key = crypto.createHash('sha1').update(fn).update(JSON.stringify(a)).digest('hex');
            if (!seen.has(key)) {
                seen.add(key);
                line = error ? { fn, args: a, err: String(error && error.message) } : { fn, args: a, out: plain(result) };
                fs.writeSync(fd, JSON.stringify(line) + '\n');
            }
        } catch {
            /* unserialisable call: not a golden */
        }
        if (error) throw error;
        return result;
    };

    const wrap = (mod, name, fnName = name) => {
        const orig = mod[name];
        if (typeof orig !== 'function') return;
        mod[name] = function (...args) {
            return record(fnName, args, () => orig.apply(this, args));
        };
    };

    // Order matters: patch the leaf modules' exports before index.js reads them.
    const parse = require(path.join(SRC, 'parse.js'));
    const validate = require(path.join(SRC, 'validate.js'));
    const chunk = require(path.join(SRC, 'chunk.js'));
    const text = require(path.join(SRC, 'text.js'));
    wrap(parse, 'extract');
    wrap(parse, 'langOf');
    wrap(validate, 'validateSource');
    wrap(chunk, 'regions');
    wrap(chunk, 'sectionOf');
    wrap(chunk, 'chunkCode');
    wrap(chunk, 'prepareText');
    const wrapAll = (mod, prefix) => {
        for (const k of Object.keys(mod)) if (typeof mod[k] === 'function') wrap(mod, k, `${prefix}.${k}`);
    };
    wrapAll(text, 'text');
    wrapAll(require(path.join(SRC, 'template-balance.js')), 'templateBalance');
    wrapAll(require(path.join(SRC, 'convention-color.js')), 'conventionColor');
    wrapAll(require(path.join(SRC, 'packages.js')), 'packages');
    wrapAll(require(path.join(SRC, '../grep.js')), 'grep');
    wrapAll(require(path.join(SRC, '../identity.js')), 'identity');
}
