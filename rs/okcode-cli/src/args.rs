//! bin/okcode.js parseArgs, exactly: the first non-flag word is the command; `--k v`, `--k=v`,
//! boolean flags take no value, a flag at the end is `true`, repeatable flags join with ','.
//! `edit-batch --json batch.json` names the batch (brain-compatible).

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Flag {
    On,
    Val(String),
}

#[derive(Debug, Default)]
pub struct Args {
    pub cmd: Option<String>,
    pub flags: BTreeMap<String, Flag>,
    pub positional: Vec<String>,
}

const BOOL: &[&str] = &["json", "refresh", "force", "create", "all", "help", "h", "regex", "case-sensitive", "smart-case"];
const REPEAT: &[&str] = &["glob", "path"];

impl Args {
    pub fn parse(argv: &[String]) -> Args {
        let cmd = argv.first().filter(|a| !a.starts_with('-')).cloned();
        let rest = if cmd.is_some() { &argv[1..] } else { argv };
        let mut a = Args { cmd, ..Default::default() };
        let mut i = 0;
        while i < rest.len() {
            let arg = &rest[i];
            if !arg.starts_with("--") && arg != "-h" {
                a.positional.push(arg.clone());
                i += 1;
                continue;
            }
            // JS: a.replace(/^--?/, '')
            let stripped = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')).unwrap_or(arg);
            let (mut name, value) = match stripped.find('=') {
                Some(eq) => (stripped[..eq].to_string(), Flag::Val(stripped[eq + 1..].to_string())),
                None => {
                    let name = stripped.to_string();
                    let next = rest.get(i + 1);
                    if name == "json" && a.cmd.as_deref() == Some("edit-batch") && next.is_some_and(|n| !n.starts_with("--")) {
                        i += 1;
                        ("batch".to_string(), Flag::Val(rest[i].clone()))
                    } else if BOOL.contains(&name.as_str()) {
                        (name, Flag::On)
                    } else if let Some(n) = next {
                        i += 1;
                        (name, Flag::Val(n.clone()))
                    } else {
                        (name, Flag::On)
                    }
                }
            };
            let joined = match (REPEAT.contains(&name.as_str()), a.flags.get(&name), &value) {
                (true, Some(Flag::Val(prev)), v) => Flag::Val(format!("{prev},{}", flag_str(v))),
                _ => value,
            };
            a.flags.insert(std::mem::take(&mut name), joined);
            i += 1;
        }
        a
    }

    /// flags[name] as a string (a bare boolean flag reads as "true", as `${true}` would)
    pub fn get(&self, name: &str) -> Option<String> {
        self.flags.get(name).map(flag_str)
    }

    pub fn on(&self, name: &str) -> bool {
        self.flags.get(name) == Some(&Flag::On)
    }
}

fn flag_str(f: &Flag) -> String {
    match f {
        Flag::On => "true".into(),
        Flag::Val(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(s: &str) -> Args {
        Args::parse(&s.split_whitespace().map(String::from).collect::<Vec<_>>())
    }
    #[test]
    fn parses_like_js() {
        let a = p("grep foo --glob *.js --glob !x --json --limit=5 --root");
        assert_eq!(a.cmd.as_deref(), Some("grep"));
        assert_eq!(a.positional, vec!["foo"]);
        assert_eq!(a.get("glob").as_deref(), Some("*.js,!x"));
        assert!(a.on("json"));
        assert_eq!(a.get("limit").as_deref(), Some("5"));
        assert!(a.on("root"));
        let b = p("edit-batch --json batch.json");
        assert_eq!(b.get("batch").as_deref(), Some("batch.json"));
        assert!(!b.on("json"));
        let c = p("--help");
        assert!(c.cmd.is_none() && c.on("help"));
    }
}
