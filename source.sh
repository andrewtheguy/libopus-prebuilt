# shellcheck shell=bash
# Fetch, verify and unpack the pinned opus source. Sourced, not run — by build.sh, which
# compiles it, and by sync-prebuilt.sh, which takes the headers out of it.
#
# One copy of this rather than two, because it is where the repository's central promise
# lives: the checksum is checked *before* anything is unpacked, so bytes that are not the
# pinned release never reach a compiler, a header directory, or an artifact other
# projects link. Two implementations of that rule is one too many.

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    sha256sum "$1" | awk '{print $1}'
  fi
}

# Leaves the unpacked tree at build/opus-$OPUS_VERSION and echoes nothing; callers use
# that path directly. Idempotent apart from the unpack, which is redone every time so a
# tree someone poked at by hand cannot influence a build.
ensure_source() {
  local tarball="opus-${OPUS_VERSION}.tar.gz"
  local src="build/opus-${OPUS_VERSION}"

  mkdir -p build
  if [ ! -f "build/$tarball" ]; then
    echo ">> fetching $tarball"
    curl -sSL --fail --max-time 600 -o "build/$tarball.part" \
      "https://downloads.xiph.org/releases/opus/$tarball"
    mv "build/$tarball.part" "build/$tarball"
  fi

  echo ">> verifying $tarball"
  local actual
  actual="$(sha256_of "build/$tarball")"
  [ "$actual" = "$OPUS_SHA256" ] || {
    echo "checksum mismatch for $tarball" >&2
    echo "  expected $OPUS_SHA256" >&2
    echo "  actual   $actual" >&2
    return 1
  }

  rm -rf "$src"
  tar xzf "build/$tarball" -C build
}
