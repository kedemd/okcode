'use strict';
// Native errors arrive as Errors whose message is JSON {"$okcode":1,code,message,extra}.
// Rebuild the documented JavaScript error: message, `code` and any extra fields.
function fromNative(e) {
    if (!e || typeof e.message !== 'string' || !e.message.startsWith('{"$okcode"')) return e;
    let o;
    try {
        o = JSON.parse(e.message);
    } catch {
        return e;
    }
    const err = new Error(o.message);
    if (o.code) err.code = o.code;
    if (o.extra) Object.assign(err, o.extra);
    return err;
}
function rethrow(e) {
    throw fromNative(e);
}
// Sync native call → rethrow mapped.
function sync(fn) {
    try {
        return fn();
    } catch (e) {
        throw fromNative(e);
    }
}
module.exports = { fromNative, rethrow, sync };
