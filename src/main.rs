//! `lad-luau` — convert a `bevy_mod_scripting` LAD file into a native Luau
//! `.d.luau` definition file.
//!
//! ```text
//! lad-luau <input.lad.json> [output.d.luau]
//! lad-luau --check <input.lad.json> <output.d.luau>
//! ```
//!
//! With no output path, the definitions are written to stdout.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
lad-luau — convert a bevy_mod_scripting LAD file into Luau definitions

USAGE:
    lad-luau <input.lad.json> [output.d.luau]
    lad-luau --check <input.lad.json> <output.d.luau>

ARGS:
    <input.lad.json>    The LAD file to convert.
    <output.d.luau>     Where to write the definitions; stdout if omitted.

OPTIONS:
    -c, --check         Do not write. Compare the generated definitions against
                        <output.d.luau> and exit non-zero if they differ, so CI
                        can catch committed definitions that have gone stale
                        relative to the reflection registry.
        --fflags        Print the luau-lsp analyze flags a full registry needs,
                        one per line, and exit.
        --no-init-types Skip the `<Class>Init` construct-payload aliases. Smaller
                        output and notably faster to analyze; only worth it if
                        your scripts rarely call `construct`.
    -h, --help          Print this help.
    -V, --version       Print the version.
";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut options = luau_lad_backend::Options::default();
    let mut check = false;
    let mut positional: Vec<String> = Vec::new();

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                println!("lad-luau {}", env!("CARGO_PKG_VERSION"));
                return Ok(ExitCode::SUCCESS);
            }
            "--fflags" => {
                print!("{}", luau_lad_backend::fflags_args());
                return Ok(ExitCode::SUCCESS);
            }
            "-c" | "--check" => check = true,
            "--no-init-types" => options.init_types = false,
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`\n\n{USAGE}"));
            }
            other => positional.push(other.to_string()),
        }
    }

    let Some(input) = positional.first() else {
        return Err(format!("missing <input.lad.json>\n\n{USAGE}"));
    };
    if positional.len() > 2 {
        return Err(format!("too many arguments\n\n{USAGE}"));
    }
    let output = positional.get(1).map(PathBuf::from);

    let source =
        std::fs::read_to_string(input).map_err(|e| format!("failed to read `{input}`: {e}"))?;
    let lad = ladfile::parse_lad_file(&source)
        .map_err(|e| format!("failed to parse `{input}` as a LAD file: {e}"))?;
    let defs = luau_lad_backend::lad_to_luau_with(&lad, &options);

    if check {
        let Some(path) = output else {
            return Err(format!(
                "--check needs an <output.d.luau> to compare against\n\n{USAGE}"
            ));
        };
        return check_against(&path, &defs);
    }

    match output {
        Some(path) => {
            std::fs::write(&path, defs)
                .map_err(|e| format!("failed to write `{}`: {e}", path.display()))?;
            eprintln!("wrote {}", path.display());
        }
        None => print!("{defs}"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Compare freshly generated definitions against what is on disk, reporting the
/// first line that differs. Returns a failure *code* rather than an `Err` when
/// they simply differ — that is a normal `--check` outcome, not a malfunction.
fn check_against(path: &Path, defs: &str) -> Result<ExitCode, String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(existing) => existing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("{} does not exist; run without --check", path.display());
            return Ok(ExitCode::FAILURE);
        }
        Err(e) => return Err(format!("failed to read `{}`: {e}", path.display())),
    };

    if existing == defs {
        eprintln!("{} is up to date", path.display());
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("{} is out of date; regenerate it", path.display());
    let (mut on_disk, mut generated) = (existing.lines(), defs.lines());
    for line in 1.. {
        match (on_disk.next(), generated.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (None, None) => break,
            (a, b) => {
                eprintln!("  first difference at line {line}:");
                eprintln!("    on disk:   {}", a.unwrap_or("<end of file>"));
                eprintln!("    generated: {}", b.unwrap_or("<end of file>"));
                break;
            }
        }
    }
    Ok(ExitCode::FAILURE)
}
