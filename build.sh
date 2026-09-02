#!/usr/bin/env bash
# Build one static libopus, with the SIMD paths verified rather than assumed.
#
# Usage:
#   ./build.sh <target>
#
# Targets:
#   macos-arm64            libopus.a   (Apple silicon, deployment target 11.0)
#   linux-x86_64           libopus.a   (x86-64 baseline; SSE4.1/AVX2 kernels dispatched at run time)
#   linux-aarch64          libopus.a   (NEON, ARMv8-A)
#   windows-x86_64-msvc    opus.lib    (x86-64 baseline, dispatched the same way; dynamic CRT)
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
# shellcheck source=source.sh
. ./source.sh

target="${1:-}"
[ -n "$target" ] || {
  sed -n '2,/^set -euo pipefail$/p' "$0" | sed '$d; s/^# \{0,1\}//'
  exit 1
}

src="build/opus-${OPUS_VERSION}"
out="$here/dist/$target"

# ---------------------------------------------------------------- source

# Fetched, checksummed and unpacked by source.sh, which sync-prebuilt.sh also uses to
# take the headers from — the checksum gate is one implementation, not two.
ensure_source

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
  -DOPUS_STACK_PROTECTOR=OFF
)

# Extra C flags, per target. Kept separate from `cmake_args` because they are the one
# thing here that decides *which machines the artifact runs on*, and the MANIFEST
# records them for exactly that reason.
#
# Note what these are not: `-march=native`. Nothing may be tuned to the *builder's*
# CPU, because the artifact is linked into binaries that run elsewhere. What each
# target does instead is name a **floor** — the oldest CPU the resulting library is
# allowed to require — and compile for that.
cflags=()

# Does the compiler take this flag? A build that silently ignores `-mcpu=apple-m1`
# produces a working library, just a slower one, which is the failure mode this repo
# exists to make loud.
cflag_supported() {
  echo 'int main(void){return 0;}' |
    ${CC:-cc} -Werror "$1" -x c - -o /dev/null >/dev/null 2>&1
}

# The escape hatch CMake's own error names, kept for the day a runner ships a CMake
# that has dropped compatibility with whatever `cmake_minimum_required` this opus
# declares. Inert on any CMake that predates the variable.
cmake_args+=(-DCMAKE_POLICY_VERSION_MINIMUM=3.5)

# How the SIMD kernels are reached, recorded in the MANIFEST and asserted below. The arm
# targets *presume* NEON (`OPUS_PRESUME_NEON`): it is mandatory in ARMv8-A, so calling the
# kernels unconditionally costs no machine. The x86_64 targets keep opus's own runtime
# *dispatch* (`OPUS_X86_MAY_HAVE_*`, the upstream default): the SSE4.1 and AVX2 kernels are
# compiled, each file with its own flags, and `opus_select_arch()` picks them by CPUID —
# so one archive runs on any x86-64 and still uses AVX2 where the CPU has it.
simd_mode=presume
# Overwritten by every target below; the `*)` arm exits rather than falling through, so this
# value never reaches a MANIFEST. It says what it is anyway, because an empty string in a
# MANIFEST would look like a bug in the build rather than an unreachable default.
floor='unset'

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
    # Every arm64 Mac is an M1 or later — Apple never shipped another arm64 CPU — so
    # naming the M1 as the floor costs no compatibility at all, and buys the compiler
    # ARMv8.4 with dotprod and fp16 instead of the generic ARMv8-A it assumes
    # otherwise. NEON itself is already unconditional here: opus's cmake sets
    # OPUS_PRESUME_NEON for any aarch64 target (cmake/OpusConfig.cmake:77).
    floor='apple-m1 (armv8.4, NEON unconditional)'
    if cflag_supported -mcpu=apple-m1; then
      cflags+=(-mcpu=apple-m1)
    else
      echo "   note: this compiler rejects -mcpu=apple-m1, building generic armv8-a" >&2
    fi
    ;;
  linux-x86_64)
    # No floor above the x86-64 baseline, and no `-march`. opus's cmake already does the
    # right thing here on its own: with `OPUS_X86_MAY_HAVE_SSE4_1` and `_AVX2` (both ON
    # by default) it compiles the intrinsics files with `-msse4.1` and `-mavx2 -mfma
    # -mavx` *per file* (CMakeLists.txt:515-530) and leaves the CPUID dispatch in, so a
    # Coffee Lake still runs the AVX2 NSQ and pitch kernels and an Ivy Bridge runs the
    # SSE4.1 ones instead of dying on the first `vfmadd`.
    #
    # What a `-march=x86-64-v3` floor bought on top of that was measured before it was
    # dropped: the scalar code autovectorized, and 60 s of 48 kHz stereo took 0.65% of a
    # core instead of 0.73%. That is the whole price of running everywhere, and the
    # consuming projects chose to pay it rather than ship a second archive.
    floor='x86-64 baseline (SSE4.1 and AVX2 kernels dispatched at run time)'
    simd_mode=dispatch
    ;;
  linux-aarch64)
    floor='armv8-a (NEON unconditional)'
    # No floor to *choose*. Unlike the Mac, arm64 Linux spans a decade of very different
    # cores, and NEON — the part opus actually hand-writes — is mandatory in ARMv8-A
    # and therefore already unconditional.
    ;;
  windows-x86_64-msvc)
    lib_name=opus.lib
    # Rust's MSVC targets link the dynamic CRT, and a static library built against the
    # static one fails to link with the mismatch that costs everybody an afternoon.
    cmake_args+=(-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL)
    # Same dispatch as linux-x86_64. On MSVC the MAY_HAVE path gives the AVX2 sources
    # `/arch:AVX2` per file (CMakeLists.txt:526) and everything else the default x86-64
    # code generation; only PRESUME_AVX2 would have made it global (CMakeLists.txt:547-549).
    floor='x86-64 baseline (SSE4.1 and AVX2 kernels dispatched at run time)'
    simd_mode=dispatch
    ;;
  *)
    echo "unknown target: $target" >&2
    exit 1
    ;;
esac

if [ ${#cflags[@]} -gt 0 ]; then
  # CMAKE_C_FLAGS lands *before* the per-target options opus sets, so this adds a
  # floor without displacing any of opus's own flags — including the `-msse4.1` and
  # `-mavx2` it attaches to individual intrinsics files.
  cmake_args+=("-DCMAKE_C_FLAGS=${cflags[*]}")
fi

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

case "$target" in
  windows-*)
    # `nm` is not on a Windows runner's PATH and `lib /list` needs an MSVC environment
    # this script does not set up. The compiled object files answer the same question one
    # step earlier: if cmake decided against the intrinsics, these were never built.
    found="$(find "build/$target" -name '*.obj' | grep -ciE "$want" || true)"
    evidence="object files"
    ;;
  *)
    found="$(nm "$out/lib/$lib_name" 2>/dev/null | grep -ciE "$want" || true)"
    evidence="symbol references"
    ;;
esac
[ "${found:-0}" -gt 0 ] || {
  echo "no $want $evidence in $lib_name — a scalar build, refusing to ship it" >&2
  exit 1
}
echo "   $found $want $evidence"

# And the second assertion: that the SIMD mode this target claims is the one it was
# actually built with. Getting this wrong is worse than getting the intrinsics wrong —
# a dispatch build that quietly *presumed* AVX2 SIGILLs on a machine without it, in the
# field, for whoever runs the oldest CPU, and one that quietly lost its kernels is merely
# slower. So the claim is checked rather than trusted.
echo ">> verifying the CPU floor"

case "$target:$simd_mode" in
  *x86_64*:dispatch)
    # The cache proves intent: MAY_HAVE on, PRESUME off, for both ISA levels. A PRESUME
    # that crept in through a cmake default would read ON here.
    for opt in OPUS_X86_MAY_HAVE_SSE4_1 OPUS_X86_MAY_HAVE_AVX2; do
      grep -q "^${opt}:BOOL=ON$" "build/$target/CMakeCache.txt" || {
        echo "$opt is not ON in the cache — the kernels were not compiled" >&2
        exit 1
      }
    done
    for opt in OPUS_X86_PRESUME_SSE4_1 OPUS_X86_PRESUME_AVX2; do
      grep -q "^${opt}:BOOL=OFF$" "build/$target/CMakeCache.txt" || {
        echo "$opt is not OFF in the cache — this would be an AVX2-only archive" >&2
        exit 1
      }
    done
    # The archive proves the result, where the tools exist (Unix only; see below): the
    # dispatcher itself is present, the AVX2 kernels are compiled, and no AVX2 or FMA
    # instruction sits outside a function named for it — the exact property a global
    # `-march` would break.
    if [ "$evidence" = "symbol references" ]; then
      # A count rather than `grep -q`: under `pipefail`, -q closing the pipe on its first
      # match makes nm die of SIGPIPE and the whole pipeline report failure.
      [ "$(nm "$out/lib/$lib_name" 2>/dev/null | grep -c ' T opus_select_arch$' || true)" -gt 0 ] || {
        echo "no opus_select_arch in $lib_name — the runtime dispatch was compiled out" >&2
        exit 1
      }
      if command -v objdump >/dev/null 2>&1; then
        leaked="$(objdump -d "$out/lib/$lib_name" 2>/dev/null |
          awk '/^[0-9a-f]+ <.*>:$/ { fn=$2 } /(vfmadd|vfnmadd|vpbroadcast|vpermd|vperm2i128|vinserti128|vextracti128)/ && fn !~ /avx2/ { print fn }' |
          sort -u)"
        [ -z "$leaked" ] || {
          echo "AVX2/FMA instructions outside the _avx2 kernels — a floor leaked in:" >&2
          echo "$leaked" | sed 's/^/    /' >&2
          exit 1
        }
        kernels="$(objdump -d "$out/lib/$lib_name" 2>/dev/null | grep -ciE 'vfmadd|vpbroadcast|vpermd' || true)"
        [ "${kernels:-0}" -gt 0 ] || {
          echo "no AVX2/FMA instructions in $lib_name — the AVX2 kernels are missing" >&2
          exit 1
        }
        echo "   $kernels AVX2/FMA instructions, all inside _avx2 kernels"
      fi
    fi
    ;;
  *x86_64*:presume)
    # Passed on the command line, so cmake wrote it to the cache verbatim; if opus's
    # `cmake_dependent_option` had refused it — no AVX2 support in the compiler, say —
    # the cache would read OFF and this build would be a runtime-dispatch build
    # wearing a Coffee Lake MANIFEST.
    for opt in OPUS_X86_PRESUME_SSE4_1 OPUS_X86_PRESUME_AVX2; do
      grep -q "^${opt}:BOOL=ON$" "build/$target/CMakeCache.txt" || {
        echo "$opt is not ON in the cache — the AVX2 floor did not take" >&2
        exit 1
      }
    done
    # The instructions themselves, where a disassembler that understands the format is
    # available. The cache only proves intent; `vfmadd` in the archive proves the compiler
    # acted on it. Unix only — a Windows runner with a stray MinGW objdump on PATH would
    # be asked to disassemble an MSVC `.lib`, and the answer to that is not evidence of
    # anything. What covers Windows instead is the workflow running `cargo test` against
    # the archive on the runner's own AVX2-capable CPU.
    if [ "$evidence" = "symbol references" ] && command -v objdump >/dev/null 2>&1; then
      fma="$(objdump -d "$out/lib/$lib_name" 2>/dev/null | grep -ciE 'vfmadd|vpbroadcast|vpermd' || true)"
      [ "${fma:-0}" -gt 0 ] || {
        echo "no AVX2/FMA instructions in $lib_name despite the AVX2 floor" >&2
        exit 1
      }
      echo "   $fma AVX2/FMA instructions"
    fi
    ;;
esac
echo "   floor: $floor"

cflags_note='(none)'
[ ${#cflags[@]} -eq 0 ] || cflags_note="${cflags[*]}"

echo ">> checksumming the archive"
# The library's own hash, not the tarball's. A .tar.gz is not reproducible — gzip stamps an
# mtime into its header — so the wrapper's checksum can only ever say "these are the bytes
# that were published". This one says something stronger and more useful: *this is the same
# library*, comparable across runs, machines and releases.
lib_sha="$(sha256_of "$out/lib/$lib_name")"
echo "   $lib_sha"

{
  echo "opus $OPUS_VERSION"
  echo "target $target"
  echo "sha256(source) $OPUS_SHA256"
  echo "sha256(library) $lib_sha"
  echo "library lib/$lib_name"
  echo "cpu_floor $floor"
  echo "simd_mode $simd_mode"
  echo "simd_evidence $found $evidence"
  echo "cflags $cflags_note"
  echo "cmake_args ${cmake_args[*]}"
} > "$out/MANIFEST"

echo ">> wrote $out"
cat "$out/MANIFEST"
