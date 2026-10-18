//! `okcode` without Node: the commands of bin/okcode.js on okcode-core.
//!
//! Shape (mirrors the JS): parse args → open the store (okdb logging off stdout; stdout is the
//! answer, --json must stay parseable) → Okcode::open → the --root workspace through the native
//! LocalFs → one command → ws.flush (writes happen after the answer). Workspace verbs go through
//! `Workspace::call(method, args)` in the JS argument shapes, so each command is "build args,
//! call, render", the renderers printing the same text as bin/okcode.js.
//! Files: open.rs (boot), mgmt.rs (management) · look.rs (look-up commands) · edit.rs (edits).

mod args;
mod edit;
mod help;
mod look;
mod mgmt;
mod open;
mod render;

use args::Args;
use open::{open_ws, Boot};
use render::{err, out};
use std::process::ExitCode;

/// The JS `Exit` (a code and an optional stderr message) — and every other failure, already
/// formatted as main's catch prints it (`${code}: ${message}` or the message).
#[derive(Debug)]
pub struct Exit {
    pub code: u8,
    pub message: String,
}

impl Exit {
    pub fn fail(message: String) -> Exit {
        Exit { code: 1, message }
    }
    pub fn from_core(e: okcode_core::Error) -> Exit {
        Exit::fail(e.to_string())
    }
}

/// need(v, usage): undefined, null, '' and a bare boolean flag all fail with the usage line.
pub fn need(v: Option<String>, usage: &str) -> Result<String, Exit> {
    match v {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(Exit::fail(usage.into())),
    }
}

const WS_COMMANDS: &[&str] = &[
    "structure", "find", "refs", "importers", "grep", "glob", "outline", "ask", "packages", "read", "read-at", "edit",
    "edit-batch", "write", "check", "stats",
];

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let a = Args::parse(&argv);
    match run(&a, argv.is_empty()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if !e.message.is_empty() {
                err(&e.message);
            }
            ExitCode::from(e.code)
        }
    }
}

fn run(a: &Args, no_args: bool) -> Result<(), Exit> {
    let cmd = a.cmd.as_deref();
    if cmd.is_none() || cmd == Some("help") || a.flags.contains_key("help") || a.flags.contains_key("h") {
        let topic = if cmd == Some("help") { a.positional.first().map(String::as_str) } else { cmd };
        match topic.and_then(help::help) {
            Some(h) => out(h),
            None => {
                out(help::USAGE);
                if let Some(t) = topic.filter(|t| *t != "help") {
                    return Err(Exit::fail(format!("unknown command \"{t}\"")));
                }
            }
        }
        return if no_args { Err(Exit { code: 1, message: String::new() }) } else { Ok(()) };
    }
    let cmd = cmd.unwrap_or_default();
    let b = Boot::new(a);
    match cmd {
        "status" => return mgmt::status(&b, a),
        "sync" => return mgmt::sync(&b, a),
        "reset" => return mgmt::reset(&b, a),
        "embedders" => return mgmt::embedders(&b, a),
        "workspaces" => return mgmt::workspaces(&b, a),
        "remove-workspace" => return mgmt::remove_workspace(&b, a),
        _ => {}
    }
    if !WS_COMMANDS.contains(&cmd) {
        return Err(Exit::fail(format!("unknown command \"{cmd}\" — okcode --help")));
    }
    let (s, opened) = open_ws(&b, a)?;
    let root = b.root.to_string_lossy().into_owned();
    let w = look::Ws { tw: s.oc.tool_ws(opened.clone(), Some(root.clone())), root, a };
    let r = (|| {
        match cmd {
            "structure" => look::structure(&w)?,
            "find" => look::find(&w)?,
            "refs" => look::refs(&w)?,
            "importers" => look::importers(&w)?,
            "grep" => look::grep(&w)?,
            "glob" => look::glob(&w)?,
            "outline" => look::outline(&w)?,
            "ask" => look::ask(&w)?,
            "packages" => look::packages(&w)?,
            "read" => look::read(&w)?,
            "read-at" => look::read_at(&w)?,
            "edit" => edit::edit(&w)?,
            "edit-batch" => edit::edit_batch(&w)?,
            "write" => edit::write(&w)?,
            "check" => edit::check(&w)?,
            "stats" => mgmt::stats(&opened, a)?,
            _ => unreachable!("WS_COMMANDS"),
        }
        // Writes happen after the answer, never in front of it.
        opened.ws.call("flush", &[]).map_err(Exit::from_core)?;
        Ok(())
    })();
    s.close();
    r
}
