'use strict';
// Embedder PROFILES → okdb embedder configs (docs/DESIGN.md §8).
//
// A profile is what the host configures under a name:
//
//   { type: 'ollama' | 'openai' | <any okdb factory type>, model, url?, apiKey?, dims?, ...providerFields }
//   { embed: async (text | texts) => vector(s), id, embedBatch?, dims?, model? }    // custom
//
// and what okdb needs is an embedder CONFIG — a plain, persistable object whose
// `type` names a factory registered on the okdb instance. Two things in a
// profile cannot be persisted and must never reach okdb's records: a function
// that supplies the api key, and a custom embed function. Both become a
// FACTORY registered under a derived type (`okcode-<slug>`), so the persisted
// config says only "use the okcode factory for this profile" and the function
// itself lives in the process that registered it. A process that did not (a
// CLI that was not handed the function) cannot embed with that profile, and
// says so (`needs-config`) instead of guessing.
//
// A plain string apiKey is passed through as okdb's `api_key`: okdb stores it
// with the embedder record (masked in its admin UI). okcode's own profile
// record never holds it.
//
// IDENTITY (src/identity.js). Every profile carries the dims-free parts of its
// vector space — { type, endpoint, model } — and the store adds the dims. For
// a built-in provider they come from the config: the PROVIDER (not a derived
// factory type, which only says "okcode holds a function for this"), its
// url/base_url, its model. A custom `embed` function says nothing okcode can
// inspect about the space it embeds into, so the host must name it: `id` is
// REQUIRED on a custom profile (e.g. 'my-embed-service@v2') and stands in the
// endpoint slot — change it whenever the function starts producing different
// vectors, and okcode builds a new store instead of mixing the two. The
// profile NAME is deliberately not part of it: a name is the host's handle for
// switching between profiles, not a statement about what the vectors mean.

const chunk = require('./analysis/chunk');
const identity = require('./identity');

const DERIVED_PREFIX = 'okcode-';

// camelCase conveniences → the provider field names okdb's built-in drivers
// read. Everything else passes through untouched.
const FIELD_ALIASES = { apiKey: 'api_key', baseUrl: 'base_url' };

const derivedType = (name) => `${DERIVED_PREFIX}${chunk.slug(name) || 'profile'}`;

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
function customFactory(profile) {
    const hostEmbed = profile.embed;
    const hostBatch = typeof profile.embedBatch === 'function' ? profile.embedBatch : null;
    return () => ({
        async embed(input) {
            if (Array.isArray(input)) return this.embedBatch(input);
            const out = await hostEmbed(input);
            return toVector(isBatch(out) ? out[0] : out);
        },
        async embedBatch(texts) {
            const out = hostBatch ? await hostBatch(texts) : await hostEmbed(texts);
            if (isBatch(out) && out.length === texts.length) return out.map(toVector);
            if (texts.length === 1 && isVectorLike(out) && !isBatch(out)) return [toVector(out)];
            // The host's function is one-text-at-a-time after all.
            const each = [];
            for (const t of texts) {
                const v = await hostEmbed(t);
                each.push(toVector(isBatch(v) ? v[0] : v));
            }
            return each;
        },
    });
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

// Translate one host profile. Registers a derived factory on `db` when the
// profile carries a function. Returns:
//   profile  — the store profile { name, embedder, dims }
//   record   — what okcode persists about it: no functions, no secrets
function fromHost(db, name, cfg) {
    if (!name || typeof name !== 'string') throw new Error('an embedder profile needs a name');
    if (!cfg || typeof cfg !== 'object') throw new Error(`embedder "${name}": config must be an object`);
    const dims = Number.isInteger(cfg.dims) && cfg.dims > 0 ? cfg.dims : null;

    if (typeof cfg.embed === 'function') {
        if (typeof cfg.id !== 'string' || !cfg.id) {
            const err = new Error(
                `embedder "${name}": a custom embed profile needs an \`id\` naming its vector space (e.g. 'my-service@v2') — change it whenever the function's vectors change`,
            );
            err.code = 'OKCODE_NEEDS_ID';
            throw err;
        }
        const type = derivedType(name);
        db.embeddings.registerEmbedderFactory(type, customFactory(cfg), { label: `okcode custom (${name})` });
        const model = typeof cfg.model === 'string' && cfg.model ? cfg.model : null;
        const embedder = { type, model: model || type, ...(dims ? { dims } : {}) };
        const record = { name, kind: 'custom', type, provider: 'custom', id: cfg.id, model, dims, needs: 'embed' };
        return { profile: { name, embedder, dims, identity: partsOfRecord(record) }, record };
    }

    if (typeof cfg.type !== 'string' || !cfg.type) {
        throw new Error(`embedder "${name}": needs a type ('ollama', 'openai', …) or an embed function`);
    }
    const fields = {};
    let keySource = null;
    let keyFn = null;
    for (const [k, v] of Object.entries(cfg)) {
        if (k === 'type' || k === 'dims' || k === 'pipeline') continue;
        if (k === 'apiKey' || k === 'api_key') {
            if (typeof v === 'function') {
                keyFn = v;
                keySource = 'function';
            } else if (v != null && v !== '') {
                fields.api_key = String(v);
                keySource = 'string';
            }
            continue;
        }
        if (typeof v === 'function') throw new Error(`embedder "${name}": "${k}" cannot be a function`);
        fields[FIELD_ALIASES[k] || k] = v;
    }
    // What is safe to keep: every provider field except the key.
    const { api_key: _secret, ...safeFields } = fields;
    const pipeline = cfg.pipeline && typeof cfg.pipeline === 'object' ? cfg.pipeline : null;

    if (keyFn) {
        const type = derivedType(name);
        db.embeddings.registerEmbedderFactory(type, keyedFactory(db, cfg.type, keyFn), {
            label: `okcode ${cfg.type} (${name})`,
        });
        const embedder = { ...safeFields, type, ...(dims ? { dims } : {}) };
        const record = {
            name,
            kind: 'builtin',
            type,
            provider: cfg.type,
            model: safeFields.model || null,
            fields: safeFields,
            dims,
            apiKey: 'function',
            needs: 'apiKey',
            ...(pipeline ? { pipeline } : {}),
        };
        return {
            profile: { name, embedder, dims, identity: partsOfRecord(record), ...(pipeline ? { pipeline } : {}) },
            record,
        };
    }

    const embedder = { ...fields, type: cfg.type, ...(dims ? { dims } : {}) };
    const record = {
        name,
        kind: 'builtin',
        type: cfg.type,
        provider: cfg.type,
        model: safeFields.model || null,
        fields: safeFields,
        dims,
        apiKey: keySource, // 'string' (held by okdb's embedder record) | null
        needs: null,
        ...(pipeline ? { pipeline } : {}),
    };
    return {
        profile: { name, embedder, dims, identity: partsOfRecord(record), ...(pipeline ? { pipeline } : {}) },
        record,
    };
}

// The dims-free identity parts of a persisted profile record.
function partsOfRecord(rec) {
    if (!rec) return null;
    if (rec.kind === 'custom')
        return { type: 'custom', endpoint: String(rec.id || ''), model: String(rec.model || '') };
    const fields = rec.fields || {};
    return {
        type: String(rec.provider || rec.type || ''),
        endpoint: identity.endpointOf(fields),
        model: String(fields.model || rec.model || ''),
    };
}

// What a host sees about a profile's vector space: { type, endpoint, model,
// dims, identity } — identity null until the dims are known.
function describe(rec, dims = null) {
    const parts = partsOfRecord(rec) || { type: null, endpoint: '', model: '' };
    const d = Number.isInteger(dims) && dims > 0 ? dims : Number.isInteger(rec && rec.dims) ? rec.dims : null;
    return {
        type: parts.type || null,
        endpoint: parts.endpoint || null,
        model: parts.model || null,
        dims: d,
        identity: identity.identityOf(parts, d),
    };
}

// Rebuild a profile from what okcode persisted, for a process whose host did
// not configure it this time. A profile that needs a function the host did
// not hand over comes back with `needsConfig` and no store profile.
//
// A string api key is not in okcode's record; the pipeline's embedder engine
// record (okdb's) still holds it, and an existing pipeline is reused as-is —
// so the profile keeps working. A NEW workspace would create its embedder
// from these fields without the key: the host must re-supply it for that.
function fromRecord(db, rec) {
    if (!rec || !rec.name) return null;
    if (rec.needs) {
        return {
            profile: null,
            record: rec,
            needsConfig: true,
            reason:
                rec.needs === 'embed'
                    ? `custom embed function for "${rec.name}" not supplied to this process`
                    : `apiKey function for "${rec.name}" not supplied to this process`,
        };
    }
    const embedder = { ...(rec.fields || {}), type: rec.type, ...(rec.dims ? { dims: rec.dims } : {}) };
    return {
        profile: {
            name: rec.name,
            embedder,
            dims: rec.dims || null,
            identity: partsOfRecord(rec),
            ...(rec.pipeline ? { pipeline: rec.pipeline } : {}),
        },
        record: rec,
        needsConfig: false,
    };
}

// The model name the store derives the pipeline name from (store.js modelOf).
const modelOf = (embedder) => (embedder && (embedder.model || embedder.type)) || null;

module.exports = { fromHost, fromRecord, partsOfRecord, describe, derivedType, modelOf, DERIVED_PREFIX };
