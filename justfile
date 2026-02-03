# Piper SDK - Justfile
# https://github.com/casey/just

# Default recipe (run with `just`)
default:
    @echo "Piper SDK - Available commands:"
    @echo ""
    @just --list

# Build the entire workspace
build:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo build --workspace

# Build specific package
build-pkg package:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo build -p {{package}}

# Run all tests
test:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo test --workspace

# Run tests for specific package (with optional extra arguments)
test-pkg package *args:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo test -p {{package}} {{args}}

# Run release build
release:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo build --workspace --release

# Clean build artifacts
clean:
    cargo clean

# Check code
check:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo check --all-targets

# Run linter (default, no MuJoCo needed)
clippy:
    cargo clippy --workspace --exclude piper-physics --all-targets --features "piper-driver/realtime" -- -D warnings

# Run linter with all features (excluding mock due to conflicts, requires MuJoCo)
clippy-all:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo clippy --workspace --all-targets --features "piper-driver/realtime,piper-sdk/serde,piper-tools/full" -- -D warnings

# Run linter on piper-physics only (requires MuJoCo)
clippy-physics:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo clippy -p piper-physics --all-targets -- -D warnings

# Run linter with mock mode (library code only, no tests/examples/bins)
clippy-mock:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    # Note: tests, examples, and bins require hardware backends (GsUsb, SocketCAN)
    # We use --lib to check only library source code with mock feature
    # Dynamically list library crates to avoid manual maintenance
    LIB_CRATES=$(bash scripts/list_library_crates.sh)
    cargo clippy $LIB_CRATES --lib --features "piper-driver/mock" -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Verify formatting
fmt-check:
    cargo fmt --all -- --check

# Build documentation
doc:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo doc --no-deps --document-private-items

# Check documentation links
doc-check:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    if [ -n "${MUJOCO_DYNAMIC_LINK_DIR:-}" ]; then
        >&2 echo "✓ Using MuJoCo from: $MUJOCO_DYNAMIC_LINK_DIR"
        case "$(uname -s)" in
            Linux*)
                >&2 echo "✓ RPATH embedded for Linux"
                ;;
            Darwin*)
                >&2 echo "✓ Framework linked for macOS"
                ;;
        esac
    fi
    cargo doc --no-deps --document-private-items 2>&1 | grep -i "warning\|error" && exit 1 || exit 0

# Show MuJoCo installation location
mujoco-info:
    @echo "=== MuJoCo Installation Info ==="
    @echo ""
    @just _mujojo_cache_info

# Clean MuJoCo installation
mujoco-clean:
    #!/usr/bin/env bash
    rm -rf ~/.local/lib/mujoco
    rm -rf ~/Library/Frameworks/mujoco.framework
    if [ -n "${LOCALAPPDATA:-}" ]; then
        rm -rf "$LOCALAPPDATA/mujoco"
    fi
    echo "✓ MuJoCo installation cleaned"

# Open shell with MuJoCo environment
mujoco-shell:
    #!/usr/bin/env bash
    eval "$(just _mujoco_download)"
    @echo "=== MuJoCo Environment Shell ==="
    @echo "Type 'exit' to leave the shell"
    @echo ""
    exec bash

# Private helper: Parse MuJoCo version from cargo metadata
# Uses cargo metadata instead of Cargo.lock to support library projects
# that don't commit Cargo.lock to version control
_mujoco_parse_version:
    #!/usr/bin/env bash
    # Use cargo metadata to get the resolved mujoco-rs version
    # Format: "2.3.0+mj-3.3.7" -> extract "3.3.7"
    cargo metadata --format-version 1 2>/dev/null | \
      python -c 'import sys, json; data = json.load(sys.stdin); pkgs = {p["name"]: p for p in data["packages"]}; print(pkgs.get("mujoco-rs", {}).get("version", "NOT_FOUND"))' | \
      sed -E 's/.*\+mj-([0-9.]+).*/\1/'

# Private helper: Download/setup MuJoCo (cross-platform with manual download)
_mujoco_download:
    #!/usr/bin/env bash
    set -euo pipefail

    # Get MuJoCo version from Cargo.lock
    mujoco_version=$(just _mujoco_parse_version)
    base_url="https://github.com/google-deepmind/mujoco/releases/download"

    # Detect platform and set directories
    case "$(uname -s)" in
        Linux*)
            install_dir="$HOME/.local/lib/mujoco"
            version_dir="$install_dir/mujoco-${mujoco_version}"
            lib_dir="$version_dir/lib"
            download_url="${base_url}/${mujoco_version}/mujoco-${mujoco_version}-linux-x86_64.tar.gz"
            ;;
        Darwin*)
            install_dir="$HOME/Library/Frameworks"
            framework_path="$install_dir/mujoco.framework"
            version_dir="$framework_path/Versions/A"
            download_url="${base_url}/${mujoco_version}/mujoco-${mujoco_version}-macos-universal2.dmg"
            ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            install_dir="$LOCALAPPDATA/mujoco"
            version_dir="$install_dir/mujoco-${mujoco_version}"
            lib_dir="$version_dir/lib"
            download_url="${base_url}/${mujoco_version}/mujoco-${mujoco_version}-windows-x86_64.zip"
            ;;
        *)
            >&2 echo "❌ Unsupported platform: $(uname -s)"
            exit 1
            ;;
    esac

    # Check if MuJoCo is already installed
    case "$(uname -s)" in
        Linux*|MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            if [ -d "$lib_dir" ]; then
                echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$lib_dir\""
                case "$(uname -s)" in
                    Linux*)
                        echo "export LD_LIBRARY_PATH=\"$lib_dir\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}\""
                        ;;
                    MINGW*|MSYS*|CYGWIN*|Windows_NT*)
                        # Copy DLL to target directories
                        bin_dir="$version_dir/bin"
                        if [ -f "$bin_dir/mujoco.dll" ]; then
                            for target_dir in target/debug target/release; do
                                mkdir -p "$target_dir"
                                cp -f "$bin_dir/mujoco.dll" "$target_dir/" 2>/dev/null || true
                            done
                            cp -f "$bin_dir/mujoco.dll" "./mujoco.dll" 2>/dev/null || true
                        fi
                        ;;
                esac
                >&2 echo "✓ Using cached MuJoCo: $lib_dir"
                exit 0
            fi
            ;;
        Darwin*)
            if [ -d "$version_dir" ]; then
                echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$version_dir\""
                echo "export DYLD_LIBRARY_PATH=\"$version_dir\${DYLD_LIBRARY_PATH:+:\$DYLD_LIBRARY_PATH}\""
                >&2 echo "✓ Using cached MuJoCo: $framework_path"
                exit 0
            fi
            ;;
    esac

    # Download and install MuJoCo
    mkdir -p "$install_dir"
    >&2 echo "Downloading MuJoCo ${mujoco_version}..."

    case "$(uname -s)" in
        Linux*)
            # Download and extract tar.gz
            curl -L "$download_url" | tar xz -C "$install_dir"
            >&2 echo "✓ MuJoCo installed to: $version_dir"
            ;;
        Darwin*)
            # Download DMG
            dmg_path="$install_dir/mujoco-${mujoco_version}.dmg"
            mount_point="/tmp/mujoco_mount_$$"

            curl -L -o "$dmg_path" "$download_url"

            # Remove quarantine from DMG
            xattr -d com.apple.quarantine "$dmg_path" 2>/dev/null || true

            # Mount DMG
            >&2 echo "Mounting DMG..."
            hdiutil attach "$dmg_path" -nobrowse -readonly -mountpoint "$mount_point" >/dev/null

            # Copy framework
            >&2 echo "Copying MuJoCo framework..."
            cp -R "$mount_point/mujoco.framework" "$install_dir/"

            # Unmount DMG
            hdiutil detach "$mount_point" >/dev/null 2>&1 || true

            # Remove quarantine from framework
            xattr -r -d com.apple.quarantine "$framework_path" 2>/dev/null || true

            # Create symlink for mujoco-rs build script (expects libmujoco.dylib at root)
            ln -sf "$version_dir/libmujoco.3.3.7.dylib" "$version_dir/libmujoco.dylib"

            # Clean up DMG
            rm -f "$dmg_path"

            >&2 echo "✓ MuJoCo installed to: $framework_path"
            ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            # Download and extract ZIP
            zip_path="$install_dir/mujoco-${mujoco_version}.zip"
            temp_extract_dir="$install_dir/temp_extract_$$"

            curl -L -o "$zip_path" "$download_url"
            
            # Extract to temporary directory first
            mkdir -p "$temp_extract_dir"
            
            # Try unzip first
            if command -v unzip &>/dev/null; then
                unzip -q -UU "$zip_path" -d "$temp_extract_dir" 2>/dev/null || true
            fi
            
            # If unzip failed or not available, try PowerShell
            # Convert paths to Windows format for PowerShell
            zip_path_win=$(echo "$zip_path" | sed 's|/|\\|g')
            temp_extract_dir_win=$(echo "$temp_extract_dir" | sed 's|/|\\|g')
            if [ ! -d "$temp_extract_dir/lib" ] && [ ! -d "$temp_extract_dir/mujoco-${mujoco_version}" ]; then
                >&2 echo "unzip failed, trying PowerShell Expand-Archive..."
                powershell -Command "Expand-Archive -Path '$zip_path_win' -DestinationPath '$temp_extract_dir_win' -Force" 2>/dev/null || true
            fi
            
            rm -f "$zip_path"
            
            # Check if ZIP contains version directory or direct contents
            if [ -d "$temp_extract_dir/mujoco-${mujoco_version}" ]; then
                # ZIP contains version directory, move it to install_dir
                mv "$temp_extract_dir/mujoco-${mujoco_version}" "$install_dir/" 2>/dev/null || \
                    cp -r "$temp_extract_dir/mujoco-${mujoco_version}" "$install_dir/" && \
                    rm -rf "$temp_extract_dir/mujoco-${mujoco_version}"
            elif [ -d "$temp_extract_dir/lib" ] || [ -d "$temp_extract_dir/bin" ]; then
                # ZIP contains direct contents, create version directory and move contents
                mkdir -p "$version_dir"
                # Move all contents from temp directory to version directory
                for item in "$temp_extract_dir"/*; do
                    [ -e "$item" ] && mv "$item" "$version_dir/" 2>/dev/null || true
                done
            else
                >&2 echo "❌ Failed to extract MuJoCo: unexpected ZIP structure"
                >&2 echo "   Temp directory contents: $(ls -la "$temp_extract_dir" 2>/dev/null || echo 'empty')"
                rm -rf "$temp_extract_dir"
                exit 1
            fi
            
            # Clean up temporary directory
            rmdir "$temp_extract_dir" 2>/dev/null || rm -rf "$temp_extract_dir" 2>/dev/null || true

            # Copy DLL to target directories
            bin_dir="$version_dir/bin"
            if [ -f "$bin_dir/mujoco.dll" ]; then
                for target_dir in target/debug target/release; do
                    mkdir -p "$target_dir"
                    cp -f "$bin_dir/mujoco.dll" "$target_dir/" 2>/dev/null || true
                done
                cp -f "$bin_dir/mujoco.dll" "./mujoco.dll" 2>/dev/null || true
                >&2 echo "✓ Copied mujoco.dll to target directories"
            fi

            >&2 echo "✓ MuJoCo installed to: $version_dir"
            ;;
    esac

    # Verify installation
    case "$(uname -s)" in
        Linux*|MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            if [ ! -d "$lib_dir" ]; then
                >&2 echo "❌ Failed to install MuJoCo: lib directory not found"
                exit 1
            fi
            echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$lib_dir\""
            case "$(uname -s)" in
                Linux*)
                    echo "export LD_LIBRARY_PATH=\"$lib_dir\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}\""
                    ;;
            esac
            ;;
        Darwin*)
            if [ ! -d "$version_dir" ]; then
                >&2 echo "❌ Failed to install MuJoCo: framework not found"
                exit 1
            fi
            echo "export MUJOCO_DYNAMIC_LINK_DIR=\"$version_dir\""
            echo "export DYLD_LIBRARY_PATH=\"$version_dir\${DYLD_LIBRARY_PATH:+:\$DYLD_LIBRARY_PATH}\""
            ;;
    esac


# Private helper: Show MuJoCo cache info
_mujojo_cache_info:
    #!/usr/bin/env bash
    case "$(uname -s)" in
        Linux*)
            install_dir="$HOME/.local/lib/mujoco"
            if [ -d "$install_dir/mujoco-3.3.7/lib" ]; then
                echo "Platform: Linux"
                echo "Status: ✓ Installed"
                echo "Location: $install_dir/mujoco-3.3.7/lib"
            else
                echo "Platform: Linux"
                echo "Status: (not installed yet)"
                echo "Location: $install_dir"
            fi
            ;;
        Darwin*)
            framework_dir="$HOME/Library/Frameworks/mujoco.framework"
            if [ -d "$framework_dir/Versions/A" ]; then
                echo "Platform: macOS"
                echo "Status: ✓ Installed"
                echo "Location: $framework_dir"
            else
                echo "Platform: macOS"
                echo "Status: (not installed yet)"
                echo "Location: $framework_dir"
            fi
            ;;
        MINGW*|MSYS*|CYGWIN*|Windows_NT*)
            install_dir="$LOCALAPPDATA/mujoco"
            if [ -d "$install_dir/mujoco-3.3.7/lib" ]; then
                echo "Platform: Windows"
                echo "Status: ✓ Installed"
                echo "Location: $install_dir/mujoco-3.3.7/lib"
            else
                echo "Platform: Windows"
                echo "Status: (not installed yet)"
                echo "Location: $install_dir"
            fi
            ;;
        *)
            echo "Platform: Unknown"
            echo "Status: Unsupported"
            ;;
    esac
