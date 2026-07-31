#!/usr/bin/env bash
# Build one static libopus, with the SIMD paths verified rather than assumed.
#
# Usage:
#   ./build.sh <target>
#
# Targets:
#   macos-arm64            libopus.a   (deployment target 11.0)
#   linux-x86_64           libopus.a   (SSE/SSE2 baseline, SSE4.1+AVX2 at runtime)
#   linux-aarch64          libopus.a   (NEON baseline)
#   windows-x86_64-msvc    opus.lib    (dynamic CRT, /MD — see README)
#
# Output: dist/<target>/{lib,include}/… plus a MANIFEST naming the version, the
# checksum, the flags and the SIMD objects that ended up in the archive.
#
# **cmake, deliberately.** opus's own build is what detects the architecture, decides
# which intrinsics files to compile and wires up runtime CPU dispatch; a hand-rolled
# `cc` build has to reproduce all of that faithfully or it quietly compiles the scalar
# fallbacks and nobody notices until the audio path is slow. Building here with cmake
# once is exactly what frees every *consumer* from needing cmake at all.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"
# shellcheck source=opus.env
. ./opus.env

target="${1:-}"
[ -n "$target" ] || {
  sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit 1
}

tarball="opus-${OPUS_VERSION}.tar.gz"
src="build/opus-${OPUS_VERSION}"
out="$here/dist/$target"

# ---------------------------------------------------------------- source

mkdir -p build
if [ ! -f "build/$tarball" ]; then
  echo ">> fetching $tarball"
  curl -sSL --fail --max-time 600 -o "build/$tarball.part" \
    "https://downloads.xiph.org/releases/opus/$tarball"
  mv "build/$tarball.part" "build/$tarball"
fi

echo ">> verifying $tarball"
# Before unpacking, not after: the point of a pinned checksum is that unexpected bytes
# never reach a compiler, let alone an artifact other projects link.
if command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "build/$tarball" | awk '{print $1}')"
else
  actual="$(sha256sum "build/$tarball" | awk '{print $1}')"
fi
[ "$actual" = "$OPUS_SHA256" ] || {
  echo "checksum mismatch for $tarball" >&2
  echo "  expected $OPUS_SHA256" >&2
  echo "  actual   $actual" >&2
  exit 1
}

rm -rf "$src"
tar xzf "build/$tarball" -C build

# ---------------------------------------------------------------- configure

# Common to every target. `OPUS_DISABLE_INTRINSICS=OFF` is the default and is stated
# anyway: it is the one switch that would silently turn this into a scalar build, and
# the verification step below exists to catch exactly that.
cmake_args=(
  -DCMAKE_BUILD_TYPE=Release
  -DBUILD_SHARED_LIBS=OFF
  -DOPUS_BUILD_SHARED_LIBRARY=OFF
  -DOPUS_DISABLE_INTRINSICS=OFF
  -DOPUS_BUILD_PROGRAMS=OFF
  -DOPUS_BUILD_TESTING=OFF
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON
  # Rust links this into binaries that must run on machines other than the builder,
  # so nothing may be tuned to the host CPU. Runtime dispatch handles the rest.
  -DOPUS_STACK_PROTECTOR=OFF
)

# The escape hatch CMake's own error names, kept for the day a runner ships a CMake
# that has dropped compatibility with whatever `cmake_minimum_required` this opus
# declares. Inert on any CMake that predates the variable.
cmake_args+=(-DCMAKE_POLICY_VERSION_MINIMUM=3.5)

lib_name=libopus.a
case "$target" in
  macos-arm64)
    cmake_args+=(
      -DCMAKE_OSX_ARCHITECTURES=arm64
      # Lower than any consumer targets. A static library built for a *newer* minimum
      # than the binary linking it is a link-time warning today and a support call
      # later; there is no cost to building older.
      -DCMAKE_OSX_DEPLOYMENT_TARGET=11.0
    )
    ;;
  linux-x86_64 | linux-aarch64)
    # Nothing arch-specific to pass: opus's cmake detects the compiler's SIMD support
    # and turns on the `MAY_HAVE` options, which compile the intrinsics *and* the
    # runtime CPU checks that choose between them.
    ;;
  windows-x86_64-msvc)
    lib_name=opus.lib
    # Rust's MSVC targets link the dynamic CRT, and a static library built against the
    # static one fails to link with the mismatch that costs everybody an afternoon.
    cmake_args+=(-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL)
    ;;
  *)
    echo "unknown target: $target" >&2
    exit 1
    ;;
esac

echo ">> configuring opus ${OPUS_VERSION} for $target"
cmake -S "$src" -B "build/$target" "${cmake_args[@]}"

echo ">> building"
cmake --build "build/$target" --config Release --parallel

# ---------------------------------------------------------------- collect

archive="$(find "build/$target" -name "$lib_name" -type f | head -1)"
[ -n "$archive" ] || {
  echo "no $lib_name was produced" >&2
  exit 1
}

rm -rf "$out"
mkdir -p "$out/lib" "$out/include/opus"
cp "$archive" "$out/lib/$lib_name"
# `audiopus_sys` only needs the library, but a consumer that generates its own
# bindings needs the headers, and they must be the ones this archive was built from.
cp "$src"/include/*.h "$out/include/opus/"

# ---------------------------------------------------------------- verify

# The assertion that makes this repo worth having. A build that lost its intrinsics is
# indistinguishable from a good one at every level except speed, which is the hardest
# kind of regression to notice — so it fails here instead.
echo ">> verifying the SIMD objects made it in"
case "$target" in
  macos-arm64 | linux-aarch64) want='neon' ;;
  *) want='sse|avx' ;;
esac

if [ "$target" = "windows-x86_64-msvc" ]; then
  # `nm` is not on a Windows runner's PATH and `lib /list` needs an MSVC environment
  # this script does not set up. The compiled object files answer the same question one
  # step earlier: if cmake decided against the intrinsics, these were never built.
  found="$(find "build/$target" -name '*.obj' | grep -ciE "$want" || true)"
  evidence="object files"
else
  found="$(nm "$out/lib/$lib_name" 2>/dev/null | grep -ciE "$want" || true)"
  evidence="symbol references"
fi
[ "${found:-0}" -gt 0 ] || {
  echo "no $want $evidence in $lib_name — a scalar build, refusing to ship it" >&2
  exit 1
}
echo "   $found $want $evidence"

{
  echo "opus $OPUS_VERSION"
  echo "target $target"
  echo "sha256(source) $OPUS_SHA256"
  echo "library lib/$lib_name"
  echo "simd_evidence $found $evidence"
  echo "cmake_args ${cmake_args[*]}"
} > "$out/MANIFEST"

echo ">> wrote $out"
cat "$out/MANIFEST"
