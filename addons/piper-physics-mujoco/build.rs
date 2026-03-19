//! Minimal MuJoCo build script
//!
//! This build script only handles:
//! 1. RPATH embedding for Linux (runtime library path)
//! 2. cargo:rustc-env for test execution
//!
//! MuJoCo download/detection is handled by justfile (_mujoco_download recipe)

use std::env;

fn main() {
    // Tell cargo to rerun this script if MUJOCO_DYNAMIC_LINK_DIR changes
    println!("cargo:rerun-if-env-changed=MUJOCO_DYNAMIC_LINK_DIR");

    // Check if MUJOCO_DYNAMIC_LINK_DIR is set (by just wrapper)
    if let Ok(lib_dir) = env::var("MUJOCO_DYNAMIC_LINK_DIR") {
        // Compile-time: Tell linker where to find the library
        println!("cargo:rustc-link-search=native={}", lib_dir);

        // Runtime: Embed RPATH for Linux (so binary can run without LD_LIBRARY_PATH)
        #[cfg(target_os = "linux")]
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir);

        // Test-time: Set LD_LIBRARY_PATH for cargo test
        println!("cargo:rustc-env=LD_LIBRARY_PATH={}", lib_dir);

        // Silent success - information is printed by just wrapper
    } else {
        // Error: user needs to run via just or set environment manually
        println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR not set");
        println!("cargo:warning=Please use: just build-physics");
        println!(
            "cargo:warning=Or validate with: just check-physics / just test-physics / just clippy-physics"
        );
        println!("cargo:warning=Or set: export MUJOCO_DYNAMIC_LINK_DIR=/path/to/mujoco/lib");
    }
}
