'use strict';
// Embedder profiles. Profile translation is native; what stays here is the glue that cannot cross
// into the native core as data: the okdb factories wrapping the host's embed / apiKey functions, and their
// registration on a db.
const { pure } = require('./native/pure');
const f = pure('embedders');

const DERIVED_PREFIX = 'okcode-';
const derivedType = f('derivedType');
const partsOfRecord = f('partsOfRecord');
const describe = f('describe');
const fromRecordNative = f('fromRecord');
const modelOf = f('modelOf');
const nFromHost = f('fromHost');
const nFromHostCustom = f('fromHostCustom');

// A vector as okdb wants it. Hosts return plain arrays as often as typed ones.
function toVector(v) {
    if (v instanceof Float32Array) return v;
    if (Array.isArray(v) || ArrayBuffer.isView(v)) return Float32Array.from(v);
    throw new Error('embedder returned something that is not a vector');
}
const isVectorLike = (v) => Array.isArray(v) || ArrayBuffer.isView(v);
// A batch of vectors (not a single vector of numbers).
const isBatch = (v) => Array.isArray(v) && v.length > 0 && isVectorLike(v[0]);

// A built-in provider's factory, captured when the profile is registered.
function baseFactory(db, type) {
    const f = db.embeddings?.getEmbedderFactory?.(type) ?? null;
    if (!f) {
        const err = new Error(
            `okdb has no embedder factory "${type}" (register it with db.embeddings.registerEmbedderFactory)`,
        );
        err.code = 'OKCODE_UNKNOWN_EMBEDDER_TYPE';
        throw err;
    }
    return f;
}

// The okdb factory for a CUSTOM embed function. okdb's embedder driver calls
// `embed(text)` for single inputs and `embedBatch(texts)` when present; the
// host's function may be written either way ("texts in, vectors out" or one
// text in, one vector out), so both shapes are accepted on the way back.
//
// CONTEXT. The host's function is called as `embed(texts, { workspace })`:
// one profile embeds for every workspace, and which one's text this is (its
// id; null for an engine not tied to one) is what a host needs to decide
// where that text may go. The store puts the id in each workspace's embedder
// config; a function that takes one argument ignores it.
function customFactory(profile) {
    const hostEmbed = profile.embed;
    const hostBatch = typeof profile.embedBatch === 'function' ? profile.embedBatch : null;
    return (config) => {
        const ctx = Object.freeze({ workspace: (config && config.workspace) || null });
        return {
            async embed(input) {
                if (Array.isArray(input)) return this.embedBatch(input);
                const out = await hostEmbed(input, ctx);
                return toVector(isBatch(out) ? out[0] : out);
            },
            async embedBatch(texts) {
                const out = hostBatch ? await hostBatch(texts, ctx) : await hostEmbed(texts, ctx);
                if (isBatch(out) && out.length === texts.length) return out.map(toVector);
                if (texts.length === 1 && isVectorLike(out) && !isBatch(out)) return [toVector(out)];
                // The host's function is one-text-at-a-time after all.
                const each = [];
                for (const t of texts) {
                    const v = await hostEmbed(t, ctx);
                    each.push(toVector(isBatch(v) ? v[0] : v));
                }
                return each;
            },
        };
    };
}

// The okdb factory for a built-in provider whose api key is a FUNCTION. The
// function is called per request (a rotated key takes effect on the next
// batch) and the provider's own factory is built with the key in hand — the
// key exists only in memory, for the life of that request.
function keyedFactory(db, baseType, keyFn) {
    const base = baseFactory(db, baseType);
    return (config, okdb) => {
        const make = async () => {
            const key = await keyFn();
            if (!key) throw new Error(`apiKey() returned no key for ${baseType}`);
            return base({ ...config, type: baseType, api_key: key }, okdb);
        };
        return {
            async embed(input) {
                return (await make()).embed(input);
            },
            async embedBatch(texts) {
                const inner = await make();
                if (typeof inner.embedBatch === 'function') return inner.embedBatch(texts);
                const out = [];
                for (const t of texts) out.push(await inner.embed(t));
                return out;
            },
        };
    };
}

// The derived factories of the host's function profiles, registered on a db
// that is NOT open yet. okdb restarts a store's persisted embedder engines
// inside open(), before fromHost below runs: a custom or keyed profile's type
// was unknown at that moment ("Unknown embedder type"), its engine failed to
// start, and asking by meaning stayed broken after every restart. Registering
// is idempotent (fromHost registers the same type again after open). A keyed
// profile whose base type the db does not know yet is left to fromHost.
function preRegister(db, hostProfiles) {
    for (const [name, cfg] of Object.entries(hostProfiles || {})) {
        if (!cfg || typeof cfg !== 'object') continue;
        try {
            if (typeof cfg.embed === 'function' && typeof cfg.id === 'string' && cfg.id) {
                db.embeddings.registerEmbedderFactory(derivedType(name), customFactory(cfg), { label: `okcode custom (${name})` });
            } else if (typeof cfg.type === 'string' && cfg.type) {
                const keyFn = typeof cfg.apiKey === 'function' ? cfg.apiKey : typeof cfg.api_key === 'function' ? cfg.api_key : null;
                if (!keyFn) continue;
                db.embeddings.registerEmbedderFactory(derivedType(name), keyedFactory(db, cfg.type, keyFn), {
                    label: `okcode ${cfg.type} (${name})`,
                });
            }
        } catch {
            /* fromHost, after open, registers it or says why it cannot */
        }
    }
}

// Translate one host profile. Registers a derived factory on `db` when the
// profile carries a function. Returns { profile, record }: the profile okdb is given and the record
// okcode persists (never a function or a function-sourced secret).
function fromHost(db, name, cfg) {
    if (!name || typeof name !== 'string') throw new Error('an embedder profile needs a name');
    if (!cfg || typeof cfg !== 'object') throw new Error(`embedder "${name}": config must be an object`);

    if (typeof cfg.embed === 'function') {
        const r = nFromHostCustom(name, cfg);
        db.embeddings.registerEmbedderFactory(r.record.type, customFactory(cfg), { label: `okcode custom (${name})` });
        return r;
    }
    let keyFn = null;
    if (typeof cfg.type === 'string' && cfg.type) {
        for (const [k, v] of Object.entries(cfg)) {
            if (k === 'type' || k === 'dims' || k === 'pipeline') continue;
            if (k === 'apiKey' || k === 'api_key') {
                if (typeof v === 'function') keyFn = v;
                continue;
            }
            if (typeof v === 'function') throw new Error(`embedder "${name}": "${k}" cannot be a function`);
        }
    }
    const r = nFromHost(name, cfg, !!keyFn);
    if (keyFn) {
        db.embeddings.registerEmbedderFactory(r.record.type, keyedFactory(db, cfg.type, keyFn), {
            label: `okcode ${cfg.type} (${name})`,
        });
    }
    return r;
}

// Rebuild a profile from what okcode persisted (native).
const fromRecord = (db, rec) => fromRecordNative(rec);

module.exports = { preRegister, fromHost, fromRecord, partsOfRecord, describe, derivedType, modelOf, DERIVED_PREFIX };
