# okdb-bridge — the stable C ABI of okdb's .node

A program that wants okdb **without compiling it in** (okcode, any C / C++ / Rust / Go / Zig host) loads the `.node` that
`@kedem/okdb` ships (`@okdb/native-<platform>/okdb-node.<platform>.node`), calls `okdb_bridge(1)` and talks to okdb through the
function table declared in [`okdb_bridge.h`](okdb_bridge.h). One okdb-core per process, no okdb crates in the
consumer's build, no Node required (standalone), or sharing the live instance of a JS host (attach).

The header is the contract; this file explains it. ABI major **1**, minor **0**.

## Loading

```c
void *so = dlopen(path_to_node, RTLD_NOW | RTLD_LOCAL);
const OkdbBridgeV1 *ok = ((okdb_bridge_fn)dlsym(so, "okdb_bridge"))(OKDB_BRIDGE_ABI_MAJOR);
if (!ok) { /* this okdb does not speak ABI 1: ABI_UNSUPPORTED */ }
```

- The `.node` has **no undefined Node-API symbols** (napi is resolved at runtime from the host process), so it loads into a plain
  process with `RTLD_NOW` / `LD_BIND_NOW` and never touches Node-API there. Inside Node, `dlopen` of the path `require` loaded
  returns the same image: the same okdb statics as the JS side.
- `okdb_bridge(major)` returns `NULL` for a major this build does not support. Within a major, fields are only appended: check
  `struct_size` before using a field added in a later minor (`offsetof(field) + sizeof(field) <= struct_size`). The same rule
  applies to `OkdbBridgeHostV1` in the other direction (okdb reads `host->struct_size`).
- `version()` is okdb's package version (`"2.7.0"`): the same number as `require('@kedem/okdb/package.json').version` and
  `okdb version`. Check it for features newer than the ABI minor (new dispatch methods).

## Wire format

Plain MessagePack (any spec-compliant encoder). Strings are UTF-8, maps have string keys (objects).

| okdb value          | encoded as (replies)                                   | accepted in requests                    |
| ------------------- | ------------------------------------------------------ | --------------------------------------- |
| `undefined`, `null` | nil                                                    | nil; msgpackr's undefined (fixext1 0)   |
| number              | int when integral and \|x\| < 2^53, else float64       | int, float32, float64                   |
| bigint              | int64 / uint64                                         | (ints)                                  |
| string / Buffer     | str / bin                                              | str / bin                               |
| array / object      | array / map                                            | array / map (msgpackr records also)     |
| Date                | timestamp ext (-1)                                     | timestamp ext                           |
| a JS `Map`          | map (keys as values)                                   | a map with a non-string key             |

**Request**: `[scope, env, method, args]`, `env` = env name or nil (the scope's default), `args` = array (the positional arguments
of okdb's JS layer's `native.call(scope, env, method, args)`; okdb's JS sources are the reference for each method's argument
order, its dispatch crate's README for the scopes). Examples:

```
["db",  nil,       "registerType", ["docs", {}]]
["env", "default", "put",          ["docs", "k1", {"body": "hi"}]]
["env", "default", "get",          ["docs", "k1"]]
["fts", nil,       "search",       [...]]
```

Bridge-level scopes: `["bridge", nil, "open", []]` (finish an `OKDB_OPEN_DEFER` open), `["bridge", nil, "info", []]` →
`{abi: [major, minor], version, mode: "inline"|"owner-thread"|"attached", open}`. Process-wide registries (`OKDB.registerViewReducer`,
...): scope `"static.<scope>"`, e.g. `["static.views", nil, "registerViewReducer", [...]]`.

**Reply**: `{"ok": value}` on success, `{"err": {"code", "message", "details"?}}` on failure. The returned status mirrors it
(`OKDB_OK` = `ok`); every buffer okdb returns is freed with `free(ptr, len)`.

**Batch**: `dispatch_batch` takes an array of requests and returns `{"ok": [reply, ...]}`, one reply per item, run as ONE write
group (one outer LMDB transaction per env, a nested transaction per item: a failing item aborts only itself; if the final commit
fails, every item that had succeeded reports that error). Same semantics as okdb's JS write batching.

## Handles and threading

An okdb instance is single-threaded: everything runs on its **owner thread**.

| handle (how made)                         | owner thread                    | `dispatch` / `dispatch_batch` from another thread | `submit` completes                           | who pumps                       |
| ----------------------------------------- | ------------------------------- | ------------------------------------------------- | -------------------------------------------- | ------------------------------- |
| `open(path, opts, 0)` — inline            | the thread that called `open`   | `OKDB_WRONG_THREAD`                               | inside the owner's next `pump()`             | the consumer: `pump()`          |
| `open(..., OKDB_OPEN_OWNER_THREAD)`       | a thread okdb starts            | forwarded to the owner thread, blocks until done  | on okdb's thread, as soon as it is idle      | okdb (no consumer loop needed)  |
| `attach(db._native.bridgeHandle())`       | the JS thread of that instance  | `OKDB_WRONG_THREAD`                               | on the JS thread (woken through its loop)    | the JS host (`@kedem/okdb`)     |

- `submit` is the only call that is valid from any thread on every handle. The request is copied; `done` runs on the owner thread
  with the reply (valid during the callback only). Submits still queued at `close` complete with `OKDB_CLOSED` / `CLOSED`.
- `pump(h, budget_ms, &next)` (inline handles) runs processor drains, timers, lock heartbeats and queued submits until nothing is
  due or `budget_ms` passed (`0` = one turn); `next` = ms until the next deadline or `-1`. Call it from your loop at least by then
  (and whenever you queued work). On owner-thread / attached handles it only runs queued submits (for a host that wants them now).
- Host callbacks (below) run on the owner thread, re-entrantly inside `dispatch` / `pump`: a callback may call `dispatch` on the
  same handle.
- `close`: inline / attached on the owner thread; owner-thread handles from any thread (joins okdb's thread; from inside a
  callback on that thread it schedules the close and returns). `close` of an attached handle only detaches.
- An attached handle holds no reference that keeps the JS instance alive: after the JS side closed it, calls fail with
  `CLOSED`. Attach on the JS thread, e.g. from your own napi function: `attach(handleIdFromJs)`.

## Host callbacks (`set_host`)

Some okdb features call code that lives in the consumer: custom chunk strategies (okcode's `headedChunks`), embedder factories
and the embedders they make, preparers, vector algorithms / storage adapters, queue and processor handlers, event listeners,
engine drivers. In the JS binding these are JS functions; over the bridge they are **ids the consumer hands out**:

1. `set_host(h, &host)` installs the consumer's vtable for the handle's owner thread (`call`, `release`, `free_reply`, `drop`).
2. A function argument is the marker `{"$fn": <id>}` (an unsigned id the consumer chooses, < 2^52) anywhere the JS API takes a
   function. Registering it stores the id; okdb calls back with `host.call(ctx, op, args)` and, once it drops the last reference,
   `host.release(ctx, id)`.
3. The host answers with an `{"ok": value}` / `{"err": {"code", "message"}}` envelope (an `err` becomes the okdb error the
   feature reports; returning a non-`OKDB_OK` status without one reports `HOST_CALLBACK_FAILED`).

**Host ops** (`op`, `args`; the ops of okdb's JS `bridge.js`):

| op            | args                                       | answer                                                                 |
| ------------- | ------------------------------------------ | ---------------------------------------------------------------------- |
| `call`        | `[fnId, ...callArgs]`                      | the function's result                                                  |
| `callAsync`   | `[fnId, ...callArgs]`                      | `{"$value": result}` (a native host completes synchronously)           |
| `emb`         | `["new", factoryFnId, config]`             | `{"$id": objectId, "batch": bool}` — a new embedder                    |
| `emb`         | `["embed", objectId, input]`               | `[float, ...]` (dims floats)                                           |
| `emb`         | `["embedBatch", objectId, [input, ...]]`   | `[[float, ...], ...]` (only called when `batch` was true)              |
| `emb`         | `["start" / "stop" / "free", objectId, nil]` | nil                                                                  |
| `alg`, `stor` | `["new", fnId, arg]`, `[method, objectId, arg]` | as bridge.js `runAlg` / `runStor`                                 |
| `event`       | `[target, eventName, payload]`             | ignored                                                                |
| `handler`, `job`, `quantum`, `resolve`, `driver`, `api` | as bridge.js                 | as bridge.js (`{"$token"}` answers are polled with `resolve`)    |

Examples — a chunk strategy and an embedder factory, then a pipeline that uses both:

```
["embeddings", nil, "registerChunkStrategy",   ["headed", {"$fn": 1}]]
    host.call("call", [1, text, size, overlap])          -> {"ok": [{"text": "...", "start": 0, "end": 120}, ...]}
["embeddings", nil, "registerEmbedderFactory", ["mine", {"$fn": 2}, {"label": "Mine"}]]
    host.call("emb", ["new", 2, config])                 -> {"ok": {"$id": 10, "batch": true}}
    host.call("emb", ["embedBatch", 10, ["a", "b"]])     -> {"ok": [[...], [...]]}
["embeddings", nil, "createPipeline", ["p", {"source_type": "docs", "field": "body", "dims": 8,
                                             "embedder": {"type": "mine"}, "chunk": {"strategy": "headed", "size": 200}}]]
["events", nil, "subscribe", ["bridge:feed", "item:put"]]
    host.call("event", ["bridge:feed", "item:put", {...}])
```

Register what existing pipelines need **before** the instance opens (`OKDB_OPEN_DEFER`, then `["bridge", nil, "open", []]`):
pipelines boot at open and look their strategies / embedders up then.

**Attached handles**: the JS host keeps serving JS callbacks and okdb routes each callback to the side that registered it: okdb
tags the consumer's ids internally (the consumer always sees its own ids), and event targets for the consumer must start with
`bridge:`. The host is per owner thread: a second `set_host` on the same thread replaces the first (`drop` is called).

## Error codes

Stable strings in `err.code`. Bridge-level (status in parentheses):

| code                     | meaning                                                                                       |
| ------------------------ | --------------------------------------------------------------------------------------------- |
| `ABI_UNSUPPORTED`        | (`OKDB_INVALID`) a vtable with a `struct_size` below the minimum; `okdb_bridge()` returns NULL |
| `BAD_HANDLE`             | (`OKDB_INVALID`) unknown handle                                                               |
| `BAD_REQUEST`            | (`OKDB_INVALID`) not `[scope, env, method, args]`, or not decodable MessagePack                |
| `WRONG_THREAD`           | (`OKDB_WRONG_THREAD`) see the threading table                                                 |
| `CLOSED`                 | (`OKDB_CLOSED`) handle closed / closing, or the attached JS instance is gone                  |
| `ATTACH_NOT_FOUND`       | (`OKDB_INVALID`) no live JS instance with that `bridgeHandle()` on this thread                 |
| `HOST_CALLBACK_FAILED`   | (`OKDB_ERR`) a host callback returned an error status without an `err` envelope                |
| `PANIC`                  | (`OKDB_PANIC`) an internal error caught at the boundary                                        |

Everything else (`OKDB_ERR`) is okdb's own code, the same one the JS API throws: `NOT_FOUND`, `VALIDATION_FAILED`,
`ENV_NOT_FOUND`, `INVALID_STATE`, `NOT_IMPLEMENTED_NATIVE` (a method the native binding does not have), `NATIVE_NO_BRIDGE` (a
callback-backed call with no host installed), ...

## CLI (`cli_main`, `okdb_cli_main`)

The `okdb` CLI (`okdb [command] [options]`: server, cluster, backup / restore / export / import, tokens, licenses, doctor, shm-gc)
is linked into the `.node`: `okdb_cli_main(argc, argv)` (also `table->cli_main`) runs it in the calling process with no Node-API
and no Node. Most commands end with `exit()`. The native launcher (`okdb`, a few hundred KB, does not
link okdb) dlopens the sibling `.node` and calls it; a cluster supervisor re-executes the launcher for its workers.

## Overhead

One small call (`env.get` of a short document) costs the dispatch itself plus the MessagePack encode / decode of request and reply:
measured in `consumer-test` (`tests/standalone.rs`, `overhead` test prints it).

## Layout (in the okdb repository)

- `include/okdb_bridge.h` — the C header (the contract).
- `src/` — the `#[no_mangle]` entry points, the handle registry, the owner thread, the host adapter, the wire codec. Depends on
  `okdb-dispatch` (and `okdb-cli` for `cli_main`), no napi. The addon (`okdb-node/addon`) links it, which exports
  `okdb_bridge` / `okdb_cli_main` from the `.node`; `okdb-napi` registers the attach provider (`Native.bridgeHandle()`).
- `launcher/` — the native `okdb` launcher (no okdb dependency).
- `consumer-test/` — a consumer with no okdb dependency (outside the workspace): standalone + Node-hosted attach tests, and a C
  program compiled against the header.
