# bevy_mod_scripting_luau

A native **Luau** (`.d.luau`) definition-file backend for
[`bevy_mod_scripting`](https://github.com/makspll/bevy_mod_scripting) (BMS) LAD
(Language Agnostic Definition) files, so Luau game scripts can be type-checked
with [`luau-lsp`](https://github.com/JohnnyMorganz/luau-lsp).

BMS ships LAD post-processors for the Lua Language Server (`--- @class` `.lua`)
and mdbook, but no native Luau one — and `luau-lsp` can't consume the LuaLS
dialect. This is that missing backend.

## Design

Like the other LAD backends, it only **describes what already exists** in the
reflection registry — it adds no conventions of its own:

- **Every type is declared dynamically.** All types in the LAD file get a
  `declare class … end`; nothing is hard-coded (`World`, `Entity`, etc. are just
  types in the file) and nothing is restricted to components/resources.
- **No opinionated bindings.** The output describes the registry as-is; there are
  no extra globals or phantom types that require a particular host setup.
- **Honest keyword handling.** A binding whose name is a reserved Luau keyword is
  *skipped* (a renamed alias like `end_` would have nothing backing it at
  runtime). Reserved struct-field names are preserved via a quoted key
  (`["end"]: T`), which *is* backed by reflect index access.
- **Graceful fallback.** Anything that can't be resolved to a concrete type
  becomes `any`, which Luau treats permissively, so scripts still type-check.

## Usage

### As a library

```rust
let lad = ladfile::parse_lad_file(json)?;
let defs: String = bevy_mod_scripting_luau::lad_to_luau(&lad);
```

### As a generation-pipeline processor

`LuauLadPlugin` implements `ladfile::LadFilePlugin`, so it drops into BMS's
`ScriptingFilesGenerationPlugin` processor list:

```rust
use bevy_mod_scripting_luau::LuauLadPlugin;

let mut settings = LadFileSettings::default();
settings.processors.push(Box::new(LuauLadPlugin::default())); // writes bindings.d.luau
```

### As a CLI

```bash
lad-luau bindings.lad.json bindings.d.luau   # or omit the output path for stdout
```

Then point `luau-lsp` at the result:

```bash
luau-lsp analyze --defs=bindings.d.luau scripts/level1.luau
```

```jsonc
// .vscode/settings.json
{ "luau-lsp.types.definitionFiles": ["bindings.d.luau"] }
```

## Status & scope

This is an early, standalone backend. It covers the common surface (named struct
fields, associated functions, host globals/functions). Not yet handled, and
resolved to `any` / omitted for now: enum variants, tuple-struct positional
fields, generic monomorphisations (declared once under the base name), and
`ReflectReference` base-class inheritance. Contributions and feedback welcome.

## License

Licensed under either of MIT or Apache-2.0 at your option.
