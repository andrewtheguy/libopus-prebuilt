# libopus-prebuilt

Static **libopus 1.6.1** for four targets, built once with cmake so that nothing which
*links* it needs cmake at all.

| target | library |
|---|---|
| `macos-arm64` | `libopus.a` (deployment target 11.0) |
| `linux-x86_64` | `libopus.a` |
| `linux-aarch64` | `libopus.a` |
| `windows-x86_64-msvc` | `opus.lib` (dynamic CRT) |

## Why this exists

Every Rust project using `opus` pulls `audiopus_sys`, which builds a **vendored copy of
opus with cmake on every clean build**. Three consequences, all of which showed up in
practice:

- **It breaks.** That vendored opus declares `cmake_minimum_required` below 3.5, and
  CMake 4 — which the GitHub macOS runners now ship — refuses to configure it outright.
  Every project has to know to pass `CMAKE_POLICY_VERSION_MINIMUM=3.5`, and finds out
  when a release fails rather than when it is written, because a developer machine on
  CMake 3 builds happily.
- **It is stale.** `audiopus_sys` 0.2.2 is edition 2018 and its vendored opus is
  1.3-era. Nothing about that improves while the crate is unmaintained, and a project
  cannot reach a newer opus without doing this.
- **It is duplicated.** Each project re-derives the same environment variables, and each
  runner recompiles the same C.

Building it here once fixes all three, and adds one thing none of the per-project builds
had: **the SIMD paths are asserted rather than assumed** (see below).

## Using it

Download the archive for your target, unpack it, and point `audiopus_sys` at the prefix:

```sh
export LIBOPUS_LIB_DIR=/path/to/libopus-1.6.1-macos-arm64
export LIBOPUS_STATIC=1
export LIBOPUS_NO_PKG=1
cargo build --release
```

All three matter, and each for its own reason:

| variable | why |
|---|---|
| `LIBOPUS_LIB_DIR` | the **prefix**, not the `lib/` directory — `audiopus_sys` appends `lib` itself (`build.rs:62`) |
| `LIBOPUS_STATIC` | without it, `default_library_linking()` links *dynamically* on gnu targets, and the binary then wants a `libopus.so.0` that a slim runtime image does not ship |
| `LIBOPUS_NO_PKG` | stops pkg-config finding a system libopus first — a Homebrew `libopus.dylib` linked into something you ship fails on every machine without Homebrew |

`OPUS_LIB_DIR` / `OPUS_STATIC` / `OPUS_NO_PKG` are accepted as aliases by the same
build script.

To check it took effect, look for the build script's own report and the absence of any
cmake output:

```
cargo:info=Linking Opus as static lib: /path/to/libopus-1.6.1-macos-arm64
```

## The SIMD assertion

opus's cmake detects the architecture, compiles the intrinsics files it finds support
for, and wires up runtime CPU dispatch (`OPUS_X86_MAY_HAVE_SSE4_1` and friends mean
"compile it and check at runtime"). That machinery working is the reason this repo builds
with cmake rather than a hand-rolled `cc` script: reproducing it by hand is how you end
up silently shipping the scalar fallbacks, and "audio is slower than it should be" is
about the hardest regression there is to notice.

So `build.sh` refuses to package a library without them:

```
>> verifying the SIMD objects made it in
   17 neon symbol references
```

`nm` on the archive for the Unix targets; the compiled object file names on Windows,
where `nm` is not on the runner's PATH. Either way, a build that lost its intrinsics
fails here instead of shipping.

## ABI notes, which are the real maintenance cost

Not opus versions — the pin only moves when you move it. These:

- **macOS**: built with `-mmacosx-version-min=11.0`, lower than anything likely to link
  it. A static library built for a *newer* minimum than its consumer produces link
  warnings now and confusion later.
- **Linux**: built against glibc. libopus barely touches libc so this generally links
  into musl binaries too, but a fully static musl build wants a musl-built copy.
- **Windows**: built with the **dynamic** CRT (`/MD`), which is what Rust's MSVC targets
  use. A `/MT` library against a `/MD` binary is the classic mismatched-CRT link error.
- **PIC** is on everywhere, so the archive links into shared objects as well as
  executables.

## Building locally

```sh
./build.sh macos-arm64          # or linux-x86_64, linux-aarch64, windows-x86_64-msvc
```

Needs cmake and a C toolchain. The source tarball is fetched once into `build/` and
**verified against the SHA-256 in `opus.env` before it is unpacked**, so bytes that are
not the pinned release never reach a compiler. Output lands in `dist/<target>/` with a
`MANIFEST` recording the version, the checksum, the flags and the SIMD evidence.

## Bumping opus

Edit `opus.env` — the version and its checksum, which is the only place either lives:

```sh
curl -sSLO https://downloads.xiph.org/releases/opus/opus-<version>.tar.gz
shasum -a 256 opus-<version>.tar.gz
```

Then tag; the workflow builds all four targets and publishes them with a `SHA256SUMS`.

One thing worth knowing before a bump: opus keeps its bitstream and its ABI stable, and
1.6.1 was verified against a consumer whose checked-in Opus fixtures were generated by
the 1.3-era vendored copy — they still matched byte for byte. That is a good sign, not a
guarantee; a project with pinned fixtures should re-run its own encoder tests.
