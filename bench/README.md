# bench

Compares okcode against [adcode](https://github.com/) and against itself
(`fts: 'full'` vs `'symbols'`, with and without an embedder) on a real repo.
Every configuration runs **from zero**: a fresh copy of the repo (never the
original — `git ls-files`, or a plain recursive copy for a source tree that
is not itself a git repo) and a wiped store, each in its **own process**, so
one phase's drop in memory can never hide another's peak and one
configuration's cache can never leak into the next one's "cold" number.

## Running it

```sh
bench/compare.sh <repoDir> <scratchDir> [--configs ABCDE]

# e.g.
ADCODE=/path/to/adcode bench/compare.sh ~/some/repo /tmp/bench-scratch
bench/compare.sh ~/some/repo /tmp/bench-scratch --configs BC   # skip adcode and the embedder runs
```

Env:

- `ADCODE` — path to the adcode binary (config A). Unset skips it.
- `OLLAMA_URL` (default `http://localhost:11434`), `OLLAMA_MODEL` (default
  `qwen3-embedding:latest`) — configs D/E.

## Configurations

| | what |
| --- | --- |
| A | `adcode index` in the copy (its own `.adcode/` DB, measured in place) |
| B | okcode, `fts: 'full'`, no embedder |
| C | okcode, `fts: 'symbols'`, no embedder |
| D | okcode, `fts: 'full'` + the Ollama embedder, embed time reported separately from index time |
| E | okcode, `fts: 'symbols'` + the same embedder |

## Columns

- **cold peak RSS** — `/usr/bin/time -f %M` around the ONE process that
  opens an empty store and indexes the fresh copy (okcode: `addWorkspace`;
  adcode: `adcode index`). For D/E this process also waits for embedding to
  finish (polling `status()` until no profile has anything `pending`); the
  wall-clock column then splits **index** (until the scan/sync settles) from
  **embed** (until the wait above returns) — both inside the same measured
  process, since the embedder's own memory (batches in flight, the vector
  index) is part of what a host actually pays to run D/E.
- **cold wall** — index time, and for D/E, `+ embed Nms`.
- **warm peak RSS / warm wall** — a SECOND, separate process that reopens the
  now-populated store and re-adds the same workspace (okcode: one `list` +
  hash only what moved; adcode: `adcode index` again, its own warm path).
- **disk** — `du -sk` (real blocks, not apparent size) of the store,
  broken down for okcode by okdb env: `fts` = the content full-text index
  (`~fts/<env>`, absent entirely under `fts: 'symbols'`), `ws` = the
  workspace env itself (files/symbols rows), `vec` = embedding
  pipeline/vector sub-envs (`<env>:<pipeline>`, present only for D/E). adcode
  reports its single `.adcode/` directory.

## Quality check (D/E only, one-time)

`bench/questions-frontend.json` — 8 plain-English questions about
a 578-file TypeScript application, each with the file that answers it, determined by
reading the code (not by running anything first). `bench/quality.js` scores
`ask()` (configs D/E) and a `find()` + grep-only baseline (no embedder) by
hit@3 / hit@10: whether the right file appears in the top 3 / top 10 hits.
Run it against an already-indexed-and-embedded store:

```sh
node bench/quality.js --store <scratchDir>/D/store --questions bench/questions-frontend.json
```

## Why a fresh copy and a wiped store every time

- A warm-vs-cold measurement is meaningless if "cold" secretly reuses a
  previous run's okdb pages still in the OS page cache for the SAME files —
  a fresh copy (new inodes) and a wiped store make every "cold" run pay for
  its own first read.
- Each configuration gets its own copy (`<scratchDir>/<config>/repo`) so
  switching `fts` or adding an embedder is never measured against a tree
  some earlier phase already touched (mtimes, `.gitignore` caches, OS
  readahead).
