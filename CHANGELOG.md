# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added

- Initial native Luau (`.d.luau`) LAD backend.
  - `lad_to_luau(&LadFile) -> String`: pure conversion emitting
    `declare class … end` / `declare name: T`.
  - `LuauLadPlugin`: a `ladfile::LadFilePlugin` processor for BMS's
    `ScriptingFilesGenerationPlugin`.
  - `lad-luau` CLI for converting a `.lad.json` to `.d.luau`.
  - Declares every type dynamically, skips reserved-keyword bindings instead of
    fabricating aliases, and quotes reserved struct-field names.
