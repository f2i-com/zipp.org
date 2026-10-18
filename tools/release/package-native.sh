#!/usr/bin/env bash
# tools/release/package-native.sh <arch> <binary> <out-dir> <version> <commit>
#
# One Linux release asset, zipp-<version>-linux-<arch>: the built binary
# itself, mode 0755, checked before it counts. It must report this version
# at this commit from a clean tree (build.rs reports dirty whenever its `git
# status` probe fails, and an asset reporting another commit used to be
# published immutably). RUN_PREFIX runs a foreign-architecture binary (CI's
# armv7 job: "qemu-arm -L /usr/arm-linux-gnueabihf").
set -euo pipefail
arch=$1
binary=$2
out=$3
version=$4
commit=$5
mkdir -p "$out"
asset="$out/zipp-$version-linux-$arch"
install -m 0755 "$binary" "$asset"
read -r -a prefix <<<"${RUN_PREFIX:-}"
info="$(${prefix[@]+"${prefix[@]}"} "$asset" --version --json)"
python3 - "$version" "$commit" "$info" <<'PY'
import json, sys
version, commit, info = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
if (info.get("name"), info.get("version"), info.get("commit"), info.get("dirty")) != ("zipp", version, commit, False):
    sys.exit(f"binary identity does not match {version} at {commit}: {info}")
PY
echo "$asset"
