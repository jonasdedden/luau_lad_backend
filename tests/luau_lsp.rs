//! End-to-end integration test: generate a `.d.luau` from a real LAD file with
//! this crate, then have `luau-lsp` type-check scripts against it — asserting that
//! a correct script passes and a deliberately wrong one is rejected.
//!
//! `luau-lsp` is an external binary, so the test locates it via the `LUAU_LSP`
//! environment variable (a path to the binary) or on `PATH` as `luau-lsp`. If it
//! can't be found the test **skips** (prints a notice and passes), so the crate
//! still builds and tests cleanly where the tool isn't installed. Point it at a
//! binary to actually run the check:
//!
//! ```bash
//! LUAU_LSP=/path/to/luau-lsp cargo test --test luau_lsp
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

/// A script that uses the generated API correctly: the typed global function, a
/// field on a generated class (reached via an honest cast), and the exported
/// enum variant-name union. Must type-check.
const GOOD_SCRIPT: &str = "\
--!strict
local n: number = hello_world(1)
local s = (nil :: any) :: PlainStructType
local _ = s.int_field
local v: EnumTypeVariant = \"Unit\"
local _ = v
local _ = n + 1
";

/// Accesses a field that does not exist on the generated `PlainStructType` class —
/// `luau-lsp` must reject it.
const BAD_SCRIPT: &str = "\
--!strict
local s = (nil :: any) :: PlainStructType
local _ = s.not_a_real_field
";

/// Uses a variant name outside the generated `EnumTypeVariant` union — `luau-lsp`
/// must reject it.
const BAD_VARIANT_SCRIPT: &str = "\
--!strict
local v: EnumTypeVariant = \"Nope\"
local _ = v
";

#[test]
fn luau_lsp_typechecks_generated_defs() {
    let Some(lsp) = find_luau_lsp() else {
        eprintln!(
            "SKIP: luau-lsp not found. Set LUAU_LSP=/path/to/luau-lsp (or put it on PATH) \
             to run this integration test."
        );
        return;
    };

    // Generate the definition file from the canonical example LAD file.
    let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).expect("example LAD file parses");
    let defs = luau_lad_backend::lad_to_luau(&lad);

    // Sanity: the surface the scripts rely on must actually be present.
    assert!(
        defs.contains("declare function hello_world(arg1: number): number"),
        "generated defs missing the expected global function:\n{defs}"
    );

    let dir = scratch_dir("bms_luau_lsp");
    let defs_path = dir.join("api.d.luau");
    let good_path = dir.join("good.luau");
    let bad_path = dir.join("bad.luau");
    std::fs::write(&defs_path, &defs).unwrap();
    std::fs::write(&good_path, GOOD_SCRIPT).unwrap();
    std::fs::write(&bad_path, BAD_SCRIPT).unwrap();

    // The correct script must type-check clean (exit 0, no diagnostics).
    let good = analyze(&lsp, &defs_path, &good_path);
    assert!(
        good.status.success(),
        "luau-lsp rejected a valid script (exit {:?}):\n{}",
        good.status.code(),
        good.combined()
    );

    // The wrong script must be rejected, with a type error mentioning the bad call.
    let bad = analyze(&lsp, &defs_path, &bad_path);
    assert!(
        !bad.status.success(),
        "luau-lsp accepted a script with a type error:\n{}",
        bad.combined()
    );
    assert!(
        bad.combined().contains("TypeError"),
        "expected a TypeError diagnostic, got:\n{}",
        bad.combined()
    );

    // A variant name outside the generated union must be rejected too.
    let bad_variant_path = dir.join("bad_variant.luau");
    std::fs::write(&bad_variant_path, BAD_VARIANT_SCRIPT).unwrap();
    let bad_variant = analyze(&lsp, &defs_path, &bad_variant_path);
    assert!(
        !bad_variant.status.success(),
        "luau-lsp accepted a variant outside the union:\n{}",
        bad_variant.combined()
    );
    assert!(
        bad_variant.combined().contains("TypeError"),
        "expected a TypeError diagnostic, got:\n{}",
        bad_variant.combined()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Run `luau-lsp analyze --defs=<defs> <script>` and capture its output.
fn analyze(lsp: &Path, defs: &Path, script: &Path) -> Output {
    let out = Command::new(lsp)
        .arg("analyze")
        .arg(format!("--defs={}", defs.display()))
        .arg(script)
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
/// Returns `None` if neither can be invoked.
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
