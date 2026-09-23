'use strict';
// THE IDENTITY OF A VECTOR SPACE (docs/DESIGN.md §8).
//
// A vector is only meaningful next to vectors from the same embedder: the same
// model, served by the same provider at the same endpoint, at the same
// dimensionality. Two profiles that agree on the model NAME and dims but not
// on where it runs (ollama vs an OpenAI-compatible server, or a model swapped
// behind the same name on another host) produce vectors that look healthy and
// mean nothing next to each other. So a profile's store is addressed by the
// full identity — (type, endpoint, model, dims) — never by the model name
// alone, and a change to any part addresses a NEW store: old vectors are never
// served for the new space.
//
//   identity  JSON.stringify([type, endpoint, model, dims])
//             type      the provider ('ollama', 'openai', an okdb factory type),
//                       or 'custom' for an `embed` function
//             endpoint  the provider's url / base_url ('' = the provider's
//                       default), or a custom profile's host-chosen `id`
//             model     the model name ('' when unset)
//             dims      the vector length
//
// The string is deliberately the same shape a host can compute from its own
// embedder config (the brain's query-embed.js identityOf), so a host holding a
// vector can decide by plain equality whether okcode's store lives in the same
// space. No normalisation: `http://h:11434` and `http://h:11434/` are
// different strings, and treating them as one is the host's call to make.
//
// The pipeline NAME keeps readable parts (model slug and dims — what a person
// scanning okdb's admin sees) plus a short hash of the full identity, which is
// what actually separates two spaces: `code_<slug(model)>_<dims>_<hash8>`.

const crypto = require('crypto');
const chunk = require('./analysis/chunk');

const PREFIX = 'code_';

// The endpoint fields okdb's built-in drivers read, in precedence order.
const endpointOf = (cfg) => String((cfg && (cfg.url || cfg.base_url)) || '');

// { type, endpoint, model } — the dims-free part of an identity, taken from a
// plain embedder config (okdb shape). A store profile may carry its own parts
// (okcode's profiles do: the provider, not a derived factory type).
function partsOf(embedder) {
    const e = embedder || {};
    return { type: String(e.type || ''), endpoint: endpointOf(e), model: String(e.model || '') };
}

function identityOf(parts, dims) {
    if (!parts || !parts.type || !Number.isInteger(dims) || dims <= 0) return null;
    return JSON.stringify([String(parts.type), String(parts.endpoint || ''), String(parts.model || ''), dims]);
}

const shortHash = (s) => crypto.createHash('sha1').update(String(s)).digest('hex').slice(0, 8);

// The readable part of the name: the model, or the type when there is none.
const nameSlug = (parts) => chunk.slug(parts.model || parts.type) || 'embedder';

function pipelineName(parts, dims) {
    const identity = identityOf(parts, dims);
    if (!identity) throw new Error(`pipelineName needs a type and integer dims (got ${parts && parts.type}, ${dims})`);
    return `${PREFIX}${nameSlug(parts)}_${dims}_${shortHash(identity)}`;
}

// The pipeline, among `names`, that holds this identity — for a caller that
// does not know the dims yet (a passive process, a profile without `dims`):
// every candidate's dims are read off its name and the hash re-checked, so a
// same-named model from another endpoint is never mistaken for this one.
function findPipeline(names, parts, dims = null) {
    if (Number.isInteger(dims) && dims > 0) {
        const want = pipelineName(parts, dims);
        return names.includes(want) ? { pipeline: want, dims } : null;
    }
    const re = new RegExp(`^${PREFIX}${nameSlug(parts)}_(\\d+)_([0-9a-f]{8})$`);
    for (const n of names) {
        const m = re.exec(n);
        if (m && shortHash(identityOf(parts, Number(m[1]))) === m[2]) return { pipeline: n, dims: Number(m[1]) };
    }
    return null;
}

// Every name okcode would have given a pipeline (any scheme it has used) —
// what orphan detection considers ours.
const isOurs = (name) => String(name || '').startsWith(PREFIX);

module.exports = { partsOf, identityOf, pipelineName, findPipeline, shortHash, isOurs, endpointOf };
