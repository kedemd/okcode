//! okcode-core — the stateful core: the okdb-backed `Store` (src/store.js), the `Workspace`
//! (src/workspace.js), the public API `Okcode` (src/okcode.js) and the model-facing `Tools`
//! (src/tools.js). SHARED FILE (architect-owned): slices do not edit it.
//!
//! Threading model: every public API is SYNC and `Send + Sync`. The Node binding runs each call on
//! the libuv threadpool (napi AsyncTask), so JS stays async. Interior state lives behind locks
//! (`workspace::state`); a lock is NEVER held across an access-facade call or a store call (a JS
//! facade is served by the JS thread, which may itself be waiting for that lock → deadlock).
//!
//! okdb's `Okdb` is single-threaded (`Rc` inside): it lives on one owner thread per database
//! (`db::DbHost`); the store talks to it by message passing (`DbHost::run`).
pub mod db;
pub mod error;
pub mod host;
pub mod okcode;
pub mod pure;
pub mod record;
pub mod store;
pub mod tools;
pub mod workspace;

pub use error::{Error, Result};
pub use record::FileRecord;
pub use okcode::Okcode;
pub use store::Store;
pub use tools::Tools;
pub use workspace::{Options, Workspace};

/// Re-export so the binding and tests see one analysis layer.
pub use okcode_analysis as analysis;
pub use okcode_analysis::access::Access;
