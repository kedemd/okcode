/*
 * okdb_bridge.h — the stable C ABI of okdb's native addon (okdb-node.<platform>.node).
 *
 * A program that wants okdb without compiling it in dlopen()s the .node that @kedem/okdb ships, looks up `okdb_bridge`, and
 * talks to okdb through the function table below. Requests and replies are MessagePack; see rs/okdb-bridge/README.md for the
 * full contract (wire format, threading, host callbacks, error codes).
 *
 *   void *so = dlopen(".../okdb-node.linux-x64-gnu.node", RTLD_NOW | RTLD_LOCAL);   // RTLD_LAZY outside Node: see README
 *   okdb_bridge_fn get = (okdb_bridge_fn)dlsym(so, "okdb_bridge");
 *   const OkdbBridgeV1 *ok = get ? get(OKDB_BRIDGE_ABI_MAJOR) : NULL;               // NULL: this .node does not speak ABI 1
 *   if (!ok || ok->struct_size < offsetof(OkdbBridgeV1, cli_main) + sizeof ok->cli_main) { ... too old ... }
 *
 * Versioning: within one abi_major the table, the host vtable and every message shape only GROW (fields are appended, never
 * removed or reordered; new request methods and reply keys may appear). A consumer checks `struct_size` before using a field
 * that came in a later abi_minor. A breaking change is a new abi_major (a new `OkdbBridgeV2`), and okdb_bridge(1) keeps
 * answering as long as that major is supported.
 *
 * Pure C99, no okdb or Rust types. Every function is safe to call with NULL out-pointers (the reply is then dropped).
 */
#ifndef OKDB_BRIDGE_H
#define OKDB_BRIDGE_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OKDB_BRIDGE_ABI_MAJOR 1u
#define OKDB_BRIDGE_ABI_MINOR 2u

/* ── status codes (the return value of every call; stable within a major) ────────────────────────────────────────────────────
 * OKDB_OK: *out holds {"ok": value}. Any other status: *out (when non-NULL) holds {"err": {"code", "message", "details"?}},
 * whose `code` is a stable string (README "Error codes"); the status is the coarse class for a fast check. */
typedef int32_t okdb_status;
#define OKDB_OK           0 /* success */
#define OKDB_ERR          1 /* the operation ran and failed (okdb error: NOT_FOUND, VALIDATION_FAILED, NOT_IMPLEMENTED_NATIVE, ...) */
#define OKDB_INVALID      2 /* unknown handle, malformed request, bad argument, struct_size too small */
#define OKDB_WRONG_THREAD 3 /* the call must run on the handle's owner thread */
#define OKDB_CLOSED       4 /* the handle is closed or closing */
#define OKDB_PANIC        5 /* an internal error was caught at the boundary (a bug: please report) */
#define OKDB_UNSUPPORTED  6 /* not available in this build */

/* ── open() flags ─────────────────────────────────────────────────────────────────────────────────────────────────────────── */
/* okdb runs its own owner thread behind the handle: submit() needs nobody to pump, dispatch() from any thread is forwarded to
 * that thread (blocking), host callbacks run on it. Without the flag the CALLING thread is the owner and must call pump(). */
#define OKDB_OPEN_OWNER_THREAD 0x1u
/* construct the instance but do not open it yet: register callbacks first (set_host, chunk strategies, embedder factories
 * that existing pipelines need at boot), then send ["bridge", nil, "open", []]. */
#define OKDB_OPEN_DEFER 0x2u

/* A bridge handle. 0 is never a valid handle. */
typedef uint64_t okdb_handle;

/* Completion of submit(): runs on the handle's owner thread. `reply` (an {"ok"} / {"err"} envelope) is valid only during the
 * call; copy what you need. */
typedef void (*okdb_done_fn)(void *ctx, okdb_status status, const uint8_t *reply, size_t reply_len);

/* ── host callbacks (set_host) ────────────────────────────────────────────────────────────────────────────────────────────────
 * okdb calls back into the consumer for every callback-backed feature: functions passed as {"$fn": id} markers in requests
 * (chunk strategies, embedder factories, preparers, queue / processor handlers, ...), the objects those return (embedders,
 * algorithms, storage adapters: {"$id": n}), and event subscriptions. `op` names the callback kind ("call", "emb", "event",
 * ...; README "Host ops"), `args` is a MessagePack array. The host answers with an {"ok": value} or {"err": {...}} envelope in a
 * buffer it allocated; okdb copies it and hands it back through free_reply. Called on the owner thread only. */
typedef struct OkdbBridgeHostV1 {
    uint32_t struct_size; /* sizeof(OkdbBridgeHostV1) as the consumer compiled it */
    uint32_t reserved;    /* 0 */
    void *ctx;            /* passed back to every function below */
    /* required. Return OKDB_OK with *out and *out_len set to the reply envelope; any other status = the callback failed
     * (okdb reports HOST_CALLBACK_FAILED unless *out holds an {"err"} envelope). */
    okdb_status (*call)(void *ctx, const char *op, const uint8_t *args, size_t args_len, uint8_t **out, size_t *out_len);
    /* optional: okdb dropped its last reference to callback / object id `id` */
    void (*release)(void *ctx, uint64_t id);
    /* optional: free a reply buffer returned by call (NULL: okdb leaves it to the consumer, e.g. static buffers) */
    void (*free_reply)(void *ctx, uint8_t *ptr, size_t len);
    /* optional: the host was replaced or the handle closed; no call on this vtable follows */
    void (*drop)(void *ctx);
} OkdbBridgeHostV1;

/* ── the function table ───────────────────────────────────────────────────────────────────────────────────────────────────── */
typedef struct OkdbBridgeV1 {
    uint32_t abi_major;   /* == OKDB_BRIDGE_ABI_MAJOR the consumer asked for */
    uint32_t abi_minor;   /* >= 0; fields added in minor N exist iff struct_size covers them */
    uint32_t struct_size; /* sizeof(OkdbBridgeV1) as okdb compiled it */
    uint32_t reserved;

    /* okdb's package version ("2.7.0"): a static NUL-terminated string. */
    const char *(*version)(void);

    /* Open a standalone instance at `path` (NUL-terminated). `opts` = MessagePack map of okdb options (as `new OKDB(path,
     * opts)`; NULL/0 = defaults), `flags` = OKDB_OPEN_*. On OKDB_OK *out_h is the handle; otherwise *err (when non-NULL) is
     * an {"err"} envelope to free(). */
    okdb_status (*open)(const char *path, const uint8_t *opts, size_t opts_len, uint32_t flags, okdb_handle *out_h,
                        uint8_t **err, size_t *err_len);

    /* Attach to an instance a JS host already opened in this process: `native_handle` = `db._native.bridgeHandle()`.
     * Call on that instance's JS thread, which becomes the owner. close() detaches; it never closes the host's instance. */
    okdb_status (*attach)(uint64_t native_handle, okdb_handle *out_h, uint8_t **err, size_t *err_len);

    /* Close (standalone: close the instance and free the handle; attached: detach). Pending submits complete with OKDB_CLOSED.
     * Owner thread only, except OKDB_OPEN_OWNER_THREAD handles (any thread; joins okdb's thread unless called from it). */
    okdb_status (*close)(okdb_handle h);

    /* One request [scope, env|nil, method, args] -> *out = {"ok": result} | {"err": {...}} (free() it). Synchronous, on the
     * owner thread (else OKDB_WRONG_THREAD; OKDB_OPEN_OWNER_THREAD handles forward it and block). */
    okdb_status (*dispatch)(okdb_handle h, const uint8_t *req, size_t req_len, uint8_t **out, size_t *out_len);

    /* An array of requests run as ONE write group (one outer LMDB txn per env; a failing item aborts only itself) ->
     * *out = {"ok": [{"ok": r} | {"err": e}, ...]}. Same threading as dispatch. */
    okdb_status (*dispatch_batch)(okdb_handle h, const uint8_t *reqs, size_t reqs_len, uint8_t **out, size_t *out_len);

    /* Queue one request from ANY thread; `done(ctx, status, reply, len)` runs later on the owner thread (OKDB_OPEN_OWNER_THREAD:
     * okdb's thread; attached: the JS thread, woken through the host's loop; inline: inside the owner's next pump()). The return
     * value only reports whether it was queued (OKDB_INVALID / OKDB_CLOSED: `done` is never called). `req` is copied. */
    okdb_status (*submit)(okdb_handle h, const uint8_t *req, size_t req_len, okdb_done_fn done, void *ctx);

    /* Free a buffer okdb returned (the *out or *err of any call above). NULL is a no-op. */
    void (*free)(uint8_t *ptr, size_t len);

    /* Install (host != NULL; copied) or remove (NULL) the consumer's callback host for this handle's owner thread. Owner thread
     * only (OKDB_OPEN_OWNER_THREAD: any thread, forwarded). On an attached handle the JS host keeps serving JS callbacks; okdb
     * routes each callback to the side that issued it. */
    okdb_status (*set_host)(okdb_handle h, const OkdbBridgeHostV1 *host);

    /* Inline handles: run due work (processor drains, timers, queued submits) for up to `budget_ms` (0 = one turn) on the
     * owner thread; *next_ms = ms until the next deadline, -1 = nothing scheduled. Call it again by then. Owner-thread and
     * attached handles: runs queued submits only (okdb / the JS host pump themselves), *next_ms = -1. */
    okdb_status (*pump)(okdb_handle h, uint32_t budget_ms, int64_t *next_ms);

    /* The okdb CLI (`okdb [command] [options]`) in this process: argv[0] is the program name. Returns the exit code; most
     * commands call exit() themselves. Never touches Node-API. OKDB_UNSUPPORTED (6) is returned by builds without the CLI. */
    int32_t (*cli_main)(int32_t argc, const char *const *argv);

    /* ── added in ABI 1.1 (check struct_size) ── */

    /* submit() for an array of requests: run on the owner thread like dispatch_batch (ONE write group) and complete with ONE
     * done(ctx, status, {"ok": [reply, ...]}) — e.g. threadpool threads feeding an attached handle. */
    okdb_status (*submit_batch)(okdb_handle h, const uint8_t *reqs, size_t reqs_len, okdb_done_fn done, void *ctx);

    /* ── added in ABI 1.2 (present iff struct_size >= offsetof(OkdbBridgeV1, read) + sizeof read) ── */

    /* A READ-ONLY request from ANY thread, run on the CALLING thread without the owner (no forwarding, no pump, never
     * OKDB_WRONG_THREAD), on every handle kind (inline, OKDB_OPEN_OWNER_THREAD, attached): same [scope, env|nil, method, args]
     * shape and reply envelope as dispatch, restricted to scope "env" and its read methods — get, getEntry, has, count,
     * getMany, getRange, getKeys, getValues, getCount, getByPrefix, getIndex, byIndex, countByIndex (anything else:
     * OKDB_INVALID, code READ_METHOD_NOT_ALLOWED). Reads committed data only (an open write group of the owner is not visible
     * until it commits), one short LMDB read txn per call, nothing pinned between calls. After close (or while closing, or
     * once an attached JS instance closed) OKDB_CLOSED. *out is free()d as usual. */
    okdb_status (*read)(okdb_handle h, const uint8_t *req, size_t req_len, uint8_t **out, size_t *out_len);
} OkdbBridgeV1;

/* The one exported entry point: the table for `abi_major`, or NULL if this build does not support that major. */
typedef const OkdbBridgeV1 *(*okdb_bridge_fn)(uint32_t abi_major);
const OkdbBridgeV1 *okdb_bridge(uint32_t abi_major);

/* Same as okdb_bridge(1)->cli_main; exported on its own for the native `okdb` launcher. */
typedef int32_t (*okdb_cli_main_fn)(int32_t argc, const char *const *argv);
int32_t okdb_cli_main(int32_t argc, const char *const *argv);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* OKDB_BRIDGE_H */
