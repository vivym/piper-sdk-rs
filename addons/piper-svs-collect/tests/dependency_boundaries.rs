#[test]
fn default_workspace_tree_is_mujoco_free() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|addons| addons.parent())
        .expect("collector crate should live under <repo>/addons/piper-svs-collect");
    let output = std::process::Command::new("cargo")
        .current_dir(repo_root)
        .args(["tree", "--workspace", "--all-features"])
        .output()
        .expect("cargo tree should run");

    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(!tree.contains("mujoco"));
    assert!(!tree.contains("piper-physics"));
    assert!(!tree.contains("piper-svs-collect"));
}

#[test]
fn starter_profiles_parse_and_validate() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for profile_name in ["wiping", "peg_insertion", "surface_following"] {
        let path = manifest_dir.join("profiles").join(format!("{profile_name}.toml"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let profile: piper_svs_collect::profile::EffectiveProfile = toml::from_str(&text)
            .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()));
        profile
            .validate()
            .unwrap_or_else(|error| panic!("invalid profile {}: {error}", path.display()));
    }
}
