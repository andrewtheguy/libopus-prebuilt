#!/usr/bin/env bash
# Build the Linux archives and run the Rust test suite against them, in Docker.
#
# Usage:
#   ./test-docker.sh                 # every Linux target this machine can run
#   ./test-docker.sh linux-aarch64   # just one
#
# Why Docker and not CI: the Linux artifacts are the ones a developer on a Mac cannot
# otherwise touch, and they are also the ones most likely to be wrong — two CPU floors,
# a glibc to link against, and an `.a` that has to satisfy a linker nobody here runs.
# Ten minutes locally beats finding out on a tag.
#
# On Apple silicon, `linux/aarch64` runs natively and is fast; `linux/amd64` is emulated
# and is not. Read the note under "emulation" below before believing a failure there.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here"

image=libopus-prebuilt-test

# target : docker platform. The x86_64 pair differ only in CPU floor, and testing both
# is the point: the baseline archive is the fallback for machines the default one will
# not run on, so "the fallback still works" is a claim worth checking.
targets=(
  "linux-aarch64:linux/arm64"
  "linux-x86_64:linux/amd64"
  "linux-x86_64-baseline:linux/amd64"
)

want="${1:-}"
if [ -n "$want" ]; then
  # shellcheck disable=SC2076
  [[ " ${targets[*]} " =~ " $want:" ]] || {
    echo "not a Linux target: $want" >&2
    printf '  %s\n' "${targets[@]%%:*}" >&2
    exit 1
  }
fi

# cmake for build.sh, and that is the only reason it is in here — nothing in the *Rust*
# half needs it, which is the property this whole repository exists to give consumers.
#
# Once per platform, and tagged per platform: an image built for arm64 cannot be run with
# `--platform linux/amd64`, and docker's response to being asked is to try to *pull* the
# tag, which fails with a confusing authentication error.
build_image() {
  local platform="$1" tag="$image:${1##*/}"
  docker image inspect "$tag" >/dev/null 2>&1 && return 0
  echo ">> building the test image for $platform"
  docker build -q --platform "$platform" -t "$tag" -f - . <<'DOCKERFILE' >/dev/null
FROM rust:1-bookworm
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake binutils \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /work
DOCKERFILE
}

status=0
for entry in "${targets[@]}"; do
  target="${entry%%:*}"
  platform="${entry##*:}"
  [ -z "$want" ] || [ "$want" = "$target" ] || continue

  echo
  echo "=============== $target ($platform)"
  build_image "$platform" || {
    status=1
    echo "--- $target SKIPPED: no image for $platform" >&2
    echo "    On Apple silicon, linux/amd64 needs the qemu binfmt handler that Docker" >&2
    echo "    Desktop installs; the GitHub Actions run covers this target either way." >&2
    continue
  }
  # Without this, a `-baseline` run tests the *other* archive: dist/ is shared with the
  # earlier runs, sync-prebuilt.sh copies all of it, and cargo's default choice on x86_64
  # is the AVX2 archive. The feature is how a consumer selects the baseline one, so it is
  # also the only way to test that the selection works.
  features=""
  case "$target" in
    *-baseline) features="--features opus-prebuilt/x86-baseline" ;;
  esac

  # A per-platform CARGO_TARGET_DIR, because the host's target/ holds macOS artifacts and
  # cargo would rebuild the world on every switch — or worse, try to reuse them. `build/`
  # and `dist/` are already per-target, so those are safe to share, and sharing them means
  # the opus tarball is downloaded once for all of this.
  if docker run --rm --platform "$platform" \
      -v "$here:/work" \
      -e CARGO_TARGET_DIR="/work/target/docker-${platform##*/}" \
      "$image:${platform##*/}" bash -euo pipefail -c "
        ./build.sh $target
        ./sync-prebuilt.sh
        # --offline proves the point of the exercise: after sync-prebuilt.sh there is
        # nothing left to fetch, so a consumer's build needs no network either.
        cargo test --offline --workspace $features
        # The same end-to-end leg the pipeline runs: a release binary, executed, then
        # checked for a dynamic libopus dependency it must not have.
        cargo build --offline --release --workspace $features
        ./target/docker-${platform##*/}/release/opus-e2e | tail -3
        ./check-static.sh ./target/docker-${platform##*/}/release/opus-e2e
        echo
        cat dist/$target/MANIFEST
      "; then
    echo "--- $target OK"
  else
    code=$?
    status=1
    echo "--- $target FAILED (exit $code)" >&2
    # emulation: 132 is SIGILL. On Apple silicon with Rosetta handling linux/amd64,
    # AVX2 is unimplemented, so the *correctly built* Coffee Lake archive dies here
    # while the baseline one passes. That is the emulator's limit, not a bad artifact —
    # confirm on a real x86_64 machine, or in the GitHub Actions run, before chasing it.
    if [ "$code" = 132 ] && [ "$platform" = linux/amd64 ]; then
      echo "    SIGILL under emulation. If $target is the AVX2 build, the emulator is" >&2
      echo "    the likely cause: Rosetta does not implement AVX2. Disable 'Use Rosetta" >&2
      echo "    for x86/amd64' in Docker Desktop to fall back to QEMU, which does." >&2
    fi
  fi
done

exit "$status"
