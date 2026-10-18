'use strict';
// JavaScript values → JSON for the native core. Values JSON cannot express are marked: Buffer is
// {"$buf": base64}, Map {"$map": [[k, v]…]}, Set {"$set": […]}, RegExp {"$re": source, flags}.
// Typed arrays become plain arrays; functions and undefined are dropped (undefined in an array →
// null).
function toJson(v, seen = new Set()) {
    if (v === undefined || typeof v === 'function' || typeof v === 'symbol') return undefined;
    if (v !== null && typeof v === 'object') {
        // JSON can't carry a cycle: fail like JSON.stringify, not with a stack overflow
        if (seen.has(v)) throw new TypeError('Converting circular structure to JSON');
        seen.add(v);
        try {
            return encode(v, seen);
        } finally {
            seen.delete(v);
        }
    }
    return encode(v, seen);
}
function encode(v, seen) {
    if (v === null || typeof v !== 'object') return typeof v === 'bigint' ? Number(v) : v;
    if (Buffer.isBuffer(v)) return { $buf: v.toString('base64') };
    if (ArrayBuffer.isView(v)) return Array.from(v);
    if (v instanceof RegExp) return { $re: v.source, flags: v.flags };
    if (v instanceof Map) return { $map: [...v].map(([k, x]) => [toJson(k, seen), toJson(x, seen) ?? null]) };
    if (v instanceof Set) return { $set: [...v].map((x) => toJson(x, seen) ?? null) };
    if (v instanceof Date) return v.toISOString();
    if (Array.isArray(v)) return v.map((x) => toJson(x, seen) ?? null);
    const out = {};
    for (const k of Object.keys(v)) {
        const x = toJson(v[k], seen);
        if (x !== undefined) out[k] = x;
    }
    return out;
}
const args = (a) => a.map((x) => toJson(x) ?? null);

// The inverse, for results: the native side marks what JSON can't say. An object is revived only when
// its keys are exactly a marker's keys, so a plain result object that happens to have a `$buf` key
// among others stays as is.
//   {"$arr": [...], "$props": {...}}  → an Array carrying those extra properties (paged results:
//                                       total, offset, truncated, next — returned as arrays)
//   {"$buf": b64} → Buffer, {"$map": [[k, v]…]} → Map, {"$set": […]} → Set
const only = (o, ...keys) => {
    const ks = Object.keys(o);
    return ks.length === keys.length && keys.every((k) => ks.includes(k));
};
function fromJson(v) {
    if (v === null || typeof v !== 'object') return v;
    if (Array.isArray(v)) return v.map(fromJson);
    if (only(v, '$arr', '$props') && Array.isArray(v.$arr)) {
        const out = v.$arr.map(fromJson);
        for (const [k, x] of Object.entries(v.$props || {})) out[k] = fromJson(x);
        return out;
    }
    if (only(v, '$arr') && Array.isArray(v.$arr)) return v.$arr.map(fromJson);
    if (only(v, '$buf') && typeof v.$buf === 'string') return Buffer.from(v.$buf, 'base64');
    if (only(v, '$map') && Array.isArray(v.$map)) return new Map(v.$map.map(([k, x]) => [fromJson(k), fromJson(x)]));
    if (only(v, '$set') && Array.isArray(v.$set)) return new Set(v.$set.map(fromJson));
    const out = {};
    for (const k of Object.keys(v)) out[k] = fromJson(v[k]);
    return out;
}
module.exports = { toJson, fromJson, args };
