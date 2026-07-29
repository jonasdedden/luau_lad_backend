//! A minimal but *real* Bevy + `bevy_mod_scripting` scripting environment, used by
//! the integration tests to produce a genuine LAD file from a live reflection
//! registry — the same way a real game would generate its script API.

use std::path::{Path, PathBuf};

use bevy::asset::AssetPlugin;
use bevy::log::LogPlugin;
use bevy::prelude::*;
use bevy::MinimalPlugins;
use bevy_mod_scripting::prelude::*;
use bevy_mod_scripting::BMSPlugin;
use ladfile_builder::plugin::ScriptingFilesGenerationPlugin;

/// World position of an entity.
#[derive(Component, Reflect, Debug, Clone, Copy, Default)]
#[reflect(Component, Default)]
pub struct Position {
    /// World X position.
    pub x: f64,
    /// World Y position.
    pub y: f64,
    /// World Z position.
    pub z: f64,
}

/// Velocity of an entity, in world units per second.
#[derive(Component, Reflect, Debug, Clone, Copy, Default)]
#[reflect(Component, Default)]
pub struct Velocity {
    /// Velocity along X.
    pub x: f64,
    /// Velocity along Y.
    pub y: f64,
    /// Velocity along Z.
    pub z: f64,
}

/// Health of an entity.
#[derive(Component, Reflect, Debug, Clone, Copy, Default)]
#[reflect(Component, Default)]
pub struct Health {
    /// Current health.
    pub current: f64,
    /// Maximum health.
    pub max: f64,
}

/// Behavior stance of an entity — a plain unit enum, exercising the exported
/// variant-name union and the typed `variant_name` override.
#[derive(Reflect, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[reflect(Default)]
pub enum Stance {
    /// Standing around.
    #[default]
    Idle,
    /// Attacking on sight.
    Aggressive,
}

/// Build a headless scripting app, register the components and host globals, and
/// dump the reflection registry to a LAD file in `out_dir`. Returns the path of
/// the written `bindings.lad.json`.
pub fn generate_lad(out_dir: &Path) -> PathBuf {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin::default())
        .add_plugins(LogPlugin::default())
        .add_plugins(BMSPlugin);

    // The component surface scripts get to see.
    app.register_type::<Position>()
        .register_type::<Velocity>()
        .register_type::<Health>()
        .register_type::<Stance>();

    // Host functions exposed to scripts as plain Luau globals.
    {
        let world = app.world_mut();
        NamespaceBuilder::<GlobalNamespace>::new_unregistered(world)
            .register("info", |msg: String| {
                info!("[script] {msg}");
            })
            .register("magnitude", |x: f64, y: f64, z: f64| {
                (x * x + y * y + z * z).sqrt()
            });
    }

    // Dump the registry to a LAD file on the Startup schedule.
    app.add_plugins(ScriptingFilesGenerationPlugin::new(
        true,
        out_dir.to_path_buf(),
        Some(PathBuf::from("bindings.lad.json")),
        "Integration-test scripting API",
        true,
        false,
    ));

    app.finish();
    app.cleanup();
    app.update(); // runs the generation system, writing the LAD file

    out_dir.join("bindings.lad.json")
}
