//! okcode-analysis — the analysis layer (src/analysis/index.js): pure functions on text plus the
//! package surface, composed from the wave 1 crates. Nothing here opens a file or a connection.
//!
//! `parse` is the `extract()` dispatcher (parse.js#extract): okjs extension → tree-sitter → JS (oxc) → generic.
//! `validate` is the syntax gate dispatcher (validate.js#validateSource) — owned by the `edit` slice.
pub use okcode_access as access;
pub use okcode_chunk as chunk;
pub use okcode_edges as edges;
pub use okcode_embed as embed;
pub use okcode_grep as grep;
pub use okcode_js as js;
pub use okcode_okjs as okjs;
pub use okcode_text as text;
pub use okcode_treesitter as treesitter;

pub mod parse;
pub mod validate;

pub use parse::{analyzer_version_for, ext_of, extract, is_secret, is_textual, lang_of, TEXT_MAX};
