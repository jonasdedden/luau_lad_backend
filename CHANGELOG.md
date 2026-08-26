# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-26

Initial release: a native Luau (`.d.luau`) LAD backend, supporting
`bevy_mod_scripting` 0.17–0.19 (`ladfile` 0.17–0.19).

### Added

- **Core conversion.** `lad_to_luau(&LadFile) -> String` emits
  `declare class … end` / `declare name: T` definitions that
  `luau-lsp analyze --defs=…` checks scripts against. `lad_to_luau_with` takes
  an `Options` for the one part of the output that is a trade-off rather than a
  rendering (`init_types`, below).
- **`LuauLadPlugin`**, a `ladfile::LadFilePlugin` processor, so the backend
  drops straight into BMS's `ScriptingFilesGenerationPlugin` processor list.
- **`lad-luau` CLI** for converting a `.lad.json` to `.d.luau`, with `--check`
  (regenerate and diff against the file on disk, reporting the first differing
  line and exiting non-zero when stale — the CI check every project that commits
  its definitions needs), plus `--help`, `--version`, `--fflags` and
  `--no-init-types`.
- **Every type is declared dynamically** from the registry. Nothing is
  hard-coded by name and nothing is restricted to components or resources;
  reserved-keyword bindings are skipped rather than given fabricated aliases,
  and reserved struct-field names are quoted.
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
- **`ReflectReference` base class**: the reference proxy type is declared as a
  class carrying its namespaced methods (`display`, `iter`, `len`,
  `variant_name`, …), and every declared class `extends` it, mirroring runtime
  dispatch. Signatures still render the reference primitive as `any`/`T`.
- **Enum variant support**: every enum exports a variant-name union
  (`export type StanceVariant = "Idle" | "Aggressive"`) and, when the registry
  records the backing `ReflectReference::variant_name` function, a narrowed
  `function variant_name(self): StanceVariant` override — impossible variant
  comparisons become compile errors. Struct-variant fields become optional
  members (same-named fields of different types union); tuple variants fall back
  honestly (omitted). A real member named `variant_name` shadows the override,
  matching runtime field-first dispatch.
- **Argument and return documentation.** `ladfile_builder` parses the
  `Arguments:` / `Returns:` sections out of a Rust doc comment into structured
  `LadArgument` entries and truncates the main docstring there, so a backend
  that renders only the main docstring drops them entirely. They are emitted as
  `@param` / `@return` lines whose names match the rendered signature.
  (Per-*field* docs are impossible: `LadNamedField` carries no documentation, so
  the LAD format does not record them.)
- **Typed `construct` payloads**: `export type <Class>Init = { … }` aliases,
  generated from the same field data as the class body, so a payload literal can
  be checked before being handed to `construct`'s untyped `{ [string]: any }`.
  Entries are optional (BMS applies the payload over `Default`). Catches wrong
  value types and bad enum variant names; does *not* catch a misspelled key,
  since Luau permits extra properties in a table literal. On a full Bevy registry
  the aliases cost ~10% file size and ~35% `luau-lsp analyze` time, so they can
  be declined with `Options::init_types`.
- **The luau-lsp FFlags are the crate's business**, not the user's: named in
  every generated file's header comment, exported as `FFLAGS` / `fflags_args()`,
  printed by `lad-luau --fflags`, and writable to a companion file by
  `LuauLadPlugin::fflags_filename` — so a project stops hand-syncing the same
  five flags between CI and its editor config.
- **Tests**: unit tests, an end-to-end `luau-lsp` round-trip (skipped when the
  binary isn't installed), and a full Bevy + `bevy_mod_scripting` integration in
  the separate, non-published `bevy_integration_test` workspace member, which
  type-checks scripts against definitions generated from a live reflection
  registry.

### Notes on rendering decisions

- **Plain struct fields are not optional.** A `MonoVariant::Struct`'s fields are
  always present on a live reference, so they are declared `x: number` rather
  than `x: number?` and consumers need no `or 0` fallback at every use site.
  Fields of an *enum's* struct variants stay optional — those genuinely may
  belong to an inactive variant.
- **Identical overloads are emitted once.** BMS registers some functions more
  than once (operator impls arrive both with argument names and without).
  `luau-lsp` merges duplicate members into an overload set, so emitting every
  copy was never wrong, just repetitive: it inflates a file that already strains
  Luau's inference budgets and makes "no overload matched" list the same
  candidate twice. Copies that render to the same signature collapse to the
  best-documented one; genuinely distinct overloads are unaffected. (208 of 4942
  members on a real Bevy 0.18 registry.)
- **Heterogeneous tuples render as a union** (`{ number | string }`) rather than
  as an array of the *first* element's type, which would silently understate the
  value. Unions whose members share a Luau type collapse, so no `string | string`.
- **Colliding field keys are skipped with a warning** rather than silently
  shadowing an earlier field. Class names are made unique by suffixing, but a
  field key cannot be: it is what reflect indexing uses at runtime, so a renamed
  field would be backed by nothing.

[Unreleased]: https://github.com/jonasdedden/luau_lad_backend/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jonasdedden/luau_lad_backend/releases/tag/v0.1.0
