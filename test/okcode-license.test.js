'use strict';
// Licensing: okcode.open({ path, license }) installs the license into its own
// store (okdb's db.licenses) before creating anything; a reopen is idempotent;
// a host-supplied db owns its licensing, so `license` is ignored there. The
// CLI's --license does the same for the store it opens.
//
// Licenses are signed with a per-file Ed25519 test key that okdb is told to
// trust — the approach okdb's own license tests use (its checker's mutable
// _C constants). That needs okdb's unbundled sources (a dev checkout linked as
// @kedem/okdb). The published package is one bundle that trusts only the
// vendor key, so there the tests that need a VALID license skip; the
// invalid-license and host-db tests run everywhere.

const { describe, it, before, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const { generateKeyPairSync, createHash, sign: cryptoSign } = require('node:crypto');
const OKDB = require('@kedem/okdb');
const okcode = require('../src/okcode');
const { tmpRoot, writeFixture } = require('./fixtures/code-fixture');

function devSources() {
    try {
        return {
            licensePath: require.resolve('@kedem/okdb/lib/okdb-license'),
            _C: require('@kedem/okdb/lib/okdb-license')._C,
            codec: require('@kedem/okdb/lib/okdb-license-codec'),
        };
    } catch {
        return null;
    }
}
const DEV = devSources();
const NO_TEST_KEY = DEV
    ? false
    : 'the published @kedem/okdb bundle trusts only the vendor key: no test license can be signed';
const codec = DEV && DEV.codec;

const { privateKey, publicKey } = generateKeyPairSync('ed25519');
const PRIV = privateKey.export({ type: 'pkcs8', format: 'pem' });
const PUB = publicKey.export({ type: 'spki', format: 'pem' });
const FP = createHash('sha256')
    .update(publicKey.export({ type: 'spki', format: 'der' }))
    .digest()
    .slice(0, 8)
    .toString('hex');
if (DEV) {
    DEV._C.PUBLIC_KEY_PEM = PUB;
    DEV._C.PUB_FINGERPRINT = FP;
}

function makeLicense({ licenseType = codec.LICENSE_TYPE_OPEN, licensee = 'Okcode Test' } = {}) {
    const payload = codec.encodeLicensePayload({
        issuedAt: Date.now(),
        expiresAt: Date.now() + 90 * 86400000,
        licensee,
        features: { engines: true, embeddings: true, fts: true },
        limits: { envs: 100 },
        licenseType,
    });
    const sig = cryptoSign(null, payload, PRIV);
    return { blob: codec.encodeLicenseBlob(payload, sig), rawBuf: Buffer.concat([payload, sig]) };
}

function activationFor(rawBuf, nodeId) {
    const lhash = codec.licenseHash(rawBuf);
    const { pinHmac } = codec.derivePin(nodeId, lhash, FP);
    const expiry = Math.floor(Date.now() / 1000) + 86400;
    const e = Buffer.alloc(4);
    e.writeUInt32BE(expiry, 0);
    const msg = Buffer.concat([Buffer.from([0x01]), pinHmac.slice(0, 5), lhash, e]);
    return codec.encodeActivationToken(lhash, expiry, cryptoSign(null, msg, PRIV));
}

const collect = () => {
    const lines = [];
    return { lines, log: { info: () => {}, warn: (m) => lines.push(m), error: (m) => lines.push(m) } };
};

describe('okcode.open({ license })', () => {
    let base;
    before(() => {
        ({ base } = tmpRoot('lic'));
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));
    const store = (name) => path.join(base, name);

    it(
        'installs the license into its own store; effective at once; a reopen is idempotent',
        { skip: NO_TEST_KEY },
        async () => {
            const { blob } = makeLicense();
            let oc = await okcode.open({ path: store('own'), license: blob });
            let id;
            try {
                const list = oc.db.licenses.list();
                assert.equal(list.length, 1);
                id = list[0].id;
                assert.equal(oc.db.licenses.effective().id, id);
                const lic = (await oc.status()).license;
                assert.equal(lic.status, 'active');
                assert.equal(lic.type, 'open');
                assert.equal(lic.licensee, 'Okcode Test');
                assert.ok(!JSON.stringify(lic).includes(blob.slice(10, 60)));
            } finally {
                await oc.close();
            }
            // Same license again: no second record, same id.
            oc = await okcode.open({ path: store('own'), license: blob });
            try {
                const list = oc.db.licenses.list();
                assert.equal(list.length, 1);
                assert.equal(list[0].id, id);
            } finally {
                await oc.close();
            }
            // Without the option: still licensed (the store keeps it).
            oc = await okcode.open({ path: store('own') });
            try {
                assert.equal((await oc.status()).license.status, 'active');
            } finally {
                await oc.close();
            }
        },
    );

    it(
        'a node-bound license that needs activation is reported, not fatal; { blob, activation } activates',
        { skip: NO_TEST_KEY },
        async () => {
            const { blob, rawBuf } = makeLicense({ licenseType: codec.LICENSE_TYPE_STANDARD });
            const { lines, log } = collect();
            let oc = await okcode.open({ path: store('std'), license: blob, log });
            let nodeId;
            try {
                const lic = (await oc.status()).license;
                assert.equal(lic.status, 'needs-activation');
                assert.match(lic.pin, /^[0-9A-Z-]+$/);
                assert.ok(lines.some((m) => m.includes('needs activation') && m.includes(lic.pin)));
                nodeId = oc.db.id;
            } finally {
                await oc.close();
            }
            oc = await okcode.open({
                path: store('std'),
                license: { blob, activation: activationFor(rawBuf, nodeId) },
            });
            try {
                assert.equal((await oc.status()).license.status, 'active');
                assert.equal(oc.db.licenses.list().length, 1);
            } finally {
                await oc.close();
            }
        },
    );

    it('an invalid license refuses to open (LICENSE_INVALID) and leaves the store closed', async () => {
        // A well-formed blob signed by a key nobody trusts; where okdb's
        // codec is not reachable (the published bundle), a malformed one.
        let forged = 'OKDB-LICENSE-forged-not-a-license';
        if (codec) {
            const other = generateKeyPairSync('ed25519').privateKey.export({ type: 'pkcs8', format: 'pem' });
            const payload = codec.encodeLicensePayload({
                issuedAt: Date.now(),
                expiresAt: Date.now() + 86400000,
                licensee: 'Forged',
                features: {},
                limits: {},
                licenseType: codec.LICENSE_TYPE_OPEN,
            });
            forged = codec.encodeLicenseBlob(payload, cryptoSign(null, payload, other));
        }
        await assert.rejects(okcode.open({ path: store('bad'), license: forged }), (e) => e.code === 'LICENSE_INVALID');
        // Closed: the same path opens again in this process.
        const oc = await okcode.open({ path: store('bad') });
        try {
            assert.equal(oc.db.licenses.list().length, 0);
            assert.equal((await oc.status()).license.status, 'free');
        } finally {
            await oc.close();
        }
    });

    it('with a host db, `license` is ignored (the host owns licensing)', async () => {
        const db = new OKDB(store('host'));
        await db.open();
        const { lines, log } = collect();
        // Ignored before it is ever parsed, so any blob will do where no test
        // key can sign one.
        const blob = DEV ? makeLicense().blob : 'OKDB-LICENSE-unsigned-placeholder';
        const oc = await okcode.open({ db, license: blob, log });
        try {
            assert.equal(db.licenses.list().length, 0);
            assert.ok(lines.some((m) => /license.*ignored/.test(m)));
            assert.equal((await oc.status()).license.status, 'free');
        } finally {
            await oc.close();
            await db.close();
        }
    });
});

describe('cli --license', { skip: NO_TEST_KEY }, () => {
    let base;
    let root;
    let preload;
    before(() => {
        ({ base, root } = tmpRoot('liccli'));
        writeFixture(root);
        // The child trusts this file's test key too (same _C override, preloaded).
        preload = path.join(base, 'trust-test-key.js');
        fs.writeFileSync(
            preload,
            `const { _C } = require(${JSON.stringify(DEV.licensePath)});\n` +
                `_C.PUBLIC_KEY_PEM = ${JSON.stringify(PUB)};\n_C.PUB_FINGERPRINT = ${JSON.stringify(FP)};\n`,
        );
    });
    after(() => fs.rmSync(base, { recursive: true, force: true }));

    const run = (args) => {
        const r = spawnSync(
            process.execPath,
            ['-r', preload, path.join(__dirname, '..', 'bin', 'okcode.js'), ...args, '--root', root, '--json'],
            { encoding: 'utf8', timeout: 120000, env: { ...process.env, OKCODE_VERBOSE: '', OKDB_LICENSE_FILE: '' } },
        );
        return { code: r.status, stdout: r.stdout, stderr: r.stderr };
    };

    it('installs the license file into the store; idempotent; a bad file fails with the code', () => {
        const { blob } = makeLicense({ licensee: 'Cli Co' });
        const file = path.join(base, 'okcode.license');
        fs.writeFileSync(file, `${blob}\n`);
        const a = run(['status', '--license', file]);
        assert.equal(a.code, 0, a.stderr);
        assert.match(a.stderr, /license installed: open \(Cli Co\)/);
        assert.equal(JSON.parse(a.stdout).license.status, 'active');
        const b = run(['status', '--license', file]);
        assert.equal(b.code, 0, b.stderr);
        assert.doesNotMatch(b.stderr, /license installed/);
        assert.equal(JSON.parse(b.stdout).license.status, 'active');

        const bad = path.join(base, 'bad.license');
        fs.writeFileSync(bad, 'not a license');
        const c = run(['status', '--license', bad]);
        assert.equal(c.code, 1);
        assert.match(c.stderr, /LICENSE_INVALID/);
        assert.ok(!c.stderr.includes('not a license'));
    });
});
