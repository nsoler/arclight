#!/usr/bin/env bash
# Cross-build ArclightVFX for Windows x64 on Linux and package it as a zip:
#   dist/arclightvfx-<ver>-windows-x86_64.zip
# containing
#   bin/arclight-gui.exe, bin/arclight.exe     the GUI and the CLI / MCP server
#   share/arclight/examples/media/testsrc.mp4  the starter project's clip
#   share/arclight/examples/*.json             Help ▸ Examples
#   share/arclight/icons/                      the icons
#   README-WINDOWS.txt
#
# Run from anywhere: packaging/windows/build-zip.sh
# Needs the Rust target (rustup target add x86_64-pc-windows-gnu), zip, and
# zig: on PATH, or ZIG=/path/to/zig (pip's `ziglang` package has one). zig
# links in place of MinGW: the wrappers made here drop the MinGW-only flags,
# link LLVM's libunwind for libgcc_eh, and stand in for MinGW's dlltool.
# The .exe files import only DLLs Windows 10 and 11 ship (the UCRT among
# them), so nothing else is bundled. ffmpeg isn't bundled: install it.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
target=x86_64-pc-windows-gnu
name="arclightvfx-$version-windows-x86_64"
dist="$repo/dist"
stage="$dist/$name"
zig="${ZIG:-$(command -v zig || true)}"
[ -x "$zig" ] || { echo "zig not found: put it on PATH or set ZIG" >&2; exit 1; }

tools="$(mktemp -d)"
trap 'rm -rf "$tools"' EXIT
mkdir "$tools/path"
cat > "$tools/cc" <<EOF
#!/usr/bin/env bash
args=()
for a in "\$@"; do
  case "\$a" in
    -lgcc_eh) args+=(-lunwind) ;;
    -lmsvcrt|-lgcc|-lgcc_s|-l:libpthread.a|-nodefaultlibs|-Wl,-Bdynamic|-Wl,-Bstatic|-Wl,-O1) ;;
    -Wl,--disable-auto-image-base|*rsbegin.o|*rsend.o) ;;
    *) args+=("\$a") ;;
  esac
done
exec "$zig" cc -target x86_64-windows-gnu "\${args[@]}"
EOF
cat > "$tools/ar" <<EOF
#!/usr/bin/env bash
exec "$zig" ar "\$@"
EOF
# rustc runs MinGW's dlltool by name for raw-dylib imports.
cat > "$tools/path/x86_64-w64-mingw32-dlltool" <<EOF
#!/usr/bin/env bash
args=(); skip=0
for a in "\$@"; do
  if [ \$skip = 1 ]; then skip=0; continue; fi
  case "\$a" in
    -f|--64|--no-leading-underscore|--temp-prefix=*) ;;
    --temp-prefix) skip=1 ;;
    *) args+=("\$a") ;;
  esac
done
exec "$zig" dlltool "\${args[@]}"
EOF
chmod +x "$tools/cc" "$tools/ar" "$tools/path/"*

echo "Building ArclightVFX $version for Windows x64..."
PATH="$tools/path:$PATH" \
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$tools/cc" \
CC_x86_64_pc_windows_gnu="$tools/cc" \
AR_x86_64_pc_windows_gnu="$tools/ar" \
  cargo build --release --manifest-path "$repo/Cargo.toml" --target "$target" --bin arclight-gui --bin arclight
out="${CARGO_TARGET_DIR:-$repo/target}/$target/release"

rm -rf "$stage" && mkdir -p "$stage/bin" "$stage/share/arclight/examples/media" "$stage/share/arclight/icons"
install -m755 "$out/arclight-gui.exe" "$out/arclight.exe" "$stage/bin/"
install -m644 "$repo/examples/media/testsrc.mp4" "$stage/share/arclight/examples/media/"
# Help ▸ Examples (the list is EXAMPLES in src/gui/main.rs).
install -m644 "$repo/examples/geo-scatter.json" "$stage/share/arclight/examples/"
install -m644 "$repo/assets/arclight.svg" "$repo/assets/arclight-icon.svg" "$repo/assets/arclight.png" "$stage/share/arclight/icons/"
install -m644 "$repo/CHANGELOG.md" "$stage/"
sed "s/@VERSION@/$version/; s/\$/\r/" > "$stage/README-WINDOWS.txt" <<'EOF'
ArclightVFX Compositor @VERSION@ (beta) for Windows x64
=======================================================

Unzip anywhere and run bin\arclight-gui.exe (the GUI) or bin\arclight.exe
(the command line and MCP server). Keep the bin and share folders together:
the GUI finds its starter clip and Help > Examples in ..\share\arclight.

Needs Windows 10 or 11 (64-bit). GPU acceleration uses Direct3D 12 or
Vulkan; without a usable GPU it renders on the CPU.

For video files, install ffmpeg and put it on your PATH.

This is an early, cross-compiled Windows build:
- The scripting bridge (Python scripts or `arclight mcp --attach` driving a
  running GUI window) is not available on Windows yet. The MCP server
  itself (`arclight mcp`, stdio or --http) works.
- The GUI opens with a console window beside it, which shows its log.
- The binaries are not code-signed, so SmartScreen may warn on first run
  ("More info" > "Run anyway").
EOF

zipfile="$dist/$name.zip"
rm -f "$zipfile"
(cd "$dist" && zip -qr9 "$zipfile" "$name")
rm -rf "$stage"

echo "Done: $zipfile"
