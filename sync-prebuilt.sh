#!/usr/bin/env bash
# Connect the shell half of this repo to the Rust half.
#
# Usage:
#   ./sync-prebuilt.sh              copy dist/* into the crate's prebuilt/ cache
#   ./sync-prebuilt.sh --headers    refresh the committed headers from the pinned source
#   ./sync-prebuilt.sh --check      verify the committed headers and constants
#   ./sync-prebuilt.sh --fetch      download the latest release's archives into prebuilt/
#
# Neither `prebuilt/` nor `dist/` is committed — see .gitignore. Two things *are*:
#
#   include/opus/   the opus headers, byte-identical to the pinned tarball's. Text, small,
#                   and reviewable — the opposite of a committed `.a` on every count.
#   src/consts.rs   generated *from* those headers by the crate's gen-consts.sh.
#
# Which is a chain: opus.env pins a checksum, the checksum gates the tarball, the tarball
# is where the headers come from, and the headers are where the constants come from. It
# holds only if every link is checked, so `--check` checks all of them and CI runs it.
#
# There is nothing here to pin a release with. build.rs fetches from the repository's
# latest release, so publishing one is the whole of releasing — no follow-up commit
# restating what GitHub already serves.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
# shellcheck source=opus.env
. ./opus.env
# shellcheck source=source.sh
. ./source.sh

crate=crates/libopus-prebuilt-sys
prebuilt="$crate/prebuilt"

# Every target build.sh knows how to make.
targets=(macos-arm64 linux-x86_64 linux-aarch64 windows-x86_64-msvc)

case "${1:-}" in
  --headers | --check)
    # Both modes need the pinned tree, and getting it goes through the same checksum gate
    # build.sh uses — headers that were never verified would make the generated constants
    # unverified too, and those are what a consumer's CTL calls actually send to libopus.
    ensure_source
    src="build/opus-${OPUS_VERSION}/include"

    if [ "$1" = "--headers" ]; then
      rm -rf "$crate/include/opus"
      mkdir -p "$crate/include/opus"
      # `*.h` only. The tarball's include/ also holds a meson.build, which is opus's build
      # system and not part of its interface.
      cp "$src"/*.h "$crate/include/opus/"
      # opus's own licence and authors, next to opus's own headers, from the same verified
      # tarball. Committing somebody's source without their licence text is not a thing to
      # do by omission.
      cp "build/opus-${OPUS_VERSION}/COPYING" "build/opus-${OPUS_VERSION}/AUTHORS" \
         "$crate/include/"
      echo ">> $crate/include/opus is now opus $OPUS_VERSION's headers"
      (cd "$crate" && ./gen-consts.sh)
      exit 0
    fi

    echo ">> comparing the committed headers against opus $OPUS_VERSION"
    # Staged into a directory of nothing but headers, then compared as directories: that
    # way one `diff -r` covers changed, missing *and* extra files. A header opus deleted
    # matters as much as one it edited, because consts.rs is generated from whatever is
    # sitting here.
    staged="$(mktemp -d)"
    trap 'rm -rf "$staged"' EXIT
    cp "$src"/*.h "$staged/"
    if diff -r "$staged" "$crate/include/opus" >/dev/null 2>&1; then
      echo "   $(find "$staged" -name '*.h' | wc -l | tr -d ' ') headers, byte-identical"
    else
      echo "the committed headers are not opus $OPUS_VERSION's — run --headers" >&2
      diff -r "$staged" "$crate/include/opus" | head -30 >&2
      exit 1
    fi
    (cd "$crate" && ./gen-consts.sh --check)
    ;;

  --fetch)
    # For working offline afterwards, or for testing a target this machine cannot build.
    # Takes whatever the latest release holds, which is the same thing build.rs would fetch.
    for target in "${targets[@]}"; do
      asset="libopus-${OPUS_VERSION}-${target}.tar.gz"
      echo ">> $asset"
      tmp="$(mktemp -d)"
      curl -sSL --fail --max-time 300 -o "$tmp/$asset" \
        "https://github.com/$PREBUILT_REPO/releases/latest/download/$asset"
      rm -rf "${prebuilt:?}/${target:?}"
      mkdir -p "$prebuilt/$target"
      tar xzf "$tmp/$asset" -C "$prebuilt/$target"
      rm -rf "$tmp"
    done
    ;;

  "")
    # The local loop: whatever ./build.sh has produced becomes what cargo links, with no
    # release and no network in the picture at all.
    [ -d dist ] || { echo "nothing in dist/ — run ./build.sh <target> first" >&2; exit 1; }
    found=0
    for dir in dist/*/; do
      target="$(basename "$dir")"
      [ -f "$dir/MANIFEST" ] || continue
      rm -rf "${prebuilt:?}/${target:?}"
      mkdir -p "$prebuilt"
      cp -R "$dir" "$prebuilt/$target"
      echo ">> $target ($(sed -n 's/^cpu_floor //p' "$dir/MANIFEST"))"
      found=$((found + 1))
    done
    [ "$found" -gt 0 ] || { echo "no built targets in dist/" >&2; exit 1; }
    echo ">> $found target(s) in $prebuilt — cargo will use these before any release"
    ;;

  *)
    sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit 1
    ;;
esac
