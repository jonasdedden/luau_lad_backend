//! Dev helper: generate the LAD from the headless app, convert it with
//! `bevy_mod_scripting_luau`, and write both to a directory for inspection.
//! Usage: `cargo run -p bevy_integration_test --bin dump -- <out_dir>`

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/bms_luau_dump".to_string());
    let dir = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&dir).unwrap();
    let lad_path = bevy_integration_test::generate_lad(&dir);
    let lad = ladfile::parse_lad_file(&std::fs::read_to_string(&lad_path).unwrap()).unwrap();
    let defs = bevy_mod_scripting_luau::lad_to_luau(&lad);
    let defs_path = dir.join("api.d.luau");
    std::fs::write(&defs_path, &defs).unwrap();
    eprintln!("wrote {} ({} bytes)", defs_path.display(), defs.len());
}
