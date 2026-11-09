#!/usr/bin/env sh
# Builds the Linux release and installs it for the current user (no sudo).
# Works from any folder:  sh app/packaging/build-and-install-linux.sh
set -eu
cd "$(dirname "$0")/.."

sh packaging/build-linux.sh
sh dist/ferrous-commander/install.sh
