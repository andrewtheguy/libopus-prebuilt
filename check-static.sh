#!/usr/bin/env bash
# Assert that a binary carries libopus inside it rather than expecting to find one.
#
#   ./check-static.sh target/release/opus-e2e
#
# Two questions, and both matter, because they fail in opposite directions:
#
#   positive — is libopus actually *in* there? The version string it reports at runtime is
#             compiled into the archive, so finding it in the file's bytes says yes.
#   negative — is there a *dynamic* dependency on libopus as well or instead? This is the
#             one that passes every test on the build machine and then fails on a slim
#             runtime image or a machine without Homebrew, which is precisely the failure
#             this repository exists to remove.
#
# Run in CI on every target. A binary that links a system libopus by accident behaves
# identically to a correct one until it is copied somewhere else.
set -euo pipefail

bin="${1:?usage: ./check-static.sh <binary>}"
[ -f "$bin" ] || { echo "no such file: $bin" >&2; exit 1; }

. "$(dirname "${BASH_SOURCE[0]}")/opus.env"

fail=0

echo ">> $bin"

# `grep -a`: treat the executable as text. Portable to all three platforms, which `nm` and
# `strings` are not — Windows runners have neither.
if grep -aq "libopus ${OPUS_VERSION}" "$bin"; then
  echo "   ok    libopus ${OPUS_VERSION} is compiled in"
else
  echo "   FAIL  no 'libopus ${OPUS_VERSION}' string in the binary — is it really linked?" >&2
  fail=1
fi

case "$(uname -s)" in
  Darwin) deps="$(otool -L "$bin" | tail -n +2 || true)" ;;
  Linux)  deps="$(ldd "$bin" 2>/dev/null || true)" ;;
  *)
    # Windows, under Git Bash. `dumpbin /dependents` needs an MSVC environment this script
    # does not set up, so the PE import table is read the crude way: a DLL a binary imports
    # has its name stored, in ASCII, in the file. Enough to catch an accidental `opus.dll`
    # import, which is the mistake being looked for.
    deps="$(grep -aoiE '[a-z0-9_.-]*opus[a-z0-9_.-]*\.dll' "$bin" || true)"
    ;;
esac

if opus_deps="$(printf '%s\n' "$deps" | grep -i opus)" && [ -n "$opus_deps" ]; then
  echo "   FAIL  dynamic dependency on libopus:" >&2
  printf '           %s\n' "$opus_deps" >&2
  fail=1
else
  echo "   ok    no dynamic libopus dependency"
fi

exit "$fail"
