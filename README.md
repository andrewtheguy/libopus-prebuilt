# libopus-prebuilt

Static **libopus 1.6.1**, built once with cmake so that nothing which *links* it needs
cmake at all — plus a Rust crate that replaces `opus` + `audiopus_sys` and links it.

| target | library | CPU floor |
|---|---|---|
| `macos-arm64` | `libopus.a` | `apple-m1`, deployment target 11.0 |
| `linux-x86_64` | `libopus.a` | **x86-64-v3 / Coffee Lake** — AVX2+FMA unconditional |
| `linux-aarch64` | `libopus.a` | ARMv8-A, NEON unconditional |
| `windows-x86_64-msvc` | `opus.lib` | **`/arch:AVX2` / Coffee Lake**, dynamic CRT |

Every one of them compiles out opus's runtime CPU dispatch and calls the SIMD kernels
unconditionally, because every one names a floor that guarantees them.

## Using it from Rust

One line, and no source changes:

```toml
# was: opus = "0.3"
opus = { package = "opus-prebuilt", git = "https://github.com/andrewtheguy/libopus-prebuilt", tag = "v1.6.1-20260731-731adf1" }
```

(An example — use a tag that exists. See **Releasing** below for what the parts mean, and
note that no tag is pinned until the first release is published and `--pin`ned.)

`opus-prebuilt` sets `[lib] name = "opus"`, so every `use opus::…` and
`opus::Decoder::new` keeps compiling untouched. No cmake, no C compiler, no pkg-config,
no `LIBOPUS_*` environment variables anywhere — not in a Dockerfile, not in a packaging
script, not in CI.

Two crates are involved:

| crate | what it is |
|---|---|
| `libopus-prebuilt-sys` | the FFI, and a build script that finds the right archive and emits two link flags |
| `opus-prebuilt` | `opus` 0.3.1, verbatim, with its `extern crate audiopus_sys` line pointing at the above |
| `opus-e2e` | not a library: a consumer, written with the same dependency line you would use, built and *run* by the pipeline on all four targets |

Keeping the safe wrapper byte-identical to upstream is deliberate: the consuming projects
are written against *its* semantics, and this repository should have no opinion about
them. A future `opus` release is re-forked by copying its `lib.rs` and changing that one
line again.

### CPU floors

The x86_64 archives assume **Coffee Lake or newer** and call opus's SSE4.1 and AVX2
kernels unconditionally, with the runtime CPUID dispatch compiled out. Stated plainly, the
cost is that they execute an illegal instruction on anything without AVX2: pre-2013 Intel,
pre-Zen AMD, and — the one that surprises people — the Celeron and Pentium parts *of* the
Coffee Lake generation, where AVX2 is fused off.

That is the floor this repository was asked for, and there is deliberately no fallback
archive for anything below it. A dispatching build would be a second artifact to build,
test, release and reason about in order to serve machines nobody here targets; a project
that needs one should build its own libopus and point `LIBOPUS_PREBUILT_DIR` at it.

macOS needs no such choice. Every arm64 Mac is an M1 or later, so naming the M1 as the
floor costs no compatibility at all and buys ARMv8.4 with dotprod and fp16 over the
generic ARMv8-A the compiler assumes otherwise. NEON is unconditional there either way —
opus's cmake presumes it for any aarch64 target.

Two things this does *not* do, both on purpose: `-march=native` (the artifact runs on
machines other than the builder) and `-ffast-math` / `OPUS_FLOAT_APPROX` (it changes
libopus's arithmetic, and a project with checked-in Opus fixtures would notice).

### Where the archive comes from

`build.rs` looks in three places, in order:

1. `LIBOPUS_PREBUILT_DIR` — a prefix containing `lib/`, used as-is. The escape hatch for
   an unsupported target, and the way to build with no network whatsoever.
2. `crates/libopus-prebuilt-sys/prebuilt/<target>/` — what `./build.sh` +
   `./sync-prebuilt.sh` leave behind locally. Gitignored.
3. the GitHub release named in `prebuilt.sums`, downloaded once per machine into
   `$CARGO_HOME/libopus-prebuilt/` and **verified against the checksum committed in that
   file before it is unpacked**.

(3) is what makes a fresh clone of a consuming project build with nothing installed. The
cache living under `CARGO_HOME` means the many Docker builds that already cache
`~/.cargo` get it for free. To confirm which one was used:

```sh
cargo build -vv 2>&1 | grep 'cargo:info=libopus'
# cargo:info=libopus 1.6.1 linked statically from prebuilt/macos-arm64 (aarch64-apple-darwin)
# cargo:info=libopus cpu_floor apple-m1 (armv8.4, NEON unconditional)
```

### Still on `opus = "0.3"`

The archives are also consumable the old way — unpack one and point `audiopus_sys` at the
prefix — which is what a project not yet migrated is already doing:

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

`OPUS_LIB_DIR` / `OPUS_STATIC` / `OPUS_NO_PKG` are accepted as aliases by the same build
script. Note what this route still requires and the crate route does not: three variables
set correctly in every Dockerfile, packaging script and CI job, and `audiopus_sys` present
in the dependency tree — which is cmake in the tree, whether or not it gets used.

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

Building it here once fixes all three, and adds two things none of the per-project builds
had: a named CPU floor, and **SIMD paths that are asserted rather than assumed**.

## What is verified, and why each check exists

`build.sh` refuses to package an archive that fails either check:

```
>> verifying the SIMD objects made it in
   17 neon symbol references
>> verifying the CPU floor
   1722 AVX2/FMA instructions
   floor: x86-64-v3 / Coffee Lake (AVX2+FMA unconditional)
```

- **The SIMD paths.** opus's cmake detects the architecture, compiles the intrinsics it
  finds support for, and wires up runtime dispatch. Reproducing that by hand is how you
  end up silently shipping the scalar fallbacks, and "audio is slower than it should be"
  is about the hardest regression there is to notice. This is also why the repo builds
  with cmake rather than a hand-rolled `cc` script. Checked with `nm` on Unix, and with
  the compiled object file names on Windows, where `nm` is not on the runner's PATH.
- **The CPU floor.** A library that quietly kept its dispatch is merely slower; one that
  quietly *lost* it executes an illegal instruction in the field, on whoever runs the
  oldest machine. So the claim in the MANIFEST is checked against the CMake cache and,
  where a disassembler can read the format, against the instructions themselves.

The Rust half is checked too, in three layers, because the risk there is not in the safe
wrapper — that is upstream's code, covered by upstream's tests — but underneath it:

- `src/consts.rs` is **generated** from the pinned headers by `gen-consts.sh`, since
  every one of those ~90 values is a bare integer where a typo is not a compile error but
  a CTL that configures a different parameter than the one named. (Cross-checked once
  against `audiopus_sys`'s bindings: 73 names in common, zero disagreements.)
- `./sync-prebuilt.sh --check` proves the committed headers are byte-identical to the
  pinned tarball's and that `consts.rs` matches them. CI runs it before anything compiles.
- `crates/opus-prebuilt/tests/prebuilt.rs` sets and reads back every CTL the consuming
  projects touch, and encodes and decodes a sine wave. A wrong constant fails a test
  instead of producing bad audio months later.
- `crates/opus-e2e` is a **binary** that depends on the crate the way a project does,
  rename and all, and the pipeline builds and *runs* it on every target. Three things only
  a binary can show: that the archive links into a shippable executable in release mode,
  that the CPU floor is real — it runs on the runner's own processor, so an AVX2 archive
  that cannot execute is a failed pipeline rather than a support ticket — and that Windows
  works, which nothing else here can check. It exercises 48/16/8 kHz and mono/stereo,
  which is what moves libopus between its SILK, CELT and hybrid paths, and those paths are
  where the hand-written SIMD kernels live.
- `opus-e2e --digest` fingerprints what the *encoder produced*, and every release publishes
  those fingerprints as `ENCODER-DIGESTS`. That is the question behind "will my fixtures
  still match" — see **Bumping opus** — recorded per release so a future one can be compared
  against it without re-running anything.
- `./check-static.sh <binary>` then asks the question the name of this repository implies:
  is libopus *in* the binary (its version string is), and is there a dynamic dependency on
  one as well or instead (there must not be). That second one passes every test on the
  build machine and fails on a slim runtime image, which is the exact failure this repo
  exists to remove.

## Building and testing locally

```sh
./build.sh macos-arm64        # or any target from the table
./sync-prebuilt.sh            # make cargo link what you just built
cargo test --offline --workspace
cargo run --offline --release -p opus-e2e     # 35 checks, and the RMS of each round trip
./check-static.sh target/release/opus-e2e
```

`./build.sh` needs cmake and a C toolchain; nothing downstream of it does. The source
tarball is fetched once into `build/` and **verified against the SHA-256 in `opus.env`
before it is unpacked** (`source.sh`), so bytes that are not the pinned release never
reach a compiler. Output lands in `dist/<target>/` with a `MANIFEST` recording the
version, the checksum, the CPU floor, the flags and the SIMD evidence.

**Linux** is tested in Docker:

```sh
./test-docker.sh              # every Linux target this machine can run
./test-docker.sh linux-x86_64
```

On Apple silicon `linux/arm64` runs natively and `linux/amd64` is emulated. If an
emulated run dies with SIGILL on the AVX2 archive, suspect the emulator before the
artifact — Rosetta does not implement AVX2, QEMU does.

**Windows** cannot be built or tested anywhere else here, so the workflow's manual trigger
is its test loop:

```sh
gh workflow run build.yml -f targets=windows      # or: all, x86_64, arm64
gh run watch
```

That builds both Windows archives, links the crates against them, runs the test suite, and
then builds and runs `opus-e2e` on the runner — which is the only place `opus_encode` and
`opus_decode` from an MSVC `opus.lib` are ever actually executed.

### Which workflow does what

| workflow | trigger | what it does |
|---|---|---|
| `ci.yml` | every push and PR | shellcheck; parses every script under macOS's stock **bash 3.2**; builds and tests `linux-x86_64` by *calling* `build.yml`; and builds as a consumer does, with an empty `prebuilt/`, to exercise the download-and-verify path nothing else takes |
| `build.yml` | manual, or called | the four-target matrix: build, test, e2e binary, `check-static.sh`. Cannot publish — its token is read-only |
| `release.yml` | manual | calls `build.yml` for all four, then packages, uploads and publishes draft-first |

The split is about what changes commit to commit. The archives change only when `opus.env`
or `build.sh` does, so rebuilding four of them on every push would be four runners proving a
pinned tarball is still pinned. The crates and the scripts change constantly — and for a
while nothing checked them unless somebody remembered to press a button, which is how a
bash 3.2 incompatibility reached a commit.

## ABI notes, which are the real maintenance cost

Not opus versions — the pin only moves when you move it. These:

- **macOS**: built with a deployment target of 11.0, lower than anything likely to link
  it. A static library built for a *newer* minimum than its consumer produces link
  warnings now and confusion later. arm64 only: there is no Intel Mac artifact.
- **Linux**: built against glibc. libopus barely touches libc so this generally links
  into musl binaries too, but a fully static musl build wants a musl-built copy.
- **Windows**: built with the **dynamic** CRT (`/MD`), which is what Rust's MSVC targets
  use. A `/MT` library against a `/MD` binary is the classic mismatched-CRT link error.
- **PIC** is on everywhere, so the archive links into shared objects as well as
  executables.

## Releasing

Two workflows, deliberately not one. *Build libopus* builds and tests and never publishes;
*Release libopus archives* publishes, and does the building by **calling** the first one —
so what gets released is what passed, run by the same code rather than a copy of it.

```
gh workflow run release.yml     # or the Actions tab. No tag to type.
./sync-prebuilt.sh --pin <tag>  # then commit prebuilt.sums
```

Tags are computed, never typed: `v<opus version>-<YYYYMMDD>-<short sha>`, e.g.
`v1.6.1-20260731-731adf1`. The version says what is inside, the UTC date says when, and the
hash says which commit — and since the workflow derives all three from the tree it is
building, none of them can disagree with it. That is also why there is nothing to validate:
the failure mode of a hand-typed tag is a tag that lies, and the way to fix it is to stop
typing tags. Releasing twice from one commit on one day is refused, because it would be the
same tag.

The order is draft first, publish last, and the reason is worth knowing: **a draft release
does not create the git tag.** Four builds, their tests, the e2e binaries, packaging and upload all happen while the tag still does not exist, and it
comes into being only when the draft is published as the final step. A release that fails
half way leaves a draft somebody can delete rather than a tag pointing at archives nobody
should link. A run from any branch but the default one is marked pre-release.

Assets are the four archives, `SHA256SUMS` over all of them, and `ENCODER-DIGESTS` — what
each archive's encoder produced for a fixed input, for projects with pinned Opus fixtures.
The release refuses to publish if any of the four is missing, because a release short an
archive is discovered later, by somebody else, on the platform nobody tested.

`--pin` reads the release's own `SHA256SUMS`, so it runs after the release exists; that is
what turns the published bytes into something a consumer's `build.rs` can verify.

## Bumping opus

Edit `opus.env` — the version and its checksum, which is the only place either lives:

```sh
curl -sSLO https://downloads.xiph.org/releases/opus/opus-<version>.tar.gz
shasum -a 256 opus-<version>.tar.gz
./sync-prebuilt.sh --headers          # re-extract the headers, regenerate consts.rs
```

Then tag. One thing worth knowing before a bump: opus keeps its bitstream and its ABI
stable, and 1.6.1 was verified against a consumer whose checked-in Opus fixtures were
generated by the 1.3-era vendored copy — they still matched byte for byte. That is a good
sign, not a guarantee; a project with pinned fixtures should re-run its own encoder tests.

The honest state of this, after getting it wrong once: the *archives* are reproducible —
two runs of the same commit produced byte-identical `libopus.a` files — but a digest is a
property of the whole binary, and the first version of this measurement generated its input
with `f64::sin`. On Linux that is glibc's, which **IFUNC-dispatches** on the CPU it finds,
so the same binary against the same archive produced two different digests on two runners.
The measurement moved, not the library.

`fixed_input` in `opus-e2e` now builds the input from integers alone — a triangle wave plus
an xorshift sequence, identical on every machine — so the value depends on the archive and
on the kernels the CPU executes, and not on libm. The digests published with a release
before that change are not reproducible and should not be compared against.

What that leaves for a project with pinned Opus fixtures: compare `ENCODER-DIGESTS` between
two releases of this repository, per target. Equal digests mean the encoder emits the same
bytes and your fixtures still match; different ones mean regenerate them. Do not compare
across architectures — x86_64 and aarch64 run different hand-written SIMD kernels over float
data and have never agreed, here or in the `audiopus_sys` build.

One caveat about measuring it yourself: a digest taken under emulation is not the hardware
answer either. `./test-docker.sh` on Apple silicon runs x86_64 under QEMU, whose FMA
rounding is not bit-accurate. Trust the pipeline's numbers over a local emulated one.

## Licensing

- libopus itself: BSD-3-Clause, Xiph.Org. The pinned tarball's `COPYING` applies to the
  archives and to the committed headers.
- `crates/opus-prebuilt/src/lib.rs`: MIT OR Apache-2.0, © 2016 Tad Hardesty — the `opus`
  crate, with one line changed. Both licence texts are in that directory.
- Everything else here: same terms as libopus.

`audiopus_sys` itself is *not* vendored; the FFI in `libopus-prebuilt-sys` is written from
the opus headers. Its constant values were compared against `audiopus_sys`'s bindings once
as a cross-check, which is a fact about testing rather than a derivation.
