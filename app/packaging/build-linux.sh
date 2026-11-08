#!/usr/bin/env sh
# Builds the Linux release, a tar.gz with an install script, and a .deb when cargo-deb is present.
# Needs (Debian/Ubuntu): sudo apt install build-essential libxkbcommon-dev libgl1-mesa-dev libwayland-dev
# Run from the app folder:  sh packaging/build-linux.sh
set -eu
cd "$(dirname "$0")/.."

if [ "$(uname -s)" != Linux ]; then
    echo "Build this package on Linux; use packaging/build-windows.ps1 on Windows." >&2
    exit 1
fi

cargo build --release --locked
# Honour Cargo's output directory and name the archive for the actual CPU.
target_dir=${CARGO_TARGET_DIR:-target}
case "$target_dir" in
    /*) ;;
    *) target_dir="$PWD/$target_dir" ;;
esac
arch=$(uname -m)
mkdir -p dist/ferrous-commander
cp "$target_dir/release/ferrous-commander" dist/ferrous-commander/
cp packaging/linux/ferrous-commander.desktop packaging/linux/ferrous-commander.png dist/ferrous-commander/
cp README.md ../LICENSE dist/ferrous-commander/
cat > dist/ferrous-commander/install.sh <<'EOF'
#!/usr/bin/env sh
# Installs for the current user (no sudo): ~/.local/bin + app menu entry.
set -eu
cd "$(dirname "$0")"
exec ./ferrous-commander --install
EOF
chmod +x dist/ferrous-commander/install.sh dist/ferrous-commander/ferrous-commander
archive="dist/ferrous-commander-linux-$arch.tar.gz"
tar -C dist -czf "$archive" ferrous-commander
echo "Archive: $archive (extract, run ./install.sh)"

if command -v cargo-deb >/dev/null 2>&1; then
    cargo deb --no-build
    cp "$target_dir"/debian/*.deb dist/
    echo "Debian package in dist/"
else
    echo "cargo-deb not found: skipped the .deb (cargo install cargo-deb)"
fi
