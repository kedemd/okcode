//! The host's own analysers: extractors and extensions registered in JS (`analysis.registerExtractor`,
//! `analysis.registerExtension`). Indexing hands a path they claim to the host's `extract()`, as the
//! workspace always has; every other path is extracted natively. The Node binding installs the host and
//! keeps the claims current; without one (the standalone CLI) everything is native.
use okcode_analysis::lang_of;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

pub trait Host: Send + Sync {
    /// The host's `extract(rel, text)`; None when it does not claim `rel` or cannot be asked.
    fn extract(&self, rel: &str, text: &str) -> Option<Value>;
    /// The host's `analyzerVersionFor(rel)` when it claims `rel`.
    fn version_for(&self, rel: &str) -> Option<Option<String>>;
}

#[derive(Default)]
struct State {
    host: Option<Arc<dyn Host>>,
    /// languages with a host extractor
    langs: HashSet<String>,
    /// a host extension is registered: its `match` may claim any path
    extensions: bool,
}

fn state() -> &'static RwLock<State> {
    static S: OnceLock<RwLock<State>> = OnceLock::new();
    S.get_or_init(Default::default)
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// Bumped on every change of host or claims: a cache of analyzer versions keys on it.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

pub fn set_host(host: Arc<dyn Host>) {
    state().write().unwrap_or_else(|e| e.into_inner()).host = Some(host);
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

pub fn set_claims(langs: Vec<String>, extensions: bool) {
    let mut st = state().write().unwrap_or_else(|e| e.into_inner());
    st.langs = langs.into_iter().collect();
    st.extensions = extensions;
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// The host, when it may claim `rel`.
fn claimant(rel: &str) -> Option<Arc<dyn Host>> {
    let st = state().read().unwrap_or_else(|e| e.into_inner());
    let host = st.host.as_ref()?;
    if st.extensions || (!st.langs.is_empty() && st.langs.contains(lang_of(rel))) {
        Some(host.clone())
    } else {
        None
    }
}

/// `extract(path, text)` as indexing runs it: the host's for a path it claims, else native.
pub fn extract(rel: &str, text: Option<&str>) -> Value {
    if let (Some(t), Some(h)) = (text, claimant(rel)) {
        if let Some(v) = h.extract(rel, t) {
            return v;
        }
    }
    okcode_analysis::extract(rel, text)
}

/// `analyzerVersionFor(path)`: what a stored row's `analyzerVersion` must equal to be trusted.
pub fn analyzer_version_for(rel: &str) -> Option<String> {
    if let Some(h) = claimant(rel) {
        if let Some(v) = h.version_for(rel) {
            return v;
        }
    }
    okcode_analysis::analyzer_version_for(rel)
}
