# luau_lad_backend

A native **Luau** (`.d.luau`) definition-file backend for
[`bevy_mod_scripting`](https://github.com/makspll/bevy_mod_scripting) (BMS) LAD
(Language Agnostic Definition) files, so Luau game scripts can be type-checked
with [`luau-lsp`](https://github.com/JohnnyMorganz/luau-lsp).

BMS ships LAD post-processors for the Lua Language Server (`--- @class` `.lua`)
and mdbook, but no native Luau one — and `luau-lsp` can't consume the LuaLS
dialect. This is that missing backend.

## Design

- **Every type is declared dynamically.** All types in the LAD file get a
  `declare class … end`; nothing is hard-coded (`World`, `Entity`, etc. are just
  types in the file) and nothing is restricted to components/resources.
- **Optional only where it is earned.** A plain struct's fields are always
  present on a live reference, so they are declared non-optional (`x: number`) —
  no `or 0` at every use site. Fields of an *enum's* struct variants stay
  optional (`level: number?`): they belong to one variant, and the value may
  currently be another.
- **Documented arguments and returns.** `ladfile_builder` parses the
  `Arguments:` / `Returns:` sections out of a Rust doc comment into structured
  entries and truncates the main docstring there, so rendering only the main
  docstring silently loses them. They are emitted as `@param` / `@return` lines
  whose names match the rendered signature:

  ```luau
  -- Deal damage to an enemy, applying its class defense.
  -- @param enemy - The enemy to damage.
  -- @return killed - True if this hit brought the enemy to 0 health.
  declare function damage_enemy(enemy: Entity, amount: number): boolean
  ```

  (Per-*field* docs cannot be emitted: `LadNamedField` carries no documentation,
  so the LAD format does not record them.)
- **Typed `construct` payloads.** `construct` takes an untyped
  `{ [string]: any }`, and Luau cannot derive a payload type from the
  registration's `T`, so the backend exports a payload alias per type:

  ```luau
  export type VelocityInit = { x: number?, y: number?, z: number? }
  export type StanceInit = { variant: StanceVariant }

  local payload: VelocityInit = { x = 1, y = 2, z = 3 }
  world.insert_component(e, types.Velocity, construct(types.Velocity, payload))
  ```

  Entries are optional because BMS applies the payload over `Default`. This
  catches wrong value types and bad enum variant names. It does **not** catch a
  misspelled key — Luau permits extra properties in a table literal. Types only;
  nothing is claimed to exist at runtime.

  These are the one part of the output worth weighing: on a full Bevy registry
  they add ~10% to the file and ~35% to `luau-lsp analyze` time, paid on every
  check. Turn them off with `Options::init_types` / `--no-init-types` if your
  scripts rarely call `construct`.
- **One member per distinct signature.** BMS registers some functions more than
  once (operator impls arrive both with argument names and without). `luau-lsp`
  merges duplicate members into an overload set, so the copies were never
  *wrong*, just repeated — they inflate a file that already strains luau's
  inference budgets and make "no overload matched" list the same candidate
  twice. Copies that render identically collapse to the best-documented one;
  genuinely distinct overloads are all kept.
- **Phantom-typed registrations.** BMS's dynamic world API (`get_component`,
  `insert_component`, `get_resource`, `construct`, …) traffics in runtime
  `ReflectReference` proxies whose concrete type the LAD file can't name. To
  type it anyway, the backend emits phantom brands and generic signatures:

  ```luau
  export type Reg<T> = ScriptComponentRegistration & { __component: T }

  declare class World
      get_component: <T>(entity: Entity, registration: Reg<T>) -> T?
      insert_component: <T>(entity: Entity, registration: Reg<T>, value: T) -> nil
      ...
  ```

  and types the `types` global as a closed table
  (`types: { Velocity: Reg<Velocity>, … }`), so
  `world.get_component(e, types.Velocity)` infers `Velocity?` with **zero
  casts**, and inserting the wrong component value is a compile error.
  Script-registered dynamic components join with one cast:
  `world.register_new_component("EnemyData") :: Reg<EnemyData>`. The phantom
  field never exists at runtime; it only carries `T` for the checker. The
  rewrite is **shape-driven** (a registration argument paired with
  `ReflectReference` in return/value position), not keyed to function names.
- **Enum variants, typed.** Every enum exports a variant-name union and — when
  the registry records the backing `ReflectReference::variant_name` function —
  narrows the inherited `variant_name` method to it:

  ```luau
  export type StanceVariant = "Idle" | "Aggressive"
  declare class Stance extends ReflectReference
      function variant_name(self): StanceVariant
  end
  ```

  so `if stance:variant_name() == "Sleepy"` is a compile error ("cannot be
  compared"). Named fields of struct variants become optional members (they
  may belong to an inactive variant); same-named fields of different types
  union.
- **`ReflectReference` as a base class.** Every script-visible value of a
  declared class is a reference proxy at runtime, resolving fields first and
  falling back to the `ReflectReference`-namespaced functions. The backend
  mirrors that: it declares `ReflectReference` with its methods
  (`display`, `iter`, `len`, `variant_name`, …) and every class `extends` it —
  subclass members shadow base methods exactly as runtime dispatch does. In
  *signatures* the reference primitive still renders as `any`/`T`; the class
  exists to grant the shared methods.
- **Honest keyword handling.** A binding whose name is a reserved Luau keyword is
  *skipped* (a renamed alias like `end_` would have nothing backing it at
  runtime). Reserved struct-field names are preserved via a quoted key
  (`["end"]: T`), which *is* backed by reflect index access.
- **Graceful fallback.** Anything that stays genuinely dynamic (`ScriptValue`
  payloads, `DynamicFunction` callbacks, asset handles, query results) becomes
  `any`, which Luau treats permissively, so scripts still type-check.

## Usage

### As a library

```rust
let lad = ladfile::parse_lad_file(json)?;
let defs: String = luau_lad_backend::lad_to_luau(&lad);

// …or with the optional parts turned off:
use luau_lad_backend::{lad_to_luau_with, Options};
let defs = lad_to_luau_with(&lad, &Options { init_types: false });
```

### As a generation-pipeline processor

`LuauLadPlugin` implements `ladfile::LadFilePlugin`, so it drops into BMS's
`ScriptingFilesGenerationPlugin` processor list:

```rust
use luau_lad_backend::LuauLadPlugin;

let mut settings = LadFileSettings::default();
settings.processors.push(Box::new(LuauLadPlugin::default())); // writes bindings.d.luau

// Optionally drop the required luau-lsp flags next to the definitions, so build
// scripts and editor configs read one generated list instead of hard-coding it.
settings.processors.push(Box::new(LuauLadPlugin {
    fflags_filename: Some("fflags.txt".into()),
    ..Default::default()
}));
```

### As a CLI

```bash
lad-luau bindings.lad.json bindings.d.luau   # or omit the output path for stdout
lad-luau --check bindings.lad.json bindings.d.luau   # CI: fail if committed defs are stale
lad-luau --fflags                            # print the analyze flags, one per line
lad-luau --no-init-types bindings.lad.json bindings.d.luau   # smaller, faster to analyze
lad-luau --help
```

`--check` regenerates in memory and compares against the file on disk, reporting
the first differing line and exiting non-zero — the check every project that
commits its definitions needs.

Then point `luau-lsp` at the result:

```bash
luau-lsp analyze --defs=bindings.d.luau scripts/level1.luau
```

```jsonc
// .vscode/settings.json
{ "luau-lsp.types.definitionFiles": ["bindings.d.luau"] }
```

## Testing

Three layers, in increasing fidelity:

1. **Unit tests** (`src/lib.rs`) — the conversion itself, including dynamic
   type declaration and reserved-keyword handling.
2. **Lightweight luau-lsp test** (`tests/luau_lsp.rs`) — generates a `.d.luau`
   from the bundled example LAD file and has `luau-lsp` type-check a correct and a
   wrong script (a missing field on a generated class, a bad argument).
3. **Full Bevy integration** (`bevy_integration_test/`, a separate, non-published
   workspace member) — builds a real headless Bevy + `bevy_mod_scripting` app,
   dumps its *live reflection registry* to a LAD file, converts it with this
   crate, and has `luau-lsp` type-check scripts that use the generated component
   classes and host globals. This keeps Bevy/BMS out of the published crate's
   dependency tree.

`luau-lsp` is an external binary, so the type-checking layers run only when it's
found (via `LUAU_LSP` or `PATH`) and skip cleanly otherwise:

```bash
LUAU_LSP=/path/to/luau-lsp cargo test --workspace   # everything
LUAU_LSP=/path/to/luau-lsp cargo test               # core crate only (fast)
```

## Status & scope

This is an early, standalone backend. It covers the common surface (named struct
fields, enum variants with typed `variant_name`, associated functions incl. the
inherited `ReflectReference` surface, host globals/functions, typed registration
access, `construct` payloads, and function/argument/return documentation). Not
yet handled, and resolved to `any` / omitted for now: tuple-struct and
tuple-variant positional fields, generic monomorphisations (declared once under
the base name), asset handles (`Handle<T>` ↔ `T` isn't recorded in the LAD
format), per-query typing of `ScriptQueryResult:components()` (not expressible
in Luau; read via the typed `get_component` instead), and callback signatures
for `register_callback` (not in the reflection registry). Contributions and
feedback welcome.

Two limits worth knowing before you rely on them:

- **Misspelled `construct` payload keys are not caught.** Luau permits extra
  properties in a table literal, so `<Class>Init` buys value types and enum
  variant names, not key spelling. Neither the old solver nor `LuauSolverV2`
  flags the extra key.
- **Per-field documentation cannot be emitted at all**, because `LadNamedField`
  has no documentation field — the LAD format does not carry it. Only a type's
  own docstring and its functions' docs survive the dump.

### Large registries and luau's inference budgets

A full Bevy app easily produces hundreds of classes plus the closed `types`
table over all of them, which can exceed luau's default type-inference limits
("Code is too complex to typecheck"). The flags to raise them are the crate's
business, not each project's, so every generated file names them in its header
comment, `lad-luau --fflags` prints them, `LuauLadPlugin::fflags_filename`
writes them to a companion file for tooling to read, and `FFLAGS` /
`fflags_args()` expose them to build scripts.

```bash
luau-lsp analyze $(lad-luau --fflags) --defs=bindings.d.luau scripts/*.luau
```

(or the equivalent `luau-lsp.fflags.override` block in VS Code settings).

The one lever on output size is overload deduplication, which is unconditional
(208 redundant members out of 4942 on a real Bevy 0.18 registry). Dropping
"unreachable" types is not offered: it was tried, and on that same registry it
removed exactly one class of 771 — nearly everything registered is a component,
a resource, or reachable from one through a field or method signature, so a full
registry is genuinely almost all reachable.

## License

Licensed under either of MIT or Apache-2.0 at your option.
