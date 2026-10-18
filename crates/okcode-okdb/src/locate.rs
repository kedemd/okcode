//! Finding okdb's installed .node from a process that is not Node (the standalone `okcode` CLI). okcode depends on
//! `@kedem/okdb` ^3 as a normal npm dependency; npm installs okdb's addon as `@okdb/native-<key>/okdb-node.<key>.node`
//! (deduped with the host's, so it is the same image a JS host loads). Search order:
//!   1. $OKDB_NATIVE_BINARY
//!   2. from the executable's directory upwards, in every `node_modules`: `@kedem/okdb/native/<key>/okdb-node.<key>.node` (okdb 3.0
//!      bundles it), then the platform-package forms (`@okdb/native-<key>/…`, `@kedem/okdb-native-<key>/…`, also nested)
//! The bridge's ABI/version check (Bridge::load) settles a mismatch; this only finds a file.
use std::path::{Path, PathBuf};

/// okdb's platform key for THIS build (the CLI is built per target, so its libc is known at compile time).
pub fn platform_key() -> String {
    let os = if cfg!(target_os = "macos") { "darwin" } else if cfg!(windows) { "win32" } else { "linux" };
    let cpu = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    match os {
        "linux" => format!("linux-{cpu}-{}", if cfg!(target_env = "musl") { "musl" } else { "gnu" }),
        _ => format!("{os}-{cpu}"),
    }
}

/// Where npm puts okdb's addon, relative to a node_modules: bundled inside @kedem/okdb (3.0: `native/<key>/okdb-node.<key>.node`),
/// or a platform package (`@okdb/native-<key>`, `@kedem/okdb-native-<key>`).
fn package_rels(key: &str) -> Vec<PathBuf> {
    let file = format!("okdb-node.{key}.node");
    vec![
        PathBuf::from("@kedem").join("okdb").join("native").join(key).join(&file),
        PathBuf::from("@okdb").join(format!("native-{key}")).join(&file),
        PathBuf::from("@kedem").join(format!("okdb-native-{key}")).join(&file),
    ]
}

pub fn candidates(start: &Path, key: &str) -> Vec<PathBuf> {
    let rels = package_rels(key);
    let mut out = Vec::new();
    let mut dir = Some(start);
    while let Some(d) = dir {
        let nm = d.join("node_modules");
        for rel in &rels {
            out.push(nm.join(rel));
            out.push(nm.join("@kedem").join("okdb").join("node_modules").join(rel));
        }
        dir = d.parent();
    }
    out
}

/// The okdb .node to load, or every path tried.
pub fn find_okdb_node() -> Result<PathBuf, Vec<PathBuf>> {
    if let Some(p) = std::env::var_os("OKDB_NATIVE_BINARY").map(PathBuf::from) {
        return if p.is_file() { Ok(p) } else { Err(vec![p]) };
    }
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok());
    let start = exe.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let tried = candidates(&start, &platform_key());
    match tried.iter().find(|p| p.is_file()) {
        Some(p) => Ok(p.clone()),
        None => Err(tried),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_deduped_and_the_nested_copy() {
        let base = std::env::temp_dir().join(format!("okcode-locate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // <app>/node_modules/@kedem/okcode-linux-x64/bin/okcode  next to  <app>/node_modules/@okdb/native-<key>/…
        let bin = base.join("node_modules/@kedem/okcode-linux-x64/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let key = platform_key();
        let nested = base.join(format!("node_modules/@kedem/okdb/node_modules/@okdb/native-{key}"));
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join(format!("okdb-node.{key}.node")), b"x").unwrap();
        let hit = candidates(&bin, &key).into_iter().find(|p| p.is_file()).unwrap();
        assert!(hit.ends_with(format!("@kedem/okdb/node_modules/@okdb/native-{key}/okdb-node.{key}.node")));
        let flat = base.join(format!("node_modules/@okdb/native-{key}"));
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join(format!("okdb-node.{key}.node")), b"x").unwrap();
        let hit = candidates(&bin, &key).into_iter().find(|p| p.is_file()).unwrap();
        assert!(hit.ends_with(format!("node_modules/@okdb/native-{key}/okdb-node.{key}.node")) && !hit.to_string_lossy().contains("@kedem/okdb/"));
        // okdb 3.0 bundles its addon inside @kedem/okdb: found first
        let bundled = base.join(format!("node_modules/@kedem/okdb/native/{key}"));
        std::fs::create_dir_all(&bundled).unwrap();
        std::fs::write(bundled.join(format!("okdb-node.{key}.node")), b"x").unwrap();
        let hit = candidates(&bin, &key).into_iter().find(|p| p.is_file()).unwrap();
        assert!(hit.ends_with(format!("node_modules/@kedem/okdb/native/{key}/okdb-node.{key}.node")));
        let _ = std::fs::remove_dir_all(&base);
    }
}
