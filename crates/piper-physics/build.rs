//! MuJoCo automatic configuration build script
//!
//! This build script automatically downloads and configures MuJoCo for all platforms:
//! - Linux: Downloads tar.gz, extracts to ~/.local/lib/mujoco/
//! - macOS: Downloads DMG, mounts, copies framework, removes quarantine
//! - Windows: Downloads ZIP, extracts to %LOCALAPPDATA%\mujoco\
//!
//! If MUJOCO_DYNAMIC_LINK_DIR is already set, automatic configuration is skipped.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

// MuJoCo version and download URLs
const MUJOCO_VERSION: &str = "3.3.7";
const MUJOCO_BASE_URL: &str = "https://github.com/google-deepmind/mujoco/releases/download";

fn main() {
    // Tell cargo to rerun this script if these environment variables change
    println!("cargo:rerun-if-env-changed=MUJOCO_DYNAMIC_LINK_DIR");
    println!("cargo:rerun-if-env-changed=MUJOCO_AUTO_CONFIG");

    // If user has manually configured MuJoCo, use it and embed RPATH
    if let Ok(manual_path) = env::var("MUJOCO_DYNAMIC_LINK_DIR") {
        println!("cargo:warning=Using manually configured MUJOCO_DYNAMIC_LINK_DIR");
        println!("cargo:warning=Path: {}", manual_path);

        // Embed RPATH for zero-configuration even with manual path
        #[cfg(target_os = "linux")]
        embed_linux_rpath(&manual_path);

        #[cfg(target_os = "macos")]
        embed_macos_rpath(&manual_path);

        #[cfg(target_os = "windows")]
        {
            println!("cargo:warning=Note: Windows uses DLL copying, not RPATH");
        }

        return;
    }

    // Check if auto-configuration is disabled
    if env::var("MUJOCO_AUTO_CONFIG").ok().as_deref() == Some("false") {
        println!("cargo:warning=MuJoCo auto-configuration is disabled");
        println!("cargo:warning=Set MUJOCO_DYNAMIC_LINK_DIR to use MuJoCo");
        return;
    }

    // Platform-specific configuration
    #[cfg(target_os = "linux")]
    if let Err(e) = configure_linux() {
        print_config_error(&*e);
    }

    #[cfg(target_os = "macos")]
    if let Err(e) = configure_macos() {
        print_config_error(&*e);
    }

    #[cfg(target_os = "windows")]
    if let Err(e) = configure_windows() {
        print_config_error(&*e);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        println!("cargo:warning=MuJoCo auto-configuration not supported on this platform");
        println!("cargo:warning=Please manually install MuJoCo and set MUJOCO_DYNAMIC_LINK_DIR");
    }
}

fn print_config_error(error: &dyn std::error::Error) {
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!("❌ MuJoCo Automatic Configuration Failed");
    eprintln!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    eprintln!();
    eprintln!("Error: {}", error);
    eprintln!();
    eprintln!("You can manually install MuJoCo:");
    eprintln!(
        "  1. Download MuJoCo from: {}/{}/",
        MUJOCO_BASE_URL, MUJOCO_VERSION
    );
    eprintln!("  2. Extract to a directory of your choice");
    eprintln!("  3. Set MUJOCO_DYNAMIC_LINK_DIR to the lib directory");
    eprintln!();
    eprintln!("For detailed instructions, see:");
    eprintln!("  crates/piper-physics/README.md");
    eprintln!("  https://github.com/google-deepmind/mujoco/blob/main/BUILD.md");
    eprintln!();
}

// ============================================================================
// RPATH Embedding Helper Functions
// ============================================================================

/// Embed RPATH for Linux (manual MUJOCO_DYNAMIC_LINK_DIR)
#[cfg(target_os = "linux")]
fn embed_linux_rpath(lib_path: &str) {
    println!("cargo:rustc-link-search={}", lib_path);
    println!("cargo:rustc-link-lib=mujoco");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_path);
    println!("cargo:warning=✓ RPATH embedded: {}", lib_path);
}

/// Embed RPATH for macOS (manual MUJOCO_DYNAMIC_LINK_DIR)
#[cfg(target_os = "macos")]
fn embed_macos_rpath(lib_path: &str) {
    // Check if it's a framework path
    if lib_path.contains("framework") {
        // Extract framework parent directory from path like "/path/to/mujoco.framework/Versions/A"
        // We need the directory CONTAINING the framework, not the framework itself
        let path_buf = PathBuf::from(lib_path);
        let framework_parent = path_buf
            .parent() // Versions/A -> Versions
            .and_then(|p| p.parent()) // Versions -> mujoco.framework
            .and_then(|p| p.parent()) // mujoco.framework -> parent directory
            .ok_or("Cannot determine framework parent directory")
            .unwrap();

        println!(
            "cargo:rustc-link-search=framework={}",
            framework_parent.to_string_lossy()
        );
        println!("cargo:rustc-link-lib=framework=mujoco");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
        println!(
            "cargo:rustc-link-arg=-Wl,-rpath,{}",
            framework_parent.to_string_lossy()
        );
        println!(
            "cargo:warning=✓ RPATH embedded: @executable_path and {}",
            framework_parent.to_string_lossy()
        );
    } else {
        // Regular library path
        println!("cargo:rustc-link-search={}", lib_path);
        println!("cargo:rustc-link-lib=mujoco");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_path);
        println!(
            "cargo:warning=✓ RPATH embedded: @executable_path and {}",
            lib_path
        );
    }
}

// ============================================================================
// Common Utilities
// ============================================================================

fn get_download_url(platform: &str) -> String {
    let file = match platform {
        "linux" => "linux-x86_64.tar.gz",
        "macos" => "macos-universal.dmg",
        "windows" => "windows-x64.zip",
        _ => panic!("Unsupported platform: {}", platform),
    };

    format!("{}/{}/mujoco-{}", MUJOCO_BASE_URL, MUJOCO_VERSION, file)
}

fn download_file(url: &str, path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    println!("cargo:warning=📥 Downloading MuJoCo from {}...", url);
    println!("cargo:warning=   This may take a few minutes on first build...");

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(600)) // 10 minutes timeout
        .user_agent(&format!("piper-physics/{}", env!("CARGO_PKG_VERSION")))
        .build();

    let response = agent.get(url).call()?;

    // Get total file size if available
    let total_size = response.header("Content-Length").and_then(|v| v.parse::<u64>().ok());

    if let Some(size) = total_size {
        println!(
            "cargo:warning=   Download size: {:.1} MB",
            size as f64 / 1024.0 / 1024.0
        );
    }

    let mut reader = response.into_reader();
    let mut file = fs::File::create(path)?;
    std::io::copy(&mut reader, &mut file)?;

    println!("cargo:warning=✓ Download completed");
    Ok(())
}

fn generate_env_script(platform: &str, lib_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let script_path = match platform {
        "linux" | "macos" => PathBuf::from("setup_mujoco.sh"),
        "windows" => PathBuf::from("setup_mujoco.ps1"),
        _ => return Ok(()),
    };

    let content = match platform {
        "linux" => format!(
            r#"#!/bin/bash
# MuJoCo Environment Setup Script for Linux
# Usage: source setup_mujoco.sh

export MUJOCO_DYNAMIC_LINK_DIR={}
export LD_LIBRARY_PATH=$LD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR

echo "✓ MuJoCo environment variables set"
echo "  MUJOCO_DYNAMIC_LINK_DIR=$MUJOCO_DYNAMIC_LINK_DIR"
echo ""
echo "To make this persistent, add to ~/.bashrc or ~/.zshrc:"
echo "  source $(realpath setup_mujoco.sh)"
"#,
            lib_path
        ),
        "macos" => format!(
            r#"#!/bin/bash
# MuJoCo Environment Setup Script for macOS
# Usage: source setup_mujoco.sh

export MUJOCO_DYNAMIC_LINK_DIR={}
export DYLD_LIBRARY_PATH=$DYLD_LIBRARY_PATH:$MUJOCO_DYNAMIC_LINK_DIR

echo "✓ MuJoCo environment variables set"
echo "  MUJOCO_DYNAMIC_LINK_DIR=$MUJOCO_DYNAMIC_LINK_DIR"
echo ""
echo "To make this persistent, add to ~/.zshrc or ~/.bash_profile:"
echo "  source $(realpath setup_mujoco.sh)"
"#,
            lib_path
        ),
        "windows" => format!(
            r#"# MuJoCo Environment Setup Script for Windows
# Usage: .\setup_mujoco.ps1

$env:MUJOCO_DYNAMIC_LINK_DIR="{}"

Write-Host "✓ MuJoCo environment variables set"
Write-Host "  MUJOCO_DYNAMIC_LINK_DIR=$env:MUJOCO_DYNAMIC_LINK_DIR"
Write-Host ""
Write-Host "To make this persistent, add to your PowerShell Profile:"
Write-Host "  (Get-Content setup_mujoco.ps1) | Add-Content $PROFILE"
"#,
            lib_path
        ),
        _ => return Ok(()),
    };

    fs::write(&script_path, content)?;

    // Set executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms)?;
    }

    println!(
        "cargo:warning=📜 Environment setup script generated: {:?}",
        script_path
    );
    println!("cargo:warning=   Please run: source setup_mujoco.sh");
    println!();

    Ok(())
}

// ============================================================================
// Linux Configuration
// ============================================================================

#[cfg(target_os = "linux")]
fn configure_linux() -> Result<(), Box<dyn std::error::Error>> {
    use flate2::read::GzDecoder;
    use std::os::unix::fs::symlink;
    use tar::Archive;

    println!("cargo:warning=🐧 Configuring MuJoCo for Linux...");
    println!();

    // 1. Determine installation directory
    let home = dirs::home_dir().ok_or("无法确定用户主目录")?;
    let base_dir = home.join(".local/lib/mujoco");
    let version_dir = base_dir.join(format!("mujoco-{}", MUJOCO_VERSION));
    let current_dir = base_dir.join("current");
    let lib_dir = current_dir.join("lib");

    // 2. Check if already installed
    if version_dir.exists() {
        println!(
            "cargo:warning=✓ MuJoCo {} already installed",
            MUJOCO_VERSION
        );
        println!("cargo:warning=  Location: {:?}", version_dir);

        // Update current symlink
        let _ = fs::remove_file(&current_dir);
        symlink(&version_dir, &current_dir)?;
    } else {
        // 3. Create directory
        fs::create_dir_all(&base_dir)?;

        // 4. Download
        let download_url = get_download_url("linux");
        let tar_path = base_dir.join(format!("mujoco-{}.tar.gz", MUJOCO_VERSION));

        download_file(&download_url, &tar_path)?;

        // 5. Extract
        println!("cargo:warning=📦 Extracting MuJoCo...");
        let file = fs::File::open(&tar_path)?;
        let decoder = GzDecoder::new(file);
        let mut archive = Archive::new(decoder);
        archive.unpack(&base_dir)?;

        // Clean up tar.gz
        let _ = fs::remove_file(&tar_path);

        // 6. Create current symlink
        symlink(&version_dir, &current_dir)?;

        println!("cargo:warning=✓ MuJoCo installed successfully");
    }

    // 7. Setup linker paths
    let lib_path = lib_dir.to_string_lossy();
    println!("cargo:rustc-link-search={}", lib_path);
    println!("cargo:rustc-link-lib=mujoco");

    // 8. Embed RPATH for zero-configuration (executable finds library automatically)
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_path);
    println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
    println!("cargo:warning=✓ RPATH embedded: {}", lib_path);

    // 9. Generate environment setup script (optional, for debugging)
    generate_env_script("linux", &lib_path)?;

    println!("cargo:warning=");
    println!("cargo:warning=🎉 MuJoCo configuration complete!");
    println!("cargo:warning=");
    println!();

    Ok(())
}

// ============================================================================
// macOS Configuration
// ============================================================================

#[cfg(target_os = "macos")]
fn configure_macos() -> Result<(), Box<dyn std::error::Error>> {
    use std::process::Command;

    println!("cargo:warning=🍎 Configuring MuJoCo for macOS...");
    println!();

    // 1. Determine installation directory
    let home = dirs::home_dir().ok_or("无法确定用户主目录")?;
    let frameworks_dir = home.join("Library/Frameworks");
    let framework_path = frameworks_dir.join("mujoco.framework");
    let mount_point = PathBuf::from("/tmp/mujoco_mount_piper");
    let dmg_path = frameworks_dir.join(format!("mujoco-{}.dmg", MUJOCO_VERSION));

    // 2. Check if already installed
    if framework_path.join("Versions/A").exists() {
        println!("cargo:warning=✓ MuJoCo framework already installed");
        println!("cargo:warning=  Location: {:?}", framework_path);

        setup_macos_linking(&framework_path)?;
        return Ok(());
    }

    // 3. Create directory
    fs::create_dir_all(&frameworks_dir)?;

    // 4. Download DMG
    let download_url = get_download_url("macos");
    download_file(&download_url, &dmg_path)?;

    // 5. Remove quarantine from DMG
    println!("cargo:warning=🔓 Removing quarantine from DMG...");
    let _ = Command::new("xattr")
        .args(["-d", "com.apple.quarantine"])
        .arg(&dmg_path)
        .status();

    // 6. Mount DMG
    println!("cargo:warning=💿 Mounting DMG...");
    let attach_output = Command::new("hdiutil")
        .args([
            "attach",
            &dmg_path.to_string_lossy(),
            "-nobrowse",
            "-readonly",
            "-mountpoint",
            &mount_point.to_string_lossy(),
        ])
        .output()?;

    if !attach_output.status.success() {
        let stderr = String::from_utf8_lossy(&attach_output.stderr);
        return Err(format!("Failed to mount DMG: {}", stderr).into());
    }

    // Ensure DMG is unmounted even if later steps fail
    struct CleanupGuard {
        mount_point: PathBuf,
    }
    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            println!("cargo:warning=💿 Ejecting DMG...");
            let _ = Command::new("hdiutil")
                .args(["detach", &self.mount_point.to_string_lossy()])
                .status();
        }
    }
    let _guard = CleanupGuard {
        mount_point: mount_point.clone(),
    };

    // 7. Copy framework
    println!("cargo:warning=📋 Copying MuJoCo framework...");
    let src_framework = mount_point.join("mujoco.framework");

    let copy_output =
        Command::new("cp").arg("-R").arg(&src_framework).arg(&frameworks_dir).output()?;

    if !copy_output.status.success() {
        let stderr = String::from_utf8_lossy(&copy_output.stderr);
        return Err(format!("Failed to copy framework: {}", stderr).into());
    }

    // 8. Remove quarantine from framework (recursive)
    println!("cargo:warning=🔓 Removing quarantine from framework...");
    let _ = Command::new("xattr")
        .args(["-r", "-d", "com.apple.quarantine"])
        .arg(&framework_path)
        .status();

    // 9. Setup linking
    setup_macos_linking(&framework_path)?;

    // 10. Clean up DMG
    println!("cargo:warning=🧹 Cleaning up...");
    let _ = fs::remove_file(&dmg_path);

    println!("cargo:warning=");
    println!("cargo:warning=🎉 MuJoCo configuration complete!");
    println!("cargo:warning=");
    println!();

    Ok(())
}

#[cfg(target_os = "macos")]
fn setup_macos_linking(framework_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // macOS framework stores dylib directly in Versions/A/, not in Libraries/
    let version_a_dir = framework_path.join("Versions/A");
    let lib_path = version_a_dir.to_string_lossy();
    let framework_dir = framework_path
        .parent()
        .ok_or("Cannot get framework directory")?
        .to_string_lossy();

    // Setup linker
    println!("cargo:rustc-link-search=framework={}", framework_dir);
    println!("cargo:rustc-link-lib=framework=mujoco");

    // Embed RPATH for zero-configuration (executable finds library automatically)
    // RPATH must point to the directory CONTAINING the framework, not inside it
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
    println!("cargo:rustc-link-arg=-Wl,-rpath,{}", framework_dir);
    println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);
    println!(
        "cargo:warning=✓ RPATH embedded: @executable_path and {}",
        framework_dir
    );

    // Generate environment setup script (optional, for debugging)
    generate_env_script("macos", &lib_path)?;

    Ok(())
}

// ============================================================================
// Windows Configuration
// ============================================================================

#[cfg(target_os = "windows")]
fn configure_windows() -> Result<(), Box<dyn std::error::Error>> {
    use zip::ZipArchive;

    println!("cargo:warning=🪟 Configuring MuJoCo for Windows...");
    println!();

    // 1. Determine installation directory
    let local_app_data = env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        dirs::home_dir()
            .ok_or("Cannot determine home directory")?
            .join("AppData")
            .join("Local")
            .to_string_lossy()
            .to_string()
    });

    let base_dir = PathBuf::from(&local_app_data).join("mujoco");
    let version_dir = base_dir.join(format!("mujoco-{}", MUJOCO_VERSION));
    let current_dir = base_dir.join("current");
    let lib_dir = current_dir.join("lib");

    // 2. Check if already installed
    if version_dir.exists() {
        println!(
            "cargo:warning=✓ MuJoCo {} already installed",
            MUJOCO_VERSION
        );
        println!("cargo:warning=  Location: {:?}", version_dir);
    } else {
        // 3. Create directory
        fs::create_dir_all(&base_dir)?;

        // 4. Download ZIP
        let download_url = get_download_url("windows");
        let zip_path = base_dir.join(format!("mujoco-{}.zip", MUJOCO_VERSION));

        download_file(&download_url, &zip_path)?;

        // 5. Extract
        println!("cargo:warning=📦 Extracting MuJoCo...");
        let file = fs::File::open(&zip_path)?;
        let mut archive = ZipArchive::new(file)?;

        archive.extract(&base_dir)?;

        // Clean up ZIP
        let _ = fs::remove_file(&zip_path);

        println!("cargo:warning=✓ MuJoCo installed successfully");
    }

    // 6. Setup linker paths
    let lib_path = lib_dir.to_string_lossy();
    println!("cargo:rustc-link-search={}", lib_path);
    println!("cargo:rustc-link-lib=mujoco");
    println!("cargo:warning=MUJOCO_DYNAMIC_LINK_DIR={}", lib_path);

    // 7. Copy DLL to target directories for zero-configuration
    // Windows requires DLL in same directory as executable or in PATH
    if let Ok(src_dll) = version_dir.join("bin").join("mujoco.dll").canonicalize() {
        // Copy to both debug and release target directories
        for target in &["debug", "release"] {
            let target_dir = PathBuf::from("target").join(target);
            if target_dir.exists() {
                let dest = target_dir.join("mujoco.dll");
                match fs::copy(&src_dll, &dest) {
                    Ok(_) => println!("cargo:warning=✓ Copied mujoco.dll to target/{}", target),
                    Err(e) => println!(
                        "cargo:warning=Warning: Failed to copy DLL to target/{}: {}",
                        target, e
                    ),
                }
            }
        }

        // Also copy to project root for cargo run compatibility
        let project_dll = PathBuf::from("mujoco.dll");
        if !project_dll.exists() {
            match fs::copy(&src_dll, &project_dll) {
                Ok(_) => {
                    println!("cargo:warning=✓ Copied mujoco.dll to project root (for cargo run)")
                },
                Err(e) => println!(
                    "cargo:warning=Warning: Failed to copy DLL to project root: {}",
                    e
                ),
            }
        }

        println!("cargo:warning=✓ DLL zero-configuration complete");
    }

    // 8. Generate environment setup script (optional, for debugging)
    generate_env_script("windows", &lib_path)?;

    println!("cargo:warning=");
    println!("cargo:warning=🎉 MuJoCo configuration complete!");
    println!("cargo:warning=");
    println!();

    Ok(())
}
