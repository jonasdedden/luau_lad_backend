//! Native **Luau** (`.d.luau`) definition-file backend for `bevy_mod_scripting`
//! LAD ([Language Agnostic Definition]) files.
//!
//! `bevy_mod_scripting` (BMS) ships LAD post-processors for the Lua Language
//! Server (`--- @class` `.lua`) and for mdbook, but no native Luau one — and
//! [`luau-lsp`] cannot consume the LuaLS dialect. This crate is that missing
//! backend: it turns a [`LadFile`] into `declare extern type … end` / `declare name: T`
//! definitions that `luau-lsp analyze --defs=…` checks scripts against.
//!
//! Every type in the LAD file is declared dynamically from the registry — no type
//! is hard-coded by name. The public entry point [`lad_to_luau`] is a pure
//! `&LadFile -> String` conversion, and [`LuauLadPlugin`] is the [`LadFilePlugin`]
//! processor that writes the file into a generation output directory.
//!
//! The reflection API BMS exposes to scripts is *dynamic*: functions like
//! `world.get_component` traffic in `ReflectReference` proxies whose concrete type
//! depends on the *registration value* passed at runtime, which the LAD file
//! records as an untyped primitive. To recover static types across that boundary,
//! the backend emits **phantom-typed registration brands**:
//!
//! ```luau
//! export type Reg<T> = ScriptComponentRegistration & { __component: T }
//! ```
//!
//! and rewrites every function that pairs a registration argument with a
//! `ReflectReference` into a generic signature
//! (`get_component: <T>(entity: Entity, registration: Reg<T>) -> T?`). The `types`
//! global is emitted as a closed table (`types: { Velocity: Reg<Velocity>, … }`),
//! so `world.get_component(e, types.Velocity)` infers `Velocity?` with no casts,
//! and wrong-component writes are compile errors. Script-registered dynamic
//! components join the scheme with a single cast:
//! `world.register_new_component("X") :: Reg<XShape>`. The brand's phantom field
//! never exists at runtime; it only carries `T` for the checker. The rewrite is
//! shape-driven (registration argument + `ReflectReference` positions), not
//! name-driven, so it applies to future BMS functions of the same shape
//! automatically. Whatever remains genuinely dynamic (`ScriptValue` payloads,
//! `DynamicFunction` callbacks) is typed `any`, which Luau treats permissively,
//! so such scripts still type-check.
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

/// Render a parsed LAD file as Luau definition-file source, with default
/// [`Options`].
///
/// This is the whole backend as a pure function; [`LuauLadPlugin`] just calls it
/// and writes the result to disk.
pub fn lad_to_luau(lad: &LadFile) -> String {
    lad_to_luau_with(lad, &Options::default())
}

/// [`lad_to_luau`], with control over the optional parts of the output.
pub fn lad_to_luau_with(lad: &LadFile, options: &Options) -> String {
    Converter::new(lad, options).render()
}

/// Knobs for the parts of the output that are a trade-off rather than a
/// straightforward rendering of the registry.
#[derive(Clone, Debug)]
pub struct Options {
    /// Emit `export type <Class>Init = { … }` payload aliases, so `construct`
    /// payload literals can be type-checked (`construct` itself takes an
    /// untyped `{ [string]: any }`; Luau cannot derive the payload type from
    /// the registration's `T`). Aliases are types only — nothing is claimed to
    /// exist at runtime.
    ///
    /// On by default. Worth turning off for a codebase that rarely calls
    /// `construct`: these are the one part of the output with a cost worth
    /// weighing — on a full Bevy registry, a tenth more file for roughly a
    /// third more `luau-lsp analyze` time, paid on every check.
    pub init_types: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { init_types: true }
    }
}

/// The `luau-lsp` type-inference budgets a full registry's worth of definitions
/// needs, as `(flag, value)` pairs.
///
/// A real Bevy app produces hundreds of classes plus a closed `types` table over
/// all of them, which overruns luau's defaults ("Code is too complex to
/// typecheck"). Every consumer needs these in both CI and their editor config,
/// so the backend states them rather than leaving each project to rediscover
/// them: every generated file names them in its header comment, and
/// [`LuauLadPlugin::fflags_filename`] writes them to a companion file for
/// tooling to read.
pub const FFLAGS: [(&str, &str); 5] = [
    ("LuauTypeInferIterationLimit", "10000000"),
    ("LuauTarjanChildLimit", "1000000"),
    ("LuauTypeCloneIterationLimit", "10000000"),
    ("LuauSolverConstraintLimit", "100000"),
    ("LuauTypeInferRecursionLimit", "2000"),
];

/// The [`FFLAGS`] as `luau-lsp analyze` arguments, one per line.
pub fn fflags_args() -> String {
    FFLAGS
        .iter()
        .map(|(flag, value)| format!("--flag:{flag}={value}\n"))
        .collect()
}

/// A [`LadFilePlugin`] post-processor that writes a native Luau `.d.luau` file.
///
/// Add it to BMS's `ScriptingFilesGenerationPlugin` processor list; on generation
/// it writes [`filename`](Self::filename) into the configured output directory.
#[derive(Clone, Debug)]
pub struct LuauLadPlugin {
    /// File name to write inside the generation output directory.
    pub filename: PathBuf,
    /// When set, also write the [`FFLAGS`] as `luau-lsp analyze` arguments to
    /// this file (one per line) inside the output directory, so build scripts
    /// and editor configs can read them from one place instead of each
    /// hard-coding the list.
    pub fflags_filename: Option<PathBuf>,
    /// Rendering options, see [`Options`].
    pub options: Options,
}

impl Default for LuauLadPlugin {
    fn default() -> Self {
        Self {
            filename: PathBuf::from("bindings.d.luau"),
            fflags_filename: None,
            options: Options::default(),
        }
    }
}

impl LadFilePlugin for LuauLadPlugin {
    fn name(&self) -> &'static str {
        "Luau definition file generator"
    }

    fn run(&self, ladfile: &LadFile, output_dir: &Path) -> Result<(), Box<dyn Error>> {
        std::fs::write(
            output_dir.join(&self.filename),
            lad_to_luau_with(ladfile, &self.options),
        )?;
        if let Some(fflags) = &self.fflags_filename {
            std::fs::write(output_dir.join(fflags), fflags_args())?;
        }
        Ok(())
    }
}

/// The registration brands: which registration class each one wraps, the alias
/// name it is declared under, and the phantom field carrying `T`.
const BRANDS: [(&str, &str, &str); 3] = [
    ("ScriptComponentRegistration", "Reg", "__component"),
    ("ScriptResourceRegistration", "ResReg", "__resource"),
    ("ScriptTypeRegistration", "TypeReg", "__type"),
];

/// The name the shared reference-proxy base class is declared under. Reserved
/// in the class namespace whenever the LAD file records the type.
const REFLECT_REFERENCE_CLASS: &str = "ReflectReference";

struct Converter<'a> {
    lad: &'a LadFile,
    /// type id -> the Luau identifier we declare it under (sanitised + unique).
    /// Contains every non-primitive type in the LAD file, so type references
    /// resolve dynamically with no hard-coded names.
    names: HashMap<LadTypeId, String>,
    /// The BMS registration types (component/resource/type, in [`BRANDS`] order),
    /// when present in the LAD file: the anchors for the phantom-typed brands.
    regs: [Option<LadTypeId>; 3],
    /// The `ReflectReference` proxy type, when recorded in the LAD file. Every
    /// script-visible value of a declared class is such a proxy at runtime, so
    /// it is declared as a base class (carrying its namespaced methods) that
    /// every other class `extends`. In *signatures* the reference primitive
    /// stays `any`/`T` — the class exists only to grant the shared methods.
    reflect_ref: Option<LadTypeId>,
    /// enum type id -> name of its exported variant-name union alias
    /// (`export type <Class>Variant = "A" | "B"`), for enums with >= 1 variant.
    variant_aliases: HashMap<LadTypeId, String>,
    /// type id -> name of its exported `construct` payload alias
    /// (`export type <Class>Init = { … }`), for types with something to fill in.
    init_aliases: HashMap<LadTypeId, String>,
    /// Whether the registry records `ReflectReference`'s `variant_name`
    /// function (a reference in, a string out). Only then do enum classes get
    /// the typed `function variant_name(self): <union>` override — the
    /// override must be backed by something real, like everything else.
    has_variant_name: bool,
}

impl<'a> Converter<'a> {
    fn new(lad: &'a LadFile, options: &Options) -> Self {
        // Assign a stable, unique Luau name to every non-primitive type. Iterating
        // a sorted key list keeps both the names and the output deterministic.
        let mut ids: Vec<&LadTypeId> = lad
            .types
            .iter()
            .filter(|(_, def)| def.metadata.mapped_to_primitive_kind.is_none())
            .map(|(id, _)| id)
            .collect();
        ids.sort_by(|a, b| lad.types[*a].identifier.cmp(&lad.types[*b].identifier));

        // Locate the ReflectReference proxy type (recorded as a primitive-mapped
        // type), the anchor for the shared base class.
        let mut reflect_candidates: Vec<&LadTypeId> = lad
            .types
            .iter()
            .filter(|(_, def)| {
                matches!(
                    def.metadata.mapped_to_primitive_kind,
                    Some(ReflectionPrimitiveKind::ReflectReference)
                )
            })
            .map(|(id, _)| id)
            .collect();
        reflect_candidates.sort_by_key(|id| &lad.types[*id].path);
        let reflect_ref = reflect_candidates.first().map(|id| (*id).clone());

        let mut names = HashMap::new();
        // The brand aliases (and the base class, when present) live in the same
        // namespace as class names.
        let mut used: HashSet<String> = BRANDS
            .iter()
            .map(|(_, alias, _)| alias.to_string())
            .collect();
        if reflect_ref.is_some() {
            used.insert(REFLECT_REFERENCE_CLASS.to_string());
        }
        for id in &ids {
            // Type identifiers are CamelCase Rust paths, so they never collide with
            // (lowercase) Luau keywords; sanitising is enough, no escaping needed.
            let mut name = sanitize(&lad.types[*id].identifier);
            if name.is_empty() {
                name = "Unknown".to_string();
            }
            while !used.insert(name.clone()) {
                name.push('_');
            }
            names.insert((*id).clone(), name);
        }

        // Allocate the variant-name union alias of every (non-empty) enum, in
        // the same deterministic order as the class names.
        let mut variant_aliases = HashMap::new();
        for id in &ids {
            if matches!(&lad.types[*id].layout, LadTypeLayout::Enum(variants) if !variants.is_empty())
            {
                let mut alias = format!("{}Variant", names[*id]);
                while !used.insert(alias.clone()) {
                    alias.push('_');
                }
                variant_aliases.insert((*id).clone(), alias);
            }
        }

        // Allocate the `construct` payload alias of every type that has
        // something to put in a payload — named struct fields, or enum variants
        // (whose payload is the `variant` key). Types with neither would get an
        // empty alias that says nothing, so they are skipped.
        let mut init_aliases = HashMap::new();
        if options.init_types {
            for id in &ids {
                let worth_it = match &lad.types[*id].layout {
                    LadTypeLayout::MonoVariant(LadVariant::Struct { fields, .. }) => {
                        !fields.is_empty()
                    }
                    LadTypeLayout::Enum(variants) => !variants.is_empty(),
                    _ => false,
                };
                if !worth_it {
                    continue;
                }
                let mut alias = format!("{}Init", names[*id]);
                while !used.insert(alias.clone()) {
                    alias.push('_');
                }
                init_aliases.insert((*id).clone(), alias);
            }
        }

        // The typed variant_name override on enum classes is only emitted when
        // the registry actually records the backing function: a
        // ReflectReference-namespaced `variant_name` taking a reference and
        // returning a string.
        let has_variant_name = reflect_ref.as_ref().is_some_and(|reference_id| {
            lad.functions.values().any(|func| {
                matches!(&func.namespace, LadFunctionNamespace::Type(t) if t == reference_id)
                    && func.identifier == "variant_name"
                    && func.arguments.iter().any(|a| {
                        matches!(
                            a.kind,
                            LadFieldOrVariableKind::Primitive(
                                ReflectionPrimitiveKind::ReflectReference
                            )
                        )
                    })
                    && contains_string_primitive(&func.return_type.kind)
            })
        });

        // Locate the registration classes. Matching prefers the BMS-owned type if
        // several crates happen to use the same identifier.
        let regs = BRANDS.map(|(identifier, _, _)| {
            let mut candidates: Vec<&LadTypeId> = lad
                .types
                .iter()
                .filter(|(_, def)| {
                    def.identifier == identifier && def.metadata.mapped_to_primitive_kind.is_none()
                })
                .map(|(id, _)| id)
                .collect();
            candidates.sort_by_key(|id| &lad.types[*id].path);
            candidates
                .iter()
                .find(|id| lad.types[**id].path.starts_with("bevy_mod_scripting"))
                .or(candidates.first())
                .map(|id| (*id).clone())
        });

        Converter {
            lad,
            names,
            regs,
            reflect_ref,
            variant_aliases,
            init_aliases,
            has_variant_name,
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("--!strict\n");
        out.push_str("-- AUTO-GENERATED from the Bevy reflection registry via luau_lad_backend.\n");
        out.push_str("-- Do not edit by hand.\n");
        // Unconditional: "Code is too complex to typecheck" is the first thing a
        // consumer hits, and a comment costs nothing to check past.
        out.push_str(
            "--\n-- A registry this size overruns luau's default type-inference budgets.\n\
             -- Analyze with:\n",
        );
        for (flag, value) in FFLAGS {
            out.push_str(&format!("--     --flag:{flag}={value}\n"));
        }
        out.push_str("-- (or the equivalent `luau-lsp.fflags.override` editor setting).\n");
        out.push('\n');

        // Phantom-typed registration brands. The phantom field never exists at
        // runtime; it only carries `T` through generic signatures. Forward
        // references are legal in definition files, so these can lead the file.
        let mut any_brand = false;
        for (i, (_, alias, field)) in BRANDS.iter().enumerate() {
            if let Some(id) = &self.regs[i] {
                out.push_str(&format!(
                    "export type {alias}<T> = {} & {{ {field}: T }}\n",
                    self.type_name(id)
                ));
                any_brand = true;
            }
        }
        if any_brand {
            out.push('\n');
        }

        // Group functions by namespace once.
        let mut methods: HashMap<&LadTypeId, Vec<&LadFunction>> = HashMap::new();
        let mut global_fns: Vec<&LadFunction> = Vec::new();
        for func in self.lad.functions.values() {
            match &func.namespace {
                LadFunctionNamespace::Type(id) => methods.entry(id).or_default().push(func),
                LadFunctionNamespace::Global => global_fns.push(func),
            }
        }

        // The shared reference-proxy base class. At runtime every value of a
        // declared class is a `ReflectReference` proxy which resolves fields
        // first and falls back to these namespaced functions, so declaring
        // them once here and having every class extend it mirrors the actual
        // dispatch (subclass members legitimately shadow these).
        if let Some(reference_id) = &self.reflect_ref {
            out.push_str(&format!(
                "declare extern type {REFLECT_REFERENCE_CLASS} with\n"
            ));
            let mut fns = methods.remove(reference_id).unwrap_or_default();
            fns.sort_by_key(|f| f.identifier.to_string());
            for func in fns {
                self.push_member(&mut out, REFLECT_REFERENCE_CLASS, func, true);
            }
            out.push_str("end\n\n");
        }
        let extends = if self.reflect_ref.is_some() {
            format!(" extends {REFLECT_REFERENCE_CLASS}")
        } else {
            String::new()
        };

        // Every non-primitive type gets a class declaration, in name order.
        let mut typed: Vec<(&LadTypeId, &str)> =
            self.names.iter().map(|(id, n)| (id, n.as_str())).collect();
        typed.sort_by_key(|(_, n)| *n);
        for (id, name) in typed {
            let Some(def) = self.lad.types.get(id) else {
                continue;
            };

            // The variant-name union of an enum, exported next to its class.
            if let (Some(alias), LadTypeLayout::Enum(variants)) =
                (self.variant_aliases.get(id), &def.layout)
            {
                let union = variants
                    .iter()
                    .map(|variant| format!("\"{}\"", lad_variant_name(variant)))
                    .collect::<Vec<_>>()
                    .join(" | ");
                out.push_str(&format!("export type {alias} = {union}\n"));
            }

            // The `construct` payload alias. Every entry is optional: BMS builds
            // the value from `Default` and applies the payload over it, so a
            // partial payload is legal — the alias buys key spelling and value
            // types, which is what actually goes wrong.
            if let Some(alias) = self.init_aliases.get(id) {
                match &def.layout {
                    LadTypeLayout::MonoVariant(LadVariant::Struct { fields, .. }) => {
                        out.push_str(&format!("export type {alias} = {{\n"));
                        for f in dedupe_field_keys(fields.iter().map(|f| (&f.name, &f.type_)), name)
                        {
                            out.push_str(&format!("\t{}: {}?,\n", f.0, self.kind(f.1)));
                        }
                        out.push_str("}\n");
                    }
                    // A unit/enum value crosses the boundary as its variant name.
                    LadTypeLayout::Enum(_) => {
                        if let Some(variant) = self.variant_aliases.get(id) {
                            out.push_str(&format!(
                                "export type {alias} = {{ variant: {variant} }}\n"
                            ));
                        }
                    }
                    _ => {}
                }
            }

            if let Some(doc) = &def.documentation {
                push_doc(&mut out, doc, "");
            }
            out.push_str(&format!("declare extern type {name}{extends} with\n"));

            // Associated functions: a method if its first script-visible argument is
            // the owning type, otherwise a dot-callable function field.
            let fns = self.dedupe_overloads(methods.remove(id).unwrap_or_default());

            match &def.layout {
                // Named struct fields of a plain struct. These are always present
                // on a live reference — a struct has no inactive variant to hide
                // them — so they are *not* optional; only enum variant fields
                // below earn that. Keyword names are preserved via a quoted key
                // (`["end"]`), which is genuinely backed by reflect indexing.
                LadTypeLayout::MonoVariant(LadVariant::Struct { fields, .. }) => {
                    for f in dedupe_field_keys(fields.iter().map(|f| (&f.name, &f.type_)), name) {
                        out.push_str(&format!("\t{}: {}\n", f.0, self.kind(f.1)));
                    }
                }
                // Enums: named fields of all struct variants, merged (a field may
                // belong to an inactive variant, so optionality is doubly earned;
                // same-named fields of different types union). Tuple variants have
                // no honest member representation yet and are omitted, matching
                // the tuple-struct fallback.
                LadTypeLayout::Enum(variants) => {
                    let mut merged: Vec<(String, Vec<String>)> = Vec::new();
                    for variant in variants {
                        if let LadVariant::Struct { fields, .. } = variant {
                            for f in fields {
                                let key = field_key(&f.name);
                                let ty = self.kind(&f.type_);
                                match merged.iter_mut().find(|(k, _)| *k == key) {
                                    Some((_, types)) => {
                                        if !types.contains(&ty) {
                                            types.push(ty);
                                        }
                                    }
                                    None => merged.push((key, vec![ty])),
                                }
                            }
                        }
                    }
                    for (key, types) in &merged {
                        if let [ty] = types.as_slice() {
                            out.push_str(&format!("\t{key}: {ty}?\n"));
                        } else {
                            out.push_str(&format!("\t{key}: ({})?\n", types.join(" | ")));
                        }
                    }

                    // Typed variant inspection, narrowing the base class's
                    // `variant_name` — but only when the backing function is in
                    // the registry, and never shadow-fighting a real member of
                    // the same name (fields win at runtime, so they win here).
                    let shadowed = merged.iter().any(|(key, _)| key == "variant_name")
                        || fns.iter().any(|f| f.identifier == "variant_name");
                    if self.has_variant_name && !shadowed {
                        if let Some(alias) = self.variant_aliases.get(id) {
                            out.push_str(&format!("\tfunction variant_name(self): {alias}\n"));
                        }
                    }
                }
                _ => {}
            }

            for func in fns {
                self.push_member(&mut out, name, func, false);
            }
            out.push_str("end\n\n");
        }

        // Global host functions.
        for func in self.dedupe_overloads(global_fns) {
            let Some(name) = self.callable_name(&func.identifier) else {
                continue;
            };
            if let Some(doc) = &func.documentation {
                push_doc(&mut out, doc, "");
            }
            let args = self.script_args(func);
            self.push_signature_docs(&mut out, "", args, &func.return_type);
            let generic = self.genericize(args, &func.return_type.kind);
            let params = self.params(args, generic.as_ref());
            let ret = self.kind_g(&func.return_type.kind, generic.is_some());
            let t = if generic.is_some() { "<T>" } else { "" };
            out.push_str(&format!("declare function {name}{t}({params}): {ret}\n\n"));
        }

        // Global instances (`world`, the static type accessors, …). Primitive-typed
        // globals carry no useful surface, so they are skipped — matching the LuaLS
        // backend.
        let mut globals: Vec<(String, String)> = Vec::new();
        for (key, inst) in &self.lad.globals {
            let Some(name) = self.callable_name(key) else {
                continue;
            };
            let ty = match self.registration_map(&inst.type_kind) {
                Some(table) => table,
                None => self.kind(&inst.type_kind),
            };
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
    /// `reflect_receiver` marks the base-class case, where the receiver argument is
    /// the `ReflectReference` primitive rather than a declared type.
    fn push_member(
        &self,
        out: &mut String,
        owner: &str,
        func: &LadFunction,
        reflect_receiver: bool,
    ) {
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
        let (args, has_self) = self.split_receiver(func, owner, reflect_receiver);
        // The receiver's own doc is dropped along with the argument: it renders
        // as `self`, and BMS documents it as `_self`, which says nothing here.
        self.push_signature_docs(out, "\t", args, &func.return_type);
        if has_self {
            let params = self.params(args, None);
            let ret = self.kind(&func.return_type.kind);
            let sep = if params.is_empty() { "" } else { ", " };
            out.push_str(&format!("\tfunction {name}(self{sep}{params}): {ret}\n"));
        } else {
            let generic = self.genericize(args, &func.return_type.kind);
            let params = self.params(args, generic.as_ref());
            let ret = self.kind_g(&func.return_type.kind, generic.is_some());
            let t = if generic.is_some() { "<T>" } else { "" };
            out.push_str(&format!("\t{name}: {t}({params}) -> {ret}\n"));
        }
    }

    /// Decide whether a function gets the phantom-generic treatment: exactly one
    /// registration-typed argument, and every `ReflectReference` in the signature
    /// sits in the return type or the final argument (the value slot of
    /// `insert_component`-shaped functions). References elsewhere (e.g. the asset
    /// handle of `get_asset`, which is a `Handle<T>`, not a `T`) would bind `T`
    /// wrongly, so such functions keep their honest `any`s. Returns the
    /// registration argument's index and its branded type.
    fn genericize(
        &self,
        args: &[LadArgument],
        ret: &LadFieldOrVariableKind,
    ) -> Option<(usize, String)> {
        let mut reg = None;
        for (i, a) in args.iter().enumerate() {
            if let Some(brand) = self.registration_brand(&a.kind) {
                if reg.is_some() {
                    return None;
                }
                reg = Some((i, brand));
            }
        }
        let (idx, brand) = reg?;
        let mut has_ref = contains_reflect_reference(ret);
        for (i, a) in args.iter().enumerate() {
            if contains_reflect_reference(&a.kind) {
                if i + 1 != args.len() {
                    return None;
                }
                has_ref = true;
            }
        }
        has_ref.then_some((idx, brand))
    }

    /// If `kind` is a registration reference — or a union made up solely of them,
    /// like `construct`'s — the branded generic type to use in its place, e.g.
    /// `Reg<T>` or `TypeReg<T> | Reg<T> | ResReg<T>`.
    fn registration_brand(&self, kind: &LadFieldOrVariableKind) -> Option<String> {
        match kind {
            LadFieldOrVariableKind::Ref(id)
            | LadFieldOrVariableKind::Mut(id)
            | LadFieldOrVariableKind::Val(id) => {
                let i = self.regs.iter().position(|r| r.as_ref() == Some(id))?;
                Some(format!("{}<T>", BRANDS[i].1))
            }
            LadFieldOrVariableKind::Union(items) => {
                let mut brands: Vec<String> = Vec::new();
                for item in items {
                    let brand = self.registration_brand(item)?;
                    if !brands.contains(&brand) {
                        brands.push(brand);
                    }
                }
                (!brands.is_empty()).then(|| brands.join(" | "))
            }
            _ => None,
        }
    }

    /// If `kind` is `HashMap<string, registration-union>` — the shape of BMS's
    /// `types` global — render it as a closed table typing each known type's entry
    /// with its phantom brand, with the original union as an indexer fallback.
    /// Runtime keys are Bevy short paths, which match the LAD identifier exactly
    /// for non-generic types; anything else (generic monomorphisations, sanitised
    /// or de-duplicated names) is left to the fallback.
    fn registration_map(&self, kind: &LadFieldOrVariableKind) -> Option<String> {
        let LadFieldOrVariableKind::HashMap(key, value) = kind else {
            return None;
        };
        if self.kind(key) != "string" || self.registration_brand(value).is_none() {
            return None;
        }

        let mut typed: Vec<(&LadTypeId, &str)> =
            self.names.iter().map(|(id, n)| (id, n.as_str())).collect();
        typed.sort_by_key(|(_, n)| *n);

        let mut out = String::from("{\n");
        for (id, name) in typed {
            let Some(def) = self.lad.types.get(id) else {
                continue;
            };
            if !def.generics.is_empty() || def.identifier != name {
                continue;
            }
            let brand_index = if def.metadata.is_component {
                0
            } else if def.metadata.is_resource {
                1
            } else {
                2
            };
            if self.regs[brand_index].is_none() {
                continue;
            }
            out.push_str(&format!("\t{name}: {}<{name}>,\n", BRANDS[brand_index].1));
        }
        out.push_str(&format!("\t[string]: {},\n}}", self.kind(value)));
        Some(out)
    }

    /// Split a method's receiver off its argument list. The owning-type argument
    /// (after any auto-injected `FunctionCallContext`) marks the function a method;
    /// for the reference base class the receiver is the reference primitive itself.
    fn split_receiver<'f>(
        &self,
        func: &'f LadFunction,
        owner: &str,
        reflect_receiver: bool,
    ) -> (&'f [LadArgument], bool) {
        let args = self.script_args(func);
        if let Some(first) = args.first() {
            let is_receiver = self.kind(&first.kind) == owner
                || (reflect_receiver
                    && matches!(
                        first.kind,
                        LadFieldOrVariableKind::Primitive(
                            ReflectionPrimitiveKind::ReflectReference
                        )
                    ));
            if is_receiver {
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

    /// Render a parameter list. When `generic` is set (see [`Self::genericize`]),
    /// the registration argument at the given index becomes the brand and any
    /// `ReflectReference` renders as `T`.
    fn params(&self, args: &[LadArgument], generic: Option<&(usize, String)>) -> String {
        args.iter()
            .enumerate()
            .map(|(i, a)| {
                let ty = match generic {
                    Some((idx, brand)) if *idx == i => brand.clone(),
                    _ => self.kind_g(&a.kind, generic.is_some()),
                };
                format!("{}: {ty}", arg_name(a, i))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Emit the per-argument and return documentation the LAD file carries
    /// alongside a function's main docstring.
    ///
    /// `ladfile_builder` parses the `Arguments:` / `Returns:` sections out of the
    /// Rust doc comment into structured entries and *truncates* the main
    /// docstring at that point, so a backend that renders only the main
    /// docstring silently drops everything the author wrote about the
    /// parameters. Names match [`Self::params`] exactly, so the comment and the
    /// signature always agree.
    fn push_signature_docs(
        &self,
        out: &mut String,
        indent: &str,
        args: &[LadArgument],
        ret: &LadArgument,
    ) {
        for (i, a) in args.iter().enumerate() {
            if let Some(doc) = doc_line(a.documentation.as_deref()) {
                out.push_str(&format!("{indent}-- @param {} - {doc}\n", arg_name(a, i)));
            }
        }
        if let Some(doc) = doc_line(ret.documentation.as_deref()) {
            match ret
                .name
                .as_deref()
                .map(sanitize)
                .filter(|n| !n.is_empty() && !is_reserved(n))
            {
                Some(name) => out.push_str(&format!("{indent}-- @return {name} - {doc}\n")),
                None => out.push_str(&format!("{indent}-- @return {doc}\n")),
            }
        }
    }

    /// Collapse overloads that render to the same signature.
    ///
    /// BMS registers some functions more than once — operator impls in
    /// particular arrive both with argument names and without — and every copy
    /// used to be emitted. `luau-lsp` merges duplicate members into an overload
    /// set, so the extras were never *wrong*, just repeated: they inflate a file
    /// that already strains luau's inference budgets and make "no overload
    /// matched" diagnostics list the same candidate several times. Genuinely
    /// distinct overloads (different argument or return *types*) are all kept.
    fn dedupe_overloads<'f>(&self, fns: Vec<&'f LadFunction>) -> Vec<&'f LadFunction> {
        let mut keyed: Vec<(String, &'f LadFunction)> = Vec::new();
        for func in fns {
            let key = self.signature_key(func);
            match keyed.iter_mut().find(|(k, _)| *k == key) {
                // Keep whichever copy tells the reader more.
                Some(slot) if self.doc_score(func) > self.doc_score(slot.1) => slot.1 = func,
                Some(_) => {}
                None => keyed.push((key, func)),
            }
        }
        keyed.sort_by(|a, b| a.1.identifier.cmp(&b.1.identifier).then(a.0.cmp(&b.0)));
        keyed.into_iter().map(|(_, func)| func).collect()
    }

    /// Identity of a function as it will be *rendered*: name plus argument and
    /// return types. Argument names and documentation are excluded on purpose —
    /// they are exactly what differs between the redundant copies.
    fn signature_key(&self, func: &LadFunction) -> String {
        let args = self
            .script_args(func)
            .iter()
            .map(|a| self.kind(&a.kind))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{}({args}) -> {}",
            func.identifier,
            self.kind(&func.return_type.kind)
        )
    }

    /// How much a candidate copy of an overload tells the reader. Ties keep the
    /// first one seen, which is the LAD file's own order.
    fn doc_score(&self, func: &LadFunction) -> usize {
        let args = self.script_args(func);
        args.iter().filter(|a| a.name.is_some()).count()
            + args.iter().filter(|a| a.documentation.is_some()).count()
            + usize::from(func.documentation.is_some())
            + usize::from(func.return_type.documentation.is_some())
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
        self.kind_g(kind, false)
    }

    /// [`Self::kind`], rendering `ReflectReference` as the generic `T` instead of
    /// `any` when inside a genericized signature.
    fn kind_g(&self, kind: &LadFieldOrVariableKind, ref_as_t: bool) -> String {
        match kind {
            LadFieldOrVariableKind::Ref(id)
            | LadFieldOrVariableKind::Mut(id)
            | LadFieldOrVariableKind::Val(id)
            | LadFieldOrVariableKind::Unknown(id) => self.type_name(id),
            LadFieldOrVariableKind::Option(inner) => format!("{}?", self.kind_g(inner, ref_as_t)),
            LadFieldOrVariableKind::Vec(inner) | LadFieldOrVariableKind::Array(inner, _) => {
                format!("{{ {} }}", self.kind_g(inner, ref_as_t))
            }
            LadFieldOrVariableKind::HashSet(inner) => {
                format!("{{ {} }}", self.kind_g(inner, ref_as_t))
            }
            LadFieldOrVariableKind::HashMap(k, v) => {
                format!(
                    "{{ [{}]: {} }}",
                    self.kind_g(k, ref_as_t),
                    self.kind_g(v, ref_as_t)
                )
            }
            LadFieldOrVariableKind::InteropResult(inner) => self.kind_g(inner, ref_as_t),
            // Luau has no value-level tuple type, so a tuple approximates to an
            // array. A heterogeneous tuple's element type is then the *union* of
            // its members — taking only the first member's type would state
            // something the value does not guarantee.
            LadFieldOrVariableKind::Tuple(items) => {
                let mut types: Vec<String> = Vec::new();
                for item in items {
                    let ty = self.kind_g(item, ref_as_t);
                    if !types.contains(&ty) {
                        types.push(ty);
                    }
                }
                match types.len() {
                    // The unit type. `nil` is valid in every position; a bare `()`
                    // is only legal as a function return, so avoid it.
                    0 => "nil".to_string(),
                    1 => format!("{{ {} }}", types[0]),
                    _ => format!("{{ {} }}", types.join(" | ")),
                }
            }
            LadFieldOrVariableKind::Primitive(ReflectionPrimitiveKind::ReflectReference)
                if ref_as_t =>
            {
                "T".to_string()
            }
            LadFieldOrVariableKind::Primitive(p) => primitive(p).to_string(),
            // Distinct Rust types can share a Luau type (`String` and `&str` are
            // both `string`), which would otherwise render as `string | string`.
            LadFieldOrVariableKind::Union(items) => {
                let mut types: Vec<String> = Vec::new();
                for item in items {
                    let ty = self.kind_g(item, ref_as_t);
                    if !types.contains(&ty) {
                        types.push(ty);
                    }
                }
                types.join(" | ")
            }
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

/// A parameter's rendered name. Names are labels only (scripts pass
/// positionally), so a reserved or missing one is safely replaced.
fn arg_name(arg: &LadArgument, index: usize) -> String {
    arg.name
        .as_deref()
        .map(sanitize)
        .filter(|n| !n.is_empty() && !is_reserved(n))
        .unwrap_or_else(|| format!("arg{index}"))
}

/// A doc string flattened onto one line, or `None` if it carries nothing.
/// Argument and return docs are single-sentence by convention, and a `--`
/// comment cannot span lines without repeating the prefix.
fn doc_line(doc: Option<&str>) -> Option<String> {
    let flat = doc?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!flat.is_empty()).then_some(flat)
}

/// Pair up field names with their types, dropping any whose Luau key collides
/// with an earlier one.
///
/// Class names are made unique by suffixing, but a field cannot be: the key is
/// what reflect indexing actually uses at runtime, so a renamed field would be
/// backed by nothing. Rust identifiers survive [`sanitize`] unchanged, so this
/// only bites types registered from outside Rust; dropping the later field and
/// saying so is the honest outcome.
fn dedupe_field_keys<'f, I, T>(fields: I, owner: &str) -> Vec<(String, &'f T)>
where
    I: IntoIterator<Item = (&'f String, &'f T)>,
    T: 'f,
{
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (name, type_) in fields {
        let key = field_key(name);
        if !seen.insert(key.clone()) {
            log::warn!("skipping field `{name}` on `{owner}`: key `{key}` is already taken");
            continue;
        }
        out.push((key, type_));
    }
    out
}

/// The declared name of a LAD variant, whatever its kind.
fn lad_variant_name(variant: &LadVariant) -> &str {
    match variant {
        LadVariant::Struct { name, .. }
        | LadVariant::TupleStruct { name, .. }
        | LadVariant::Unit { name } => name,
    }
}

/// Whether a string-like primitive occurs anywhere inside a kind.
fn contains_string_primitive(kind: &LadFieldOrVariableKind) -> bool {
    use LadFieldOrVariableKind::*;
    match kind {
        Primitive(p) => matches!(
            p,
            ReflectionPrimitiveKind::Str | ReflectionPrimitiveKind::String
        ),
        Ref(_) | Mut(_) | Val(_) | Unknown(_) => false,
        Option(inner) | Vec(inner) | Array(inner, _) | HashSet(inner) | InteropResult(inner) => {
            contains_string_primitive(inner)
        }
        HashMap(k, v) => contains_string_primitive(k) || contains_string_primitive(v),
        Tuple(items) | Union(items) => items.iter().any(contains_string_primitive),
    }
}

/// Whether a `ReflectReference` occurs anywhere inside a kind.
fn contains_reflect_reference(kind: &LadFieldOrVariableKind) -> bool {
    use LadFieldOrVariableKind::*;
    match kind {
        Primitive(p) => matches!(p, ReflectionPrimitiveKind::ReflectReference),
        Ref(_) | Mut(_) | Val(_) | Unknown(_) => false,
        Option(inner) | Vec(inner) | Array(inner, _) | HashSet(inner) | InteropResult(inner) => {
            contains_reflect_reference(inner)
        }
        HashMap(k, v) => contains_reflect_reference(k) || contains_reflect_reference(v),
        Tuple(items) | Union(items) => items.iter().any(contains_reflect_reference),
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
        // components/resources, and nothing is hard-coded. The example file records
        // the ReflectReference proxy type, so every class extends the base class.
        assert!(
            luau.contains("declare extern type PlainStructType extends ReflectReference with"),
            "{luau}"
        );
        assert!(
            luau.contains("declare extern type EnumType extends ReflectReference with"),
            "{luau}"
        );
        assert!(
            luau.contains("declare extern type TupleStructType extends ReflectReference with"),
            "{luau}"
        );
        assert!(luau.contains("declare extern type ReflectReference with\n"), "{luau}");
        // A plain struct's fields are always present on a live reference, so
        // they are *not* optional. (Asserted on the whole member line: the
        // construct-payload alias below carries the same name optionally.)
        assert!(luau.contains("\tint_field: number\n"), "{luau}");
        assert!(!luau.contains("\tint_field: number?\n"), "{luau}");
        // Enum variant names are exported as a union alias, and the struct
        // variant's named field *is* optional — it belongs to one variant only.
        assert!(
            luau.contains("export type EnumTypeVariant = \"Unit\" | \"Struct\" | \"TupleStruct\""),
            "{luau}"
        );
        assert!(luau.contains("\tfield: number?\n"), "{luau}");
        // The example file has no ReflectReference `variant_name` function, so no
        // typed override may be fabricated.
        assert!(!luau.contains("function variant_name"), "{luau}");
        // Global function + a non-static instance handle.
        assert!(
            luau.contains("declare function hello_world(arg1: number): number"),
            "{luau}"
        );
        assert!(luau.contains("declare my_non_static_instance:"), "{luau}");

        // No fabricated keyword aliases.
        assert_no_keyword_fabrication(&luau);
    }

    /// The per-argument and return documentation the LAD file carries is
    /// rendered, not dropped: `ladfile_builder` moves the `Arguments:` /
    /// `Returns:` sections out of the main docstring, so a backend that renders
    /// only the main docstring loses them entirely.
    #[test]
    fn renders_argument_and_return_docs() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let luau = lad_to_luau(&lad);

        assert!(
            luau.contains("-- @param ref_ - I am some docs for argument 1"),
            "{luau}"
        );
        assert!(
            luau.contains("-- @param tuple - I am some docs for argument 2"),
            "{luau}"
        );
        assert!(
            luau.contains("-- @return I am some docs for the return type"),
            "{luau}"
        );
        // The documented names match the rendered signature exactly.
        assert!(luau.contains("hello_world: (ref_: any, tuple:"), "{luau}");
    }

    /// A heterogeneous tuple's element type is the union of its members. Taking
    /// only the first member's type would state something the value does not
    /// guarantee — the one place the backend could quietly lie.
    #[test]
    fn renders_heterogeneous_tuples_as_a_union() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let luau = lad_to_luau(&lad);

        assert!(luau.contains("tuple: { number | string }"), "{luau}");
        // Unions of distinct Rust types that share a Luau type collapse.
        assert!(!luau.contains("string | string"), "{luau}");
    }

    /// `construct` takes an untyped `{ [string]: any }` payload, so the backend
    /// exports a per-type payload alias to check the literal against. Entries
    /// are optional (BMS applies the payload over `Default`), and types with
    /// nothing to fill in get no alias.
    #[test]
    fn exports_construct_payload_aliases() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let luau = lad_to_luau(&lad);

        assert!(
            luau.contains("export type PlainStructTypeInit = {\n\tint_field: number?,\n}"),
            "{luau}"
        );
        // An enum's payload is its variant name, typed by the variant union.
        assert!(
            luau.contains("export type EnumTypeInit = { variant: EnumTypeVariant }"),
            "{luau}"
        );
        // A field-less type would get an alias that says nothing.
        assert!(!luau.contains("UnitTypeInit"), "{luau}");

        // Opt out and they are gone, without disturbing the class declarations.
        let bare = lad_to_luau_with(&lad, &Options { init_types: false });
        assert!(!bare.contains("Init"), "{bare}");
        assert!(bare.contains("\tint_field: number\n"), "{bare}");
    }

    /// Every generated file names the FFlags it needs, and the same list is
    /// available programmatically.
    #[test]
    fn emits_fflag_header() {
        let lad = ladfile::parse_lad_file(ladfile::EXAMPLE_LADFILE).unwrap();
        let luau = lad_to_luau(&lad);
        for (flag, value) in FFLAGS {
            assert!(luau.contains(&format!("--flag:{flag}={value}")), "{luau}");
            assert!(fflags_args().contains(&format!("--flag:{flag}={value}")));
        }
    }

    /// Functions pairing a registration argument with a `ReflectReference` get the
    /// phantom-generic treatment; everything else keeps its honest signature.
    #[test]
    fn genericizes_registration_shaped_functions() {
        let src = r#"{
          "version": "0.19.0",
          "globals": {
            "types": { "is_static": false, "type_kind": { "hashMap": [
              { "primitive": "string" },
              { "union": [
                { "val": "bms::ScriptTypeRegistration" },
                { "union": [
                  { "val": "bms::ScriptComponentRegistration" },
                  { "val": "bms::ScriptResourceRegistration" }
                ] }
              ] }
            ] } },
            "world": { "is_static": false, "type_kind": { "val": "test::World" } }
          },
          "types": {
            "bevy_ecs::Entity": {
              "identifier": "Entity", "crate": "bevy_ecs", "path": "bevy_ecs::Entity",
              "layout": { "kind": "Struct", "name": "Entity", "fields": [] },
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "test::Health": {
              "identifier": "Health", "crate": "test", "path": "test::Health",
              "layout": { "kind": "Struct", "name": "Health",
                "fields": [ { "name": "current", "type": { "primitive": "f64" } } ] },
              "metadata": { "is_component": true, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "test::GameRules": {
              "identifier": "GameRules", "crate": "test", "path": "test::GameRules",
              "layout": { "kind": "Struct", "name": "GameRules", "fields": [] },
              "metadata": { "is_component": false, "is_resource": true,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "test::World": {
              "identifier": "World", "crate": "test", "path": "test::World",
              "layout": { "kind": "Struct", "name": "World", "fields": [] },
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "bms::ScriptComponentRegistration": {
              "identifier": "ScriptComponentRegistration", "crate": "bms",
              "path": "bms::ScriptComponentRegistration",
              "layout": { "kind": "Struct", "name": "ScriptComponentRegistration", "fields": [] },
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "bms::ScriptResourceRegistration": {
              "identifier": "ScriptResourceRegistration", "crate": "bms",
              "path": "bms::ScriptResourceRegistration",
              "layout": { "kind": "Struct", "name": "ScriptResourceRegistration", "fields": [] },
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "bms::ScriptTypeRegistration": {
              "identifier": "ScriptTypeRegistration", "crate": "bms",
              "path": "bms::ScriptTypeRegistration",
              "layout": { "kind": "Struct", "name": "ScriptTypeRegistration", "fields": [] },
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            }
          },
          "functions": {
            "test::World::get_component": {
              "namespace": "test::World", "identifier": "get_component", "overload_index": null,
              "arguments": [
                { "kind": { "primitive": "functionCallContext" }, "name": "ctxt" },
                { "kind": { "val": "bevy_ecs::Entity" }, "name": "entity" },
                { "kind": { "val": "bms::ScriptComponentRegistration" }, "name": "registration" }
              ],
              "return_type": { "kind": { "interopResult": { "option": { "primitive": "reflectReference" } } } },
              "metadata": { "is_operator": false }
            },
            "test::World::insert_component": {
              "namespace": "test::World", "identifier": "insert_component", "overload_index": null,
              "arguments": [
                { "kind": { "primitive": "functionCallContext" }, "name": "ctxt" },
                { "kind": { "val": "bevy_ecs::Entity" }, "name": "entity" },
                { "kind": { "val": "bms::ScriptComponentRegistration" }, "name": "registration" },
                { "kind": { "primitive": "reflectReference" }, "name": "value" }
              ],
              "return_type": { "kind": { "tuple": [] } },
              "metadata": { "is_operator": false }
            },
            "test::World::has_component": {
              "namespace": "test::World", "identifier": "has_component", "overload_index": null,
              "arguments": [
                { "kind": { "primitive": "functionCallContext" }, "name": "ctxt" },
                { "kind": { "val": "bevy_ecs::Entity" }, "name": "entity" },
                { "kind": { "val": "bms::ScriptComponentRegistration" }, "name": "registration" }
              ],
              "return_type": { "kind": { "primitive": "bool" } },
              "metadata": { "is_operator": false }
            },
            "test::World::get_asset": {
              "namespace": "test::World", "identifier": "get_asset", "overload_index": null,
              "arguments": [
                { "kind": { "primitive": "functionCallContext" }, "name": "ctxt" },
                { "kind": { "primitive": "reflectReference" }, "name": "handle" },
                { "kind": { "val": "bms::ScriptTypeRegistration" }, "name": "registration" }
              ],
              "return_type": { "kind": { "interopResult": { "option": { "primitive": "reflectReference" } } } },
              "metadata": { "is_operator": false }
            },
            "::construct": {
              "namespace": null, "identifier": "construct", "overload_index": null,
              "arguments": [
                { "kind": { "primitive": "functionCallContext" }, "name": "ctxt" },
                { "kind": { "union": [
                    { "val": "bms::ScriptTypeRegistration" },
                    { "union": [
                      { "val": "bms::ScriptComponentRegistration" },
                      { "val": "bms::ScriptResourceRegistration" }
                    ] }
                  ] }, "name": "registration" },
                { "kind": { "hashMap": [ { "primitive": "string" }, { "primitive": "scriptValue" } ] }, "name": "payload" }
              ],
              "return_type": { "kind": { "interopResult": { "primitive": "reflectReference" } } },
              "metadata": { "is_operator": false }
            }
          }
        }"#;
        let lad = ladfile::parse_lad_file(src).unwrap();
        let luau = lad_to_luau(&lad);

        // Brand aliases lead the file, anchored to the declared classes.
        for alias in [
            "export type Reg<T> = ScriptComponentRegistration & { __component: T }",
            "export type ResReg<T> = ScriptResourceRegistration & { __resource: T }",
            "export type TypeReg<T> = ScriptTypeRegistration & { __type: T }",
        ] {
            assert!(luau.contains(alias), "missing `{alias}`:\n{luau}");
        }

        // Registration + ReflectReference in return/value position => generic.
        assert!(
            luau.contains("\tget_component: <T>(entity: Entity, registration: Reg<T>) -> T?"),
            "{luau}"
        );
        assert!(
            luau.contains(
                "\tinsert_component: <T>(entity: Entity, registration: Reg<T>, value: T) -> nil"
            ),
            "{luau}"
        );
        // Union-of-registrations argument (construct) is genericized too.
        assert!(
            luau.contains(
                "declare function construct<T>(registration: TypeReg<T> | Reg<T> | ResReg<T>, \
                 payload: { [string]: any }): T"
            ),
            "{luau}"
        );

        // No ReflectReference => untouched (brands still flow in via subtyping).
        assert!(
            luau.contains(
                "\thas_component: (entity: Entity, registration: ScriptComponentRegistration) -> boolean"
            ),
            "{luau}"
        );
        // ReflectReference in a non-trailing argument (an asset *handle*, not a
        // value of the registered type) => honestly untyped, not wrongly generic.
        assert!(
            luau.contains(
                "\tget_asset: (handle: any, registration: ScriptTypeRegistration) -> any?"
            ),
            "{luau}"
        );

        // The `types` global becomes a closed table: known non-generic types get
        // branded entries per their metadata, everything else hits the fallback.
        assert!(luau.contains("\tHealth: Reg<Health>,"), "{luau}");
        assert!(luau.contains("\tGameRules: ResReg<GameRules>,"), "{luau}");
        assert!(luau.contains("\tEntity: TypeReg<Entity>,"), "{luau}");
        assert!(
            luau.contains(
                "\t[string]: ScriptTypeRegistration | ScriptComponentRegistration | ScriptResourceRegistration,"
            ),
            "{luau}"
        );
    }

    /// Enums get an exported variant-name union and — when the registry records
    /// the backing `ReflectReference::variant_name` function — a narrowed
    /// `variant_name` override; every class extends the materialized
    /// `ReflectReference` base class carrying the shared proxy methods.
    #[test]
    fn declares_enum_variants_and_reference_base_class() {
        let src = r#"{
          "version": "0.19.0",
          "globals": {},
          "types": {
            "bms::ReflectReference": {
              "identifier": "ReflectReference", "crate": "bms", "path": "bms::ReflectReference",
              "layout": null,
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": "reflectReference" }
            },
            "demo::Stance": {
              "identifier": "Stance", "crate": "demo", "path": "demo::Stance",
              "layout": [
                { "kind": "Unit", "name": "Idle" },
                { "kind": "Unit", "name": "Aggressive" }
              ],
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "demo::StanceVariant": {
              "identifier": "StanceVariant", "crate": "demo", "path": "demo::StanceVariant",
              "layout": { "kind": "Struct", "name": "StanceVariant", "fields": [] },
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "demo::Mixed": {
              "identifier": "Mixed", "crate": "demo", "path": "demo::Mixed",
              "layout": [
                { "kind": "Unit", "name": "Off" },
                { "kind": "Struct", "name": "On", "fields": [
                  { "name": "level", "type": { "primitive": "f64" } },
                  { "name": "end", "type": { "primitive": "f64" } }
                ] },
                { "kind": "Struct", "name": "Boosted", "fields": [
                  { "name": "level", "type": { "primitive": "str" } }
                ] },
                { "kind": "TupleStruct", "name": "Pulse", "fields": [
                  { "type": { "primitive": "f64" } }
                ] }
              ],
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            },
            "demo::Sneaky": {
              "identifier": "Sneaky", "crate": "demo", "path": "demo::Sneaky",
              "layout": [
                { "kind": "Unit", "name": "Plain" },
                { "kind": "Struct", "name": "Odd", "fields": [
                  { "name": "variant_name", "type": { "primitive": "f64" } }
                ] }
              ],
              "metadata": { "is_component": false, "is_resource": false,
                "is_reflect": true, "mapped_to_primitive_kind": null }
            }
          },
          "functions": {
            "bms::ReflectReference::variant_name": {
              "namespace": "bms::ReflectReference", "identifier": "variant_name",
              "overload_index": null,
              "arguments": [
                { "kind": { "primitive": "functionCallContext" }, "name": "ctxt" },
                { "kind": { "primitive": "reflectReference" }, "name": "reference" }
              ],
              "return_type": { "kind": { "interopResult": { "option": { "primitive": "string" } } } },
              "metadata": { "is_operator": false }
            },
            "bms::ReflectReference::display": {
              "namespace": "bms::ReflectReference", "identifier": "display",
              "overload_index": null,
              "arguments": [
                { "kind": { "primitive": "functionCallContext" }, "name": "ctxt" },
                { "kind": { "primitive": "reflectReference" }, "name": "reference" }
              ],
              "return_type": { "kind": { "primitive": "string" } },
              "metadata": { "is_operator": false }
            }
          }
        }"#;
        let lad = ladfile::parse_lad_file(src).unwrap();
        let luau = lad_to_luau(&lad);

        // The base class carries the proxy methods as colon-methods.
        assert!(luau.contains("declare extern type ReflectReference with\n"), "{luau}");
        assert!(
            luau.contains("\tfunction variant_name(self): string?"),
            "{luau}"
        );
        assert!(luau.contains("\tfunction display(self): string"), "{luau}");
        // Every declared class extends it.
        assert!(
            luau.contains("declare extern type Stance extends ReflectReference with"),
            "{luau}"
        );
        assert!(
            luau.contains("declare extern type Mixed extends ReflectReference with"),
            "{luau}"
        );

        // Variant unions: the alias name dodges the class that already claimed
        // `StanceVariant`, and the enum class narrows `variant_name` to it.
        assert!(
            luau.contains("export type StanceVariant_ = \"Idle\" | \"Aggressive\""),
            "{luau}"
        );
        assert!(
            luau.contains("\tfunction variant_name(self): StanceVariant_"),
            "{luau}"
        );
        assert!(
            luau.contains("export type MixedVariant = \"Off\" | \"On\" | \"Boosted\" | \"Pulse\""),
            "{luau}"
        );

        // Struct-variant fields merge: same name with different types unions,
        // keyword names stay quoted, tuple variants contribute nothing.
        assert!(luau.contains("\tlevel: (number | string)?"), "{luau}");
        assert!(luau.contains("\t[\"end\"]: number?"), "{luau}");
        assert!(!luau.contains("Pulse:"), "{luau}");

        // A real field named `variant_name` wins over the override (fields are
        // resolved first at runtime), while the union alias is still exported.
        assert!(luau.contains("\tvariant_name: number?"), "{luau}");
        assert!(
            !luau.contains("function variant_name(self): SneakyVariant"),
            "{luau}"
        );
        assert!(
            luau.contains("export type SneakyVariant = \"Plain\" | \"Odd\""),
            "{luau}"
        );

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

    /// A LAD file where `mul` is registered three times: twice with identical
    /// argument and return *types* (once with names, once without — the shape
    /// BMS produces for operator impls) and once genuinely different.
    const OVERLOADS_LADFILE: &str = r#"{
      "version": "0.19.0",
      "globals": {},
      "types": {
        "demo::Marker": {
          "identifier": "Marker", "crate": "demo", "path": "demo::Marker",
          "layout": { "kind": "Struct", "name": "Marker", "fields": [
            { "name": "weight", "type": { "primitive": "f64" } } ] },
          "metadata": { "is_component": true, "is_resource": false,
            "is_reflect": true, "mapped_to_primitive_kind": null }
        },
        "demo::Scale": {
          "identifier": "Scale", "crate": "demo", "path": "demo::Scale",
          "layout": { "kind": "Struct", "name": "Scale", "fields": [] },
          "metadata": { "is_component": false, "is_resource": false,
            "is_reflect": true, "mapped_to_primitive_kind": null }
        }
      },
      "functions": {
        "demo::Marker::mul": {
          "namespace": "demo::Marker", "identifier": "mul", "overload_index": null,
          "arguments": [
            { "kind": { "ref": "demo::Marker" }, "name": "self" },
            { "kind": { "ref": "demo::Marker" }, "name": "rhs",
              "documentation": "The other marker." } ],
          "return_type": { "kind": { "ref": "demo::Marker" } },
          "metadata": { "is_operator": false }
        },
        "demo::Marker::mul#1": {
          "namespace": "demo::Marker", "identifier": "mul", "overload_index": 1,
          "arguments": [
            { "kind": { "ref": "demo::Marker" } },
            { "kind": { "ref": "demo::Marker" } } ],
          "return_type": { "kind": { "ref": "demo::Marker" } },
          "metadata": { "is_operator": false }
        },
        "demo::Marker::mul#2": {
          "namespace": "demo::Marker", "identifier": "mul", "overload_index": 2,
          "arguments": [
            { "kind": { "ref": "demo::Marker" }, "name": "self" },
            { "kind": { "ref": "demo::Scale" }, "name": "rhs" } ],
          "return_type": { "kind": { "ref": "demo::Scale" } },
          "metadata": { "is_operator": false }
        }
      }
    }"#;

    /// Overloads that render to the same signature collapse to one — keeping
    /// the better-documented copy — while genuinely distinct ones survive.
    #[test]
    fn dedupes_identical_overloads() {
        let lad = ladfile::parse_lad_file(OVERLOADS_LADFILE).unwrap();
        let luau = lad_to_luau(&lad);

        assert_eq!(
            luau.matches("function mul(").count(),
            2,
            "expected exactly the two distinct `mul` overloads:\n{luau}"
        );
        // The surviving copy is the one carrying names and docs.
        assert!(
            luau.contains("function mul(self, rhs: Marker): Marker"),
            "{luau}"
        );
        assert!(luau.contains("-- @param rhs - The other marker."), "{luau}");
        // The genuinely different overload is untouched.
        assert!(
            luau.contains("function mul(self, rhs: Scale): Scale"),
            "{luau}"
        );
    }

    #[test]
    fn sanitizes_and_escapes() {
        assert_eq!(sanitize("Handle<Image>"), "Handle_Image_");
        assert_eq!(field_key("end"), "[\"end\"]");
        assert_eq!(field_key("current"), "current");
        assert!(is_reserved("continue"));
    }
}
