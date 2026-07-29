# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Changed

- **Plain struct fields are no longer optional.** A `MonoVariant::Struct`'s
  fields are always present on a live reference, so they are now declared
  `x: number` rather than `x: number?`, and consumers can drop the `or 0`
  fallbacks that forced at every use site. Fields of an *enum's* struct variants
  stay optional — those genuinely may belong to an inactive variant.
  **Breaking** for scripts that relied on the optionality: a now-impossible
  `== nil` comparison becomes a type error, which is the point.
- **Identical overloads are emitted once.** BMS registers some functions more
  than once — operator impls arrive both with argument names and without — and
  every copy used to be emitted. `luau-lsp` merges duplicate members into an
  overload set, so they were never wrong, just repeated: they inflate a file
  that already strains luau's inference budgets and make "no overload matched"
  list the same candidate twice. Copies that render to the same signature now
  collapse to the best-documented one; genuinely distinct overloads are
  unaffected. (208 of 4942 members on a real Bevy 0.18 registry.)
- **Heterogeneous tuples render as a union** (`{ number | string }`) instead of
  an array of the *first* element's type, which silently understated the value.
  Unions whose members share a Luau type also collapse, so no more
  `string | string`.

### Added

- **Argument and return documentation.** `ladfile_builder` parses the
  `Arguments:` / `Returns:` sections out of a Rust doc comment into structured
  `LadArgument` entries and truncates the main docstring there, so a backend
  that renders only the main docstring dropped them entirely. They are now
  emitted as `@param` / `@return` lines whose names match the rendered
  signature. (Per-*field* docs remain impossible: `LadNamedField` carries no
  documentation, so the LAD format does not record them.)
- **Typed `construct` payloads**: `export type <Class>Init = { … }` aliases,
  generated from the same field data as the class body, so a payload literal can
  be checked before being handed to `construct`'s untyped `{ [string]: any }`.
  Entries are optional (BMS applies the payload over `Default`). Catches wrong
  value types and bad enum variant names; does *not* catch a misspelled key,
  since Luau permits extra properties in a table literal. Opt out with
  `Options::init_types`.
- **The luau-lsp FFlags are now the crate's business.** Named in every generated
  file's header comment, exported as `FFLAGS` / `fflags_args()`, printed by
  `lad-luau --fflags`, and writable to a companion file by
  `LuauLadPlugin::fflags_filename` — so a project stops hand-syncing the same
  five flags between CI and its editor config.
- **`Options` / `lad_to_luau_with`** for the one part of the output that is a
  trade-off rather than a rendering: `init_types`. On a full Bevy registry the
  payload aliases cost ~10% file size and ~35% `luau-lsp analyze` time, which is
  worth declining if your scripts rarely call `construct`.
- **CLI**: `--check` (regenerate and diff against the file on disk, reporting
  the first differing line and exiting non-zero when stale — the CI check every
  project that commits its definitions needs), plus `--help`, `--version`,
  `--fflags` and `--no-init-types`.

### Fixed

- A field whose sanitised Luau key collides with an earlier one is skipped with
  a warning instead of silently shadowing it. Class names are made unique by
  suffixing, but a field key cannot be: it is what reflect indexing uses at
  runtime, so a renamed field would be backed by nothing.

- **Enum variant support**: every enum exports a variant-name union
  (`export type StanceVariant = "Idle" | "Aggressive"`) and, when the registry
  records the backing `ReflectReference::variant_name` function, a narrowed
  `function variant_name(self): StanceVariant` override — impossible variant
  comparisons become compile errors. Struct-variant fields become optional
  members (same-named fields of different types union); tuple variants fall
  back honestly (omitted). A real member named `variant_name` shadows the
  override, matching runtime field-first dispatch.
- **`ReflectReference` base class**: the reference proxy type is now declared
  as a class carrying its namespaced methods (`display`, `iter`, `len`,
  `variant_name`, …) — previously these were silently dropped — and every
  declared class `extends` it, mirroring runtime dispatch. Signatures still
  render the reference primitive as `any`/`T`.
- **Phantom-typed registrations**: statically typed component/resource access
  across BMS's dynamic world API.
  - Emits `Reg<T>` / `ResReg<T>` / `TypeReg<T>` brand aliases
    (`ScriptComponentRegistration & { __component: T }` etc.).
  - Rewrites functions that pair a registration argument with a
    `ReflectReference` (return or trailing value argument) into generic
    signatures: `get_component: <T>(entity: Entity, registration: Reg<T>) -> T?`,
    `insert_component: <T>(…, value: T) -> nil`, `construct<T>(…): T`, … The
    rewrite is shape-driven, not keyed to function names; functions where a
    `ReflectReference` means something else (e.g. `get_asset`'s handle) keep
    their honest `any`.
  - Types the `types` global as a closed table
    (`types: { Velocity: Reg<Velocity>, …, [string]: <registration union> }`),
    so reflected components need **zero casts**; script-registered dynamic
    components need one (`world.register_new_component("X") :: Reg<XShape>`).
- Initial native Luau (`.d.luau`) LAD backend.
  - `lad_to_luau(&LadFile) -> String`: pure conversion emitting
    `declare class … end` / `declare name: T`.
  - `LuauLadPlugin`: a `ladfile::LadFilePlugin` processor for BMS's
    `ScriptingFilesGenerationPlugin`.
  - `lad-luau` CLI for converting a `.lad.json` to `.d.luau`.
  - Declares every type dynamically, skips reserved-keyword bindings instead of
    fabricating aliases, and quotes reserved struct-field names.
  - Tests: unit tests, a lightweight `luau-lsp` round-trip, and a full Bevy +
    `bevy_mod_scripting` integration (separate `bevy_integration_test` workspace
    member) that type-checks scripts against definitions generated from a live
    reflection registry.
