//! `lad-luau` — convert a `bevy_mod_scripting` LAD file into a native Luau
//! `.d.luau` definition file.
//!
//! ```text
//! lad-luau <input.lad.json> [output.d.luau]
//! ```
//!
//! With no output path, the definitions are written to stdout.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: lad-luau <input.lad.json> [output.d.luau]");
        return ExitCode::FAILURE;
    };
    let output = args.next().map(PathBuf::from);

    let source = match std::fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: failed to read `{input}`: {e}");
            return ExitCode::FAILURE;
        }
    };

    let lad = match ladfile::parse_lad_file(&source) {
        Ok(lad) => lad,
        Err(e) => {
            eprintln!("error: failed to parse `{input}` as a LAD file: {e}");
            return ExitCode::FAILURE;
        }
    };

    let defs = bevy_mod_scripting_luau::lad_to_luau(&lad);

    match output {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, defs) {
                eprintln!("error: failed to write `{}`: {e}", path.display());
                return ExitCode::FAILURE;
            }
            eprintln!("wrote {}", path.display());
        }
        None => print!("{defs}"),
    }
    ExitCode::SUCCESS
}
