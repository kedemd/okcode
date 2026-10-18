//! What the tool layer (tools.rs) and the public API (okcode.rs) see of a workspace and of the
//! host.
//!
//! lib/tools.js takes WORKSPACE OBJECTS, never okcode.js: `createTools({ workspace: async (hint)
//! => ws | null, workspaces: () => [{ id, root?, description? }] })`. Here:
//!
//! - [`ToolWorkspace`] is one such ws object: the native `Workspace` itself ([`NativeWs`]), an
//!   okcode-managed one whose `ask` follows the active profile (okcode.rs), or a JS object the
//!   binding bridges (a test's fake, a ws whose methods a host wrapped).
//! - [`ToolHost`] is the host's two callbacks.
//!
//! Both are SYNC (`Send + Sync`): tools.rs runs on the libuv threadpool and a bridged
//! implementation blocks that thread on the JS answer. Never call one from the JS thread.
use crate::error::Result;
use crate::workspace::Workspace;
use serde_json::Value;
use std::sync::Arc;

/// One workspace as lib/tools.js uses it: `ws.id`, `ws.access.root`, `typeof ws.<m> ===
/// 'function'` and `await ws.<m>(...args)`.
pub trait ToolWorkspace: Send + Sync {
    /// `ws.id`
    fn id(&self) -> String;
    /// `ws.access && ws.access.root` when it is a string (rootOf's fallback).
    fn access_root(&self) -> Option<String>;
    /// `typeof ws[method] === 'function'` (isLibraryPath, glob are optional on a JS ws).
    fn has(&self, method: &str) -> bool;
    /// `await ws[method](...args)` with JSON args / result in the workspace.js shapes
    /// (Workspace::call's conventions). An Err carries the JS error's `code` and message.
    fn call(&self, method: &str, args: &[Value]) -> Result<Value>;
}

/// The host side of createTools.
pub trait ToolHost: Send + Sync {
    /// `await workspace(hint)`; `None` hint = JS `undefined`. Ok(None) = the host returned null.
    /// Err = the host threw (render says `(<tool>) failed: <message>`).
    fn workspace(&self, hint: Option<&str>) -> Result<Option<Arc<dyn ToolWorkspace>>>;
    /// `workspaces()` — `[{ id, root?, description? }]`; a throwing host is an empty list (JS `known`).
    fn workspaces(&self) -> Vec<Value>;
}

/// The native workspace as a tool workspace: every method is Workspace::call.
pub struct NativeWs {
    pub ws: Arc<Workspace>,
    /// the JS facade's `root` (the native `Access` does not carry it)
    pub root: Option<String>,
}

impl ToolWorkspace for NativeWs {
    fn id(&self) -> String {
        self.ws.id.clone()
    }
    fn access_root(&self) -> Option<String> {
        self.root.clone()
    }
    fn has(&self, method: &str) -> bool {
        crate::workspace::api::METHODS.contains(&method)
    }
    fn call(&self, method: &str, args: &[Value]) -> Result<Value> {
        self.ws.call(method, args)
    }
}
