//! Fully-integrated end-to-end test.
//!
//! Builds a real headless Bevy + `bevy_mod_scripting` app, dumps its live
//! reflection registry to a LAD file, converts that to a `.d.luau` with
//! `bevy_mod_scripting_luau`, and then has `luau-lsp` type-check Luau scripts that
//! actually use the generated component classes and host globals — confirming a
//! correct script passes and that real type errors (a bad field, a bad argument)
//! are caught.
//!
//! `luau-lsp` is external, so the type-checking half is skipped (the test still
//! validates generation + the produced definitions) when the binary isn't found.
//! Set `LUAU_LSP=/path/to/luau-lsp` or put it on `PATH` to run it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Uses the generated component classes and the typed host global — must pass.
const GOOD_SCRIPT: &str = "\
--!strict
local speed: number = magnitude(3, 4, 0)
info(\"hero ready\")

-- `world.get_component` is dynamic, so obtain a typed handle the honest way: a
-- cast to a generated class. Field access is then checked against the class.
-- Reflected fields are optional (`number?`), matching how reflection exposes them.
local vel = (nil :: any) :: Velocity
local vx: number? = vel.x
local vy: number? = vel.y

local health = (nil :: any) :: Health
local current: number? = health.current
local _ = (speed :: any) and vx and vy and current
";

/// Accesses a field that does not exist on the generated `Velocity` class.
const BAD_FIELD_SCRIPT: &str = "\
--!strict
local vel = (nil :: any) :: Velocity
local _ = vel.acceleration
";

/// Passes a string where the generated `magnitude` global wants a number.
const BAD_ARG_SCRIPT: &str = "\
--!strict
local _ = magnitude(\"fast\", 1, 2)
";

#[test]
fn generated_defs_typecheck_in_a_real_environment() {
    let dir = scratch_dir("bms_luau_bevy");

    // 1. Generate a LAD file from a live Bevy + BMS reflection registry.
    let lad_path = bevy_integration_test::generate_lad(&dir);
    let lad_src = std::fs::read_to_string(&lad_path).expect("LAD file was generated");
    let lad = ladfile::parse_lad_file(&lad_src).expect("generated LAD parses");

    // 2. Convert it with the crate under test.
    let defs = bevy_mod_scripting_luau::lad_to_luau(&lad);
    let defs_path = dir.join("api.d.luau");
    std::fs::write(&defs_path, &defs).unwrap();

    // 3. The reflected components and host globals must be present, as classes with
    //    their real fields — proving types are declared dynamically from the
    //    registry, not hard-coded.
    for needle in [
        "declare class Velocity",
        "declare class Health",
        "declare class Position",
        "declare class World",
        "declare world: World",
        "declare function magnitude(",
    ] {
        assert!(defs.contains(needle), "generated defs missing `{needle}`");
    }
    assert!(
        defs.contains("current: number?"),
        "Health.current field missing/!optional"
    );

    // 4. Type-check real scripts against the generated definitions with luau-lsp.
    let Some(lsp) = find_luau_lsp() else {
        eprintln!(
            "SKIP luau-lsp checks: binary not found. Set LUAU_LSP=/path/to/luau-lsp \
             (or put it on PATH). Generation + definition assertions still ran."
        );
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

    let good = analyze(&lsp, &defs_path, &dir, "good.luau", GOOD_SCRIPT);
    assert!(
        good.status.success(),
        "luau-lsp rejected a correct script (exit {:?}):\n{}",
        good.status.code(),
        good.combined()
    );

    let bad_field = analyze(&lsp, &defs_path, &dir, "bad_field.luau", BAD_FIELD_SCRIPT);
    assert!(
        !bad_field.status.success(),
        "bad field not rejected:\n{}",
        bad_field.combined()
    );
    assert!(
        bad_field.combined().contains("'acceleration'")
            || bad_field.combined().contains("TypeError"),
        "expected a field TypeError, got:\n{}",
        bad_field.combined()
    );

    let bad_arg = analyze(&lsp, &defs_path, &dir, "bad_arg.luau", BAD_ARG_SCRIPT);
    assert!(
        !bad_arg.status.success(),
        "bad argument not rejected:\n{}",
        bad_arg.combined()
    );
    assert!(
        bad_arg.combined().contains("TypeError"),
        "expected an argument TypeError, got:\n{}",
        bad_arg.combined()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Write `script` into `dir` and run `luau-lsp analyze --defs=<defs> <script>`.
fn analyze(lsp: &Path, defs: &Path, dir: &Path, name: &str, script: &str) -> Output {
    let script_path = dir.join(name);
    std::fs::write(&script_path, script).unwrap();
    let out = Command::new(lsp)
        .arg("analyze")
        .arg(format!("--defs={}", defs.display()))
        .arg(&script_path)
        .output()
        .expect("failed to run luau-lsp");
    Output {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

struct Output {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl Output {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Locate the `luau-lsp` binary, preferring the `LUAU_LSP` override, then `PATH`.
fn find_luau_lsp() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("LUAU_LSP") {
        candidates.push(PathBuf::from(p));
    }
    candidates.push(PathBuf::from("luau-lsp"));
    candidates.into_iter().find(|bin| {
        Command::new(bin)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    })
}

/// A unique, freshly-created scratch directory under the system temp dir.
fn scratch_dir(prefix: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    dir.push(format!("{prefix}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}
