//! Native **Luau** (`.d.luau`) definition-file backend for `bevy_mod_scripting`
//! LAD ([Language Agnostic Definition]) files.
//!
//! `bevy_mod_scripting` (BMS) ships LAD post-processors for the Lua Language
//! Server (`--- @class` `.lua`) and for mdbook, but no native Luau one — and
//! [`luau-lsp`] cannot consume the LuaLS dialect. This crate is that missing
//! backend: it turns a [`LadFile`] into `declare class … end` / `declare name: T`
//! definitions that `luau-lsp analyze --defs=…` checks scripts against.
//!
//! Like the other LAD backends, it only *describes what already exists* in the
//! reflection registry — it adds no conventions of its own. Every type in the LAD
//! file is declared dynamically; nothing is special-cased or hard-coded. The
//! public entry point [`lad_to_luau`] is a pure `&LadFile -> String` conversion,
//! and [`LuauLadPlugin`] is the [`LadFilePlugin`] processor that writes the file
//! into a generation output directory.
//!
//! The reflection API BMS exposes to Lua is *dynamic* (component references are
//! `ReflectReference` proxies resolved at runtime), so the generated types cover
//! the statically-knowable surface: every reflected type and its named fields, the
//! registered host globals/functions, and instance handles like `world`. Anything
//! that can't be resolved to a concrete type becomes `any`, which Luau treats
//! permissively, so scripts still type-check.
//!
//! [Language Agnostic Definition]: ladfile
//! [`luau-lsp`]: https://github.com/JohnnyMorganz/luau-lsp

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};

use ladfile::{
    LadArgument, LadFieldOrVariableKind, LadFile, LadFilePlugin, LadFunction, LadFunctionNamespace,
    LadTypeId, LadTypeLayout, LadVariant, ReflectionPrimitiveKind,
};

/// Render a parsed LAD file as Luau definition-file source.
///
/// This is the whole backend as a pure function; [`LuauLadPlugin`] just calls it
/// and writes the result to disk.
pub fn lad_to_luau(lad: &LadFile) -> String {
    Converter::new(lad).render()
}

/// A [`LadFilePlugin`] post-processor that writes a native Luau `.d.luau` file.
///
/// Add it to BMS's `ScriptingFilesGenerationPlugin` processor list; on generation
/// it writes [`filename`](Self::filename) into the configured output directory.
#[derive(Clone, Debug)]
pub struct LuauLadPlugin {
    /// File name to write inside the generation output directory.
    pub filename: PathBuf,
}

impl Default for LuauLadPlugin {
    fn default() -> Self {
        Self {
            filename: PathBuf::from("bindings.d.luau"),
        }
    }
}

impl LadFilePlugin for LuauLadPlugin {
    fn name(&self) -> &'static str {
        "Luau definition file generator"
    }

    fn run(&self, ladfile: &LadFile, output_dir: &Path) -> Result<(), Box<dyn Error>> {
        std::fs::write(output_dir.join(&self.filename), lad_to_luau(ladfile))?;
        Ok(())
    }
}

struct Converter<'a> {
    lad: &'a LadFile,
    /// type id -> the Luau identifier we declare it under (sanitised + unique).
    /// Contains every non-primitive type in the LAD file, so type references
    /// resolve dynamically with no hard-coded names.
    names: HashMap<LadTypeId, String>,
}

impl<'a> Converter<'a> {
    fn new(lad: &'a LadFile) -> Self {
        // Assign a stable, unique Luau name to every non-primitive type. Iterating
        // a sorted key list keeps both the names and the output deterministic.
        let mut ids: Vec<&LadTypeId> = lad
            .types
            .iter()
            .filter(|(_, def)| def.metadata.mapped_to_primitive_kind.is_none())
            .map(|(id, _)| id)
            .collect();
        ids.sort_by(|a, b| lad.types[*a].identifier.cmp(&lad.types[*b].identifier));

        let mut names = HashMap::new();
        let mut used: HashSet<String> = HashSet::new();
        for id in ids {
            // Type identifiers are CamelCase Rust paths, so they never collide with
            // (lowercase) Luau keywords; sanitising is enough, no escaping needed.
            let mut name = sanitize(&lad.types[id].identifier);
            if name.is_empty() {
                name = "Unknown".to_string();
            }
            while !used.insert(name.clone()) {
                name.push('_');
            }
            names.insert(id.clone(), name);
        }

        Converter { lad, names }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("--!strict\n");
        out.push_str(
            "-- AUTO-GENERATED from the Bevy reflection registry via bevy_mod_scripting_luau.\n",
        );
        out.push_str("-- Do not edit by hand.\n\n");

        // Group functions by namespace once.
        let mut methods: HashMap<&LadTypeId, Vec<&LadFunction>> = HashMap::new();
        let mut global_fns: Vec<&LadFunction> = Vec::new();
        for func in self.lad.functions.values() {
            match &func.namespace {
                LadFunctionNamespace::Type(id) => methods.entry(id).or_default().push(func),
                LadFunctionNamespace::Global => global_fns.push(func),
            }
        }

        // Every non-primitive type gets a class declaration, in name order.
        let mut typed: Vec<(&LadTypeId, &str)> =
            self.names.iter().map(|(id, n)| (id, n.as_str())).collect();
        typed.sort_by_key(|(_, n)| *n);
        for (id, name) in typed {
            let Some(def) = self.lad.types.get(id) else {
                continue;
            };
            if let Some(doc) = &def.documentation {
                push_doc(&mut out, doc, "");
            }
            out.push_str(&format!("declare class {name}\n"));

            // Named struct fields. Reflected fields may be absent on any given
            // instance, so they are optional. Keyword names are preserved via a
            // quoted key (`["end"]`), which is genuinely backed by reflect indexing.
            if let LadTypeLayout::MonoVariant(LadVariant::Struct { fields, .. }) = &def.layout {
                for f in fields {
                    out.push_str(&format!(
                        "\t{}: {}?\n",
                        field_key(&f.name),
                        self.kind(&f.type_)
                    ));
                }
            }

            // Associated functions: a method if its first script-visible argument is
            // the owning type, otherwise a dot-callable function field.
            let mut fns = methods.remove(id).unwrap_or_default();
            fns.sort_by_key(|f| f.identifier.to_string());
            for func in fns {
                self.push_member(&mut out, name, func);
            }
            out.push_str("end\n\n");
        }

        // Global host functions.
        global_fns.sort_by_key(|f| f.identifier.to_string());
        for func in global_fns {
            let Some(name) = self.callable_name(&func.identifier) else {
                continue;
            };
            if let Some(doc) = &func.documentation {
                push_doc(&mut out, doc, "");
            }
            let params = self.params(self.script_args(func));
            let ret = self.kind(&func.return_type.kind);
            out.push_str(&format!("declare function {name}({params}): {ret}\n\n"));
        }

        // Global instances (`world`, the static type accessors, …). Primitive-typed
        // globals carry no useful surface, so they are skipped — matching the LuaLS
        // backend.
        let mut globals: Vec<(String, String)> = Vec::new();
        for (key, inst) in &self.lad.globals {
            let Some(name) = self.callable_name(key) else {
                continue;
            };
            let ty = self.kind(&inst.type_kind);
            if ty == "any" && matches!(inst.type_kind, LadFieldOrVariableKind::Primitive(_)) {
                continue;
            }
            globals.push((name, ty));
        }
        globals.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, ty) in globals {
            out.push_str(&format!("declare {key}: {ty}\n"));
        }

        out
    }

    /// Render one associated function as a colon-method (if it has a receiver) or a
    /// dot-callable function field (if it is context-only). Skipped entirely if its
    /// name is a Luau keyword — there is no backing for a renamed binding.
    fn push_member(&self, out: &mut String, owner: &str, func: &LadFunction) {
        let Some(name) = self.callable_name(&func.identifier) else {
            log::warn!(
                "skipping `{}` on `{owner}`: name is a reserved Luau keyword",
                func.identifier
            );
            return;
        };
        if let Some(doc) = &func.documentation {
            push_doc(out, doc, "\t");
        }
        let (args, has_self) = self.split_receiver(func, owner);
        let ret = self.kind(&func.return_type.kind);
        if has_self {
            let params = self.params(args);
            let sep = if params.is_empty() { "" } else { ", " };
            out.push_str(&format!("\tfunction {name}(self{sep}{params}): {ret}\n"));
        } else {
            let params = self.params(args);
            out.push_str(&format!("\t{name}: ({params}) -> {ret}\n"));
        }
    }

    /// Split a method's receiver off its argument list. The owning-type argument
    /// (after any auto-injected `FunctionCallContext`) marks the function a method.
    fn split_receiver<'f>(&self, func: &'f LadFunction, owner: &str) -> (&'f [LadArgument], bool) {
        let args = self.script_args(func);
        if let Some(first) = args.first() {
            if self.kind(&first.kind) == owner {
                return (args.get(1..).unwrap_or(&[]), true);
            }
        }
        (args, false)
    }

    /// A function's script-visible arguments: BMS injects a leading auto-provided
    /// `FunctionCallContext` on many functions, which scripts never pass.
    fn script_args<'f>(&self, func: &'f LadFunction) -> &'f [LadArgument] {
        if let Some(first) = func.arguments.first() {
            if matches!(
                &first.kind,
                LadFieldOrVariableKind::Primitive(ReflectionPrimitiveKind::FunctionCallContext)
            ) {
                return func.arguments.get(1..).unwrap_or(&[]);
            }
        }
        func.arguments.as_slice()
    }

    fn params(&self, args: &[LadArgument]) -> String {
        args.iter()
            .enumerate()
            .map(|(i, a)| {
                // Parameter names are labels only (scripts pass positionally), so a
                // reserved name is safely replaced rather than escaped.
                let name = a
                    .name
                    .as_deref()
                    .map(sanitize)
                    .filter(|n| !n.is_empty() && !is_reserved(n))
                    .unwrap_or_else(|| format!("arg{i}"));
                format!("{name}: {}", self.kind(&a.kind))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// A name usable in a `declare`/method position, or `None` if it is a reserved
    /// Luau keyword (which cannot be represented without inventing an unbacked
    /// alias).
    fn callable_name(&self, raw: &str) -> Option<String> {
        let name = sanitize(raw);
        (!name.is_empty() && !is_reserved(&name)).then_some(name)
    }

    /// Resolve a LAD type kind to a Luau type expression.
    fn kind(&self, kind: &LadFieldOrVariableKind) -> String {
        match kind {
            LadFieldOrVariableKind::Ref(id)
            | LadFieldOrVariableKind::Mut(id)
            | LadFieldOrVariableKind::Val(id)
            | LadFieldOrVariableKind::Unknown(id) => self.type_name(id),
            LadFieldOrVariableKind::Option(inner) => format!("{}?", self.kind(inner)),
            LadFieldOrVariableKind::Vec(inner) | LadFieldOrVariableKind::Array(inner, _) => {
                format!("{{ {} }}", self.kind(inner))
            }
            LadFieldOrVariableKind::HashSet(inner) => format!("{{ {} }}", self.kind(inner)),
            LadFieldOrVariableKind::HashMap(k, v) => {
                format!("{{ [{}]: {} }}", self.kind(k), self.kind(v))
            }
            LadFieldOrVariableKind::InteropResult(inner) => self.kind(inner),
            LadFieldOrVariableKind::Tuple(items) => match items.first() {
                // The unit type. `nil` is valid in every position; a bare `()` is
                // only legal as a function return, so avoid it.
                None => "nil".to_string(),
                // Luau has no value-level tuple type; approximate as an array.
                Some(first) => format!("{{ {} }}", self.kind(first)),
            },
            LadFieldOrVariableKind::Primitive(p) => primitive(p).to_string(),
            LadFieldOrVariableKind::Union(items) => items
                .iter()
                .map(|i| self.kind(i))
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }

    /// Resolve a type id to a Luau type name: a builtin (for primitive-mapped
    /// types), the declared class name, or `any` for anything absent from the file.
    fn type_name(&self, id: &LadTypeId) -> String {
        if let Some(def) = self.lad.types.get(id) {
            if let Some(p) = &def.metadata.mapped_to_primitive_kind {
                return primitive(p).to_string();
            }
        }
        self.names
            .get(id)
            .cloned()
            .unwrap_or_else(|| "any".to_string())
    }
}

fn primitive(p: &ReflectionPrimitiveKind) -> &'static str {
    use ReflectionPrimitiveKind::*;
    match p {
        Bool => "boolean",
        Isize | I8 | I16 | I32 | I64 | I128 | Usize | U8 | U16 | U32 | U64 | U128 | F32 | F64 => {
            "number"
        }
        Char | Str | String | OsString | PathBuf => "string",
        _ => "any",
    }
}

/// Luau keywords (incl. the contextual `continue`) that can't name a binding.
const RESERVED: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in", "local",
    "nil", "not", "or", "repeat", "return", "then", "true", "until", "while", "continue",
];

fn is_reserved(s: &str) -> bool {
    RESERVED.contains(&s)
}

/// Make an arbitrary Rust identifier/path safe to use as a Luau identifier.
fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// A class field key: a bare identifier, or a quoted key when the name is a Luau
/// keyword. Quoting preserves the real field name (`["end"]: T`) and is backed by
/// reflect index access, unlike renaming.
fn field_key(s: &str) -> String {
    let n = sanitize(s);
    if is_reserved(&n) {
        format!("[\"{n}\"]")
    } else {
        n
    }
}

/// Emit a (possibly multi-line) doc comment with the given indent.
fn push_doc(out: &mut String, doc: &str, indent: &str) {
    for line in doc.lines() {
        out.push_str(indent);
        out.push_str("-- ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical example LAD file: every type is declared (not just a focused
    /// or component subset), fields are optional, the global function is present,
    /// and there is no opinionated branding or keyword fabrication.
    #[test]
    fn converts_example_ladfile() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let luau = lad_to_luau(&lad);

        // Plain and "container" types alike are declared — nothing is restricted to
        // components/resources, and nothing is hard-coded.
        assert!(luau.contains("declare class PlainStructType"), "{luau}");
        assert!(luau.contains("declare class EnumType"), "{luau}");
        assert!(luau.contains("declare class TupleStructType"), "{luau}");
        // Named fields are optional.
        assert!(luau.contains("int_field: number?"), "{luau}");
        // Global function + a non-static instance handle.
        assert!(
            luau.contains("declare function hello_world(arg1: number): number"),
            "{luau}"
        );
        assert!(luau.contains("declare my_non_static_instance:"), "{luau}");

        // No leftover opinionated machinery, and no fabricated keyword aliases.
        assert!(!luau.contains("Reg<"), "branding must be gone: {luau}");
        assert_no_keyword_fabrication(&luau);
    }

    /// Reserved Luau identifiers are handled honestly: keyword-named functions are
    /// skipped (a renamed binding would have nothing backing it), keyword-named
    /// fields are preserved via a quoted, reflect-backed key.
    #[test]
    fn handles_reserved_identifiers() {
        let src = r#"{
          "version": "0.19.0",
          "globals": {},
          "types": {
            "demo::Thing": {
              "identifier": "Thing",
              "crate": "demo",
              "path": "demo::Thing",
              "layout": { "kind": "Struct", "name": "Thing",
                "fields": [
                  { "name": "do", "type": { "primitive": "f64" } },
                  { "name": "ok", "type": { "primitive": "f64" } }
                ] },
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            }
          },
          "functions": {
            "demo::Thing::end": {
              "namespace": "demo::Thing", "identifier": "end", "overload_index": null,
              "arguments": [ { "kind": { "ref": "demo::Thing" }, "name": "self" } ],
              "return_type": { "kind": { "primitive": "f64" } },
              "metadata": { "is_operator": false }
            },
            "demo::Thing::value": {
              "namespace": "demo::Thing", "identifier": "value", "overload_index": null,
              "arguments": [ { "kind": { "ref": "demo::Thing" }, "name": "self" } ],
              "return_type": { "kind": { "primitive": "f64" } },
              "metadata": { "is_operator": false }
            }
          }
        }"#;
        let lad = ladfile::parse_lad_file(src).unwrap();
        let luau = lad_to_luau(&lad);

        // Keyword field preserved by quoting; non-keyword field bare.
        assert!(luau.contains("[\"do\"]: number?"), "{luau}");
        assert!(luau.contains("ok: number?"), "{luau}");
        // Keyword-named method skipped, normal method kept.
        assert!(luau.contains("function value(self): number"), "{luau}");
        assert!(
            !luau.contains("function end"),
            "keyword method must be skipped: {luau}"
        );
        // Crucially, no fabricated `end_` / `do_` aliases.
        assert_no_keyword_fabrication(&luau);
    }

    /// No reserved keyword is fabricated into a `keyword_` alias, and none is
    /// emitted as a bare binding name. Tokens are split on non-identifier characters
    /// so legitimate names like `plain_struct_function` (which contains `in_`) are
    /// not flagged, and keyword *syntax* (`function`, `end`, `nil`) is left alone.
    fn assert_no_keyword_fabrication(luau: &str) {
        for tok in luau.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            for kw in RESERVED {
                assert_ne!(
                    *tok,
                    format!("{kw}_"),
                    "fabricated keyword alias `{kw}_` in:\n{luau}"
                );
            }
        }
        // A reserved word can only appear as a *name* in a field/method/global
        // position; the class terminator `end`, `function`, and `nil` are syntax.
        for line in luau.lines() {
            let t = line.trim_start();
            for kw in RESERVED {
                assert!(
                    !t.starts_with(&format!("{kw}:"))
                        && !t.starts_with(&format!("function {kw}("))
                        && !t.starts_with(&format!("declare {kw}:")),
                    "reserved keyword `{kw}` used as a bare binding name: {line}"
                );
            }
        }
    }

    #[test]
    fn sanitizes_and_escapes() {
        assert_eq!(sanitize("Handle<Image>"), "Handle_Image_");
        assert_eq!(field_key("end"), "[\"end\"]");
        assert_eq!(field_key("current"), "current");
        assert!(is_reserved("continue"));
    }
}
