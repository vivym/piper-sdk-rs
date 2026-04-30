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

#[test]
fn profile_canonical_toml_accepts_raw_clock_table() {
    let mut profile = piper_svs_collect::profile::EffectiveProfile::default_for_tests();
    profile.raw_clock.warmup_secs = 11;
    profile.raw_clock.residual_p95_us = 2_100;
    profile.raw_clock.residual_max_us = 3_200;
    let bytes = profile.to_canonical_toml_bytes().expect("profile should serialize");
    let text = String::from_utf8(bytes).expect("canonical profile should be utf8");

    assert!(text.contains("[raw_clock]"));
    assert!(text.contains("warmup_secs = 11"));

    let parsed: piper_svs_collect::profile::EffectiveProfile =
        toml::from_str(&text).expect("profile should parse");
    parsed.validate().expect("profile should validate");
    assert_eq!(parsed.raw_clock.warmup_secs, 11);
}

#[test]
fn profile_partial_raw_clock_table_uses_field_defaults() {
    let text = r#"
[raw_clock]
warmup_secs = 12
residual_p95_us = 2200
"#;

    let parsed: piper_svs_collect::profile::EffectiveProfile =
        piper_svs_collect::profile::EffectiveProfile::from_overlay_toml(text)
            .expect("partial raw_clock overlay should parse");

    parsed.validate().expect("partial profile should validate");
    assert_eq!(parsed.raw_clock.warmup_secs, 12);
    assert_eq!(parsed.raw_clock.residual_p95_us, 2_200);
    assert_eq!(parsed.raw_clock.residual_max_us, 3_000);
    assert_eq!(parsed.raw_clock.alignment_lag_us, 5_000);
    assert_eq!(parsed.raw_clock.alignment_search_window_us, 25_000);
}
