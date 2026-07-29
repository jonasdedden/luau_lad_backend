//! Fully-integrated end-to-end test.
//!
//! Builds a real headless Bevy + `bevy_mod_scripting` app, dumps its live
//! reflection registry to a LAD file, converts that to a `.d.luau` with
//! `luau_lad_backend`, and then has `luau-lsp` type-check Luau scripts that
//! actually use the generated component classes and host globals — confirming a
//! correct script passes and that real type errors (a bad field, a bad argument)
//! are caught.
//!
//! `luau-lsp` is external, so the type-checking half is skipped (the test still
//! validates generation + the produced definitions) when the binary isn't found.
//! Set `LUAU_LSP=/path/to/luau-lsp` or put it on `PATH` to run it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Uses the typed world API end to end — reflected components with zero casts
/// (via the branded `types` table and generic `get_component`/`insert_component`),
/// plus a script-defined dynamic component typed with a single `Reg<T>` cast.
/// Must pass.
const GOOD_SCRIPT: &str = "\
--!strict
local speed: number = magnitude(3, 4, 0)
info(\"hero ready\")

local e = world.spawn()
world.insert_component(e, types.Health, construct(types.Health, { current = 50, max = 50 }))

-- Zero-cast typed reads: `types.Velocity` is branded, so `T` is inferred.
-- Reflected fields are optional (`number?`), matching how reflection exposes them.
local vel = world.get_component(e, types.Velocity)
local vx: number? = if vel then vel.x else nil

local health = world.get_component(e, types.Health)
local current: number? = if health then health.current else nil

-- Dynamic (script-registered) component: shape declared once, cast once.
type EnemyData = { kind: string, bounty: number }
local EnemyReg = world.register_new_component(\"EnemyData\") :: Reg<EnemyData>
world.insert_component(e, EnemyReg, { kind = \"grunt\", bounty = 5 })
local enemy = world.get_component(e, EnemyReg)
local bounty: number? = if enemy then enemy.bounty else nil

-- Enums: construct a variant, inspect it through the *typed* variant_name
-- override (narrowed to the exported union), zero casts.
local stance = construct(types.Stance, { variant = \"Aggressive\" })
local stance_name: StanceVariant = stance:variant_name()
if stance_name == \"Idle\" then
	info(\"chilling\")
end

-- Every declared class extends ReflectReference, so proxy methods like
-- display are typed everywhere.
local shown: string = stance:display()

local _ = (speed :: any) and vx and current and bounty and shown
";

/// Accesses a field that does not exist on `Velocity` — through a typed,
/// cast-free `get_component` read.
const BAD_FIELD_SCRIPT: &str = "\
--!strict
local vel = world.get_component(world.spawn(), types.Velocity)
if vel then
	local _ = vel.acceleration
end
";

/// Passes a string where the generated `magnitude` global wants a number.
const BAD_ARG_SCRIPT: &str = "\
--!strict
local _ = magnitude(\"fast\", 1, 2)
";

/// Inserts a `Velocity` value into the `Health` component slot — the write path
/// must be checked via the generic `insert_component`.
const BAD_INSERT_SCRIPT: &str = "\
--!strict
local e = world.spawn()
local vel = world.get_component(e, types.Velocity)
if vel then
	world.insert_component(e, types.Health, vel)
end
";

/// Typo in a dynamic component's field, after the single honest `Reg<T>` cast.
const BAD_DYNAMIC_SCRIPT: &str = "\
--!strict
type EnemyData = { kind: string, bounty: number }
local EnemyReg = world.register_new_component(\"EnemyData\") :: Reg<EnemyData>
local enemy = world.get_component(world.spawn(), EnemyReg)
if enemy then
	local _ = enemy.bouty
end
";

/// Compares a typed variant name against a variant that does not exist on the
/// enum — the narrowed `variant_name` union must reject it.
const BAD_VARIANT_SCRIPT: &str = "\
--!strict
local stance = construct(types.Stance, { variant = \"Aggressive\" })
if stance:variant_name() == \"Sleepy\" then
	info(\"impossible\")
end
";

#[test]
fn generated_defs_typecheck_in_a_real_environment() {
    let dir = scratch_dir("bms_luau_bevy");

    // 1. Generate a LAD file from a live Bevy + BMS reflection registry.
    let lad_path = bevy_integration_test::generate_lad(&dir);
    let lad_src = std::fs::read_to_string(&lad_path).expect("LAD file was generated");
    let lad = ladfile::parse_lad_file(&lad_src).expect("generated LAD parses");

    // 2. Convert it with the crate under test.
    let defs = luau_lad_backend::lad_to_luau(&lad);
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
        // The phantom-typed registration machinery.
        "export type Reg<T> = ScriptComponentRegistration & { __component: T }",
        "get_component: <T>(entity: Entity, registration: Reg<T>) -> T?",
        "insert_component: <T>(entity: Entity, registration: Reg<T>, value: T) -> nil",
        "\tVelocity: Reg<Velocity>,",
        "\tHealth: Reg<Health>,",
        // Enum variant support: exported union + typed variant_name override,
        // derived from the live registry (BMS records the backing function).
        "export type StanceVariant = \"Idle\" | \"Aggressive\"",
        "declare class Stance extends ReflectReference",
        "\tfunction variant_name(self): StanceVariant",
        // The materialized reference base class every declared class extends.
        "declare class ReflectReference\n",
        "declare class Health extends ReflectReference",
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

    // The typed write path: a Velocity value into the Health slot must fail.
    let bad_insert = analyze(&lsp, &defs_path, &dir, "bad_insert.luau", BAD_INSERT_SCRIPT);
    assert!(
        !bad_insert.status.success(),
        "wrong-component insert not rejected:\n{}",
        bad_insert.combined()
    );
    assert!(
        bad_insert.combined().contains("TypeError"),
        "expected an insert TypeError, got:\n{}",
        bad_insert.combined()
    );

    // Dynamic components: a field typo behind the single `Reg<T>` cast must fail.
    let bad_dynamic = analyze(
        &lsp,
        &defs_path,
        &dir,
        "bad_dynamic.luau",
        BAD_DYNAMIC_SCRIPT,
    );
    assert!(
        !bad_dynamic.status.success(),
        "dynamic-component field typo not rejected:\n{}",
        bad_dynamic.combined()
    );
    assert!(
        bad_dynamic.combined().contains("'bouty'"),
        "expected a field TypeError on `bouty`, got:\n{}",
        bad_dynamic.combined()
    );

    // Enums: comparing the narrowed variant_name against a non-variant must fail.
    let bad_variant = analyze(
        &lsp,
        &defs_path,
        &dir,
        "bad_variant.luau",
        BAD_VARIANT_SCRIPT,
    );
    assert!(
        !bad_variant.status.success(),
        "non-existent variant comparison not rejected:\n{}",
        bad_variant.combined()
    );
    assert!(
        bad_variant.combined().contains("TypeError"),
        "expected a variant TypeError, got:\n{}",
        bad_variant.combined()
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
