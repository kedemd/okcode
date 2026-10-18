'use strict';
// Loads okcode's native addon (Rust). @kedem/okcode bundles it per supported platform under native/<key>/okcode.node
// (with the standalone CLI binary next to it, native/<key>/okcode). Order: $OKCODE_NATIVE_BINARY (any okcode .node
// file), else the bundled binary for this platform.
const fs = require('fs');
const path = require('path');

const ROOT = path.join(__dirname, '..', '..');
const SUPPORTED = ['linux-x64-gnu'];

function platformKey() {
    if (process.platform !== 'linux') return `${process.platform}-${process.arch}`;
    let musl = false;
    try {
        musl = !(process.report && process.report.getReport().header.glibcVersionRuntime);
    } catch {}
    return `linux-${process.arch}-${musl ? 'musl' : 'gnu'}`;
}

function load() {
    const key = platformKey();
    const tried = [];
    const candidates = [];
    if (process.env.OKCODE_NATIVE_BINARY) candidates.push(process.env.OKCODE_NATIVE_BINARY);
    candidates.push(path.join(ROOT, 'native', key, 'okcode.node'));
    for (const file of candidates) {
        if (!fs.existsSync(file)) {
            tried.push(file);
            continue;
        }
        return require(file);
    }
    const supported = SUPPORTED.includes(key);
    const e = new Error(
        supported
            ? `okcode: its native addon for ${key} is missing (tried ${tried.join(', ')}); reinstall @kedem/okcode`
            : `okcode: ${key} is not supported yet — okcode ${require(path.join(ROOT, 'package.json')).version} ships a native addon for ${SUPPORTED.join(', ')} only (OKCODE_NATIVE_BINARY can point at an addon built for another platform)`,
    );
    e.code = supported ? 'OKCODE_NO_NATIVE' : 'OKCODE_UNSUPPORTED_PLATFORM';
    throw e;
}

module.exports = load();
module.exports.platformKey = platformKey;
