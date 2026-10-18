# okcode — the native implementation

okcode 1.0 is implemented in Rust; `src/` holds thin JavaScript wrappers over the addon. Crates:

- `okcode-text`, `okcode-chunk`, `okcode-grep`, `okcode-edges`, `okcode-access`, `okcode-js`, `okcode-treesitter`, `okcode-okjs`,
  `okcode-embed` — the analysis layer (parsing, chunking, edges, grep, access facades), no okdb.
- `okcode-analysis` — composes them.
- `okcode-okdb` — okcode's client of okdb's stable C bridge (`include/okdb_bridge.h`); okdb is never compiled in.
- `okcode-core` — store, workspace, the okcode API and the model-facing tools, over `okcode-okdb`.
- `okcode-node` — the Node addon (napi) behind `src/`.
- `okcode-cli` — the standalone `okcode` binary (no Node).
- `okcode-golden` — the golden-test loader (`golden/`: cases recorded from okcode 0.2).

Build: `cargo build --release -p okcode-node -p okcode-cli`. Release builds: `npm run build` (glibc 2.28, in Docker;
`packaging/build-linux-x64.sh`). Tests: the JS suite (`npm test` with `OKCODE_NATIVE_BINARY` pointing at a built addon) and
`cargo test`.
