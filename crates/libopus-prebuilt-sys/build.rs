// Find the prebuilt libopus for this target and emit the link flags. What this build
// script does *not* do is the point of the crate: no cmake, no C compiler, no
// pkg-config, no vendored source tree. Compare `audiopus_sys`, which configures and
// compiles opus here on every clean build.
//
// Three places an archive can come from, tried in this order:
//
//   1. `LIBOPUS_PREBUILT_DIR` — a prefix you built or unpacked yourself. Used as-is.
//   2. `prebuilt/<target>/` next to this file — what `./build.sh` + `./sync-prebuilt.sh`
//      leave behind, and gitignored, because a committed `.a` is one nobody can tell
//      apart from the one CI made.
//   3. the GitHub release named in `prebuilt.sums`, downloaded once per machine into
//      `$CARGO_HOME/libopus-prebuilt/` and **verified against the checksum in that
//      file before it is unpacked** — the same rule `build.sh` applies to the opus
//      source tarball, for the same reason. Bytes nobody pinned never reach a linker.
//
// (3) is what makes a fresh clone of a consuming project build with nothing installed;
// (1) is what makes it work with no network at all.
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LIBOPUS_PREBUILT_DIR");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let target = std::env::var("TARGET").unwrap();
    let version = opus_env(&manifest, "OPUS_VERSION");
    println!("cargo:rustc-env=LIBOPUS_PREBUILT_VERSION={version}");

    let (prefix, provenance) = resolve(&manifest, &target, &version);
    let lib_dir = prefix.join("lib");
    let archive = if target.contains("windows-msvc") { "opus.lib" } else { "libopus.a" };
    assert!(
        lib_dir.join(archive).exists(),
        "no {archive} in {} (from {provenance})\n\nLIBOPUS_PREBUILT_DIR must name a \
         prefix *containing* lib/, not the lib/ directory itself.",
        lib_dir.display(),
    );

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=opus");
    // For a consumer compiling its own C against the same headers, via the
    // `DEP_OPUS_INCLUDE` that `links = "opus"` exposes.
    println!("cargo:include={}", manifest.join("include").display());
    println!("cargo:rerun-if-changed={}", lib_dir.join(archive).display());

    // `cargo:info`, not `cargo:warning`: this is the normal case, and a warning on every
    // build of every consumer is noise that teaches people to ignore warnings. Visible
    // under `cargo build -vv`, which is where the README says to look.
    println!("cargo:info=libopus {version} linked statically from {provenance} ({target})");
    if let Ok(text) = std::fs::read_to_string(prefix.join("MANIFEST")) {
        // The CPU floor, echoed into the log of the build that produced the binary —
        // which is where anyone debugging an illegal instruction on an old machine will
        // look first.
        for line in text.lines().filter(|l| l.starts_with("cpu_floor")) {
            println!("cargo:info=libopus {line}");
        }
    }
    if provenance == "LIBOPUS_PREBUILT_DIR" {
        // This one earns a warning: the archive came from outside the repo, so nothing
        // checked its version, its CPU floor, or that it has any SIMD in it.
        println!("cargo:warning=libopus from LIBOPUS_PREBUILT_DIR ({}) — unverified", prefix.display());
    }
}

fn resolve(manifest: &Path, target: &str, version: &str) -> (PathBuf, String) {
    if let Some(dir) = std::env::var_os("LIBOPUS_PREBUILT_DIR") {
        return (PathBuf::from(dir), "LIBOPUS_PREBUILT_DIR".into());
    }

    let name = prebuilt_dir(target);
    let local = manifest.join("prebuilt").join(name);
    if local.join("lib").is_dir() {
        return (local, format!("prebuilt/{name}"));
    }

    let cached = cache_root().join(version).join(name);
    if cached.join("lib").is_dir() {
        return (cached, format!("cache/{version}/{name}"));
    }

    (fetch(manifest, name, version, &cached), format!("release asset for {name}"))
}

/// Download the release archive for one target, verify it, unpack it into the cache.
fn fetch(manifest: &Path, name: &str, version: &str, cached: &Path) -> PathBuf {
    let sums_path = manifest.join("prebuilt.sums");
    println!("cargo:rerun-if-changed={}", sums_path.display());
    let sums = std::fs::read_to_string(&sums_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", sums_path.display()));

    let asset = format!("libopus-{version}-{name}.tar.gz");
    let (release, want) = parse_sums(&sums, &asset).unwrap_or_else(|| {
        panic!(
            "{} has no checksum for {asset}. Either this target was never released, or \
             prebuilt.sums is stale — run ./sync-prebuilt.sh --pin <tag>. To build \
             without the release, run ./build.sh {name} && ./sync-prebuilt.sh, or point \
             LIBOPUS_PREBUILT_DIR at your own prefix.",
            sums_path.display(),
        )
    });

    assert!(
        std::env::var("CARGO_NET_OFFLINE").as_deref() != Ok("true"),
        "libopus for {name} is not cached and cargo is offline. Run ./build.sh {name} && \
         ./sync-prebuilt.sh, or set LIBOPUS_PREBUILT_DIR to a prefix containing lib/."
    );

    let repo = opus_env(manifest, "PREBUILT_REPO");
    let url = format!("https://github.com/{repo}/releases/download/{release}/{asset}");
    // Staged under a pid-suffixed name so two cargo builds racing here cannot read each
    // other's half-written tarball. The loser of the race throws its copy away below.
    let staging = cached.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).expect("cannot create the cache directory");
    let tarball = staging.join(&asset);

    println!("cargo:info=fetching {url}");
    run(Command::new("curl").args([
        "-sSL", "--fail", "--max-time", "300", "--retry", "3", "-o",
        tarball.to_str().unwrap(), &url,
    ]));

    let bytes = std::fs::read(&tarball).expect("the download vanished");
    let got = sha256_hex(&bytes);
    assert_eq!(
        got, want,
        "checksum mismatch for {asset}\n  expected {want}\n  actual   {got}\nRefusing to \
         unpack it. Nothing that is not the pinned artifact gets linked into a binary."
    );

    // `tar` rather than a Rust tar crate: it is present on macOS, on every Linux image
    // that can run cargo, and in System32 on Windows 10 1803 and later, and a build
    // dependency here would be one every consumer compiles.
    run(Command::new("tar").arg("xzf").arg(&tarball).arg("-C").arg(&staging));
    std::fs::remove_file(&tarball).ok();

    std::fs::create_dir_all(cached.parent().unwrap()).ok();
    if std::fs::rename(&staging, cached).is_err() {
        // Either another build populated the cache first — fine, use theirs — or the
        // rename genuinely failed, which the caller's `lib/` check will report.
        let _ = std::fs::remove_dir_all(&staging);
    }
    cached.to_path_buf()
}

/// `release <tag>` plus `<sha256>  <asset>` lines, which is the release's own
/// SHA256SUMS with the tag written above it.
fn parse_sums(sums: &str, asset: &str) -> Option<(String, String)> {
    let release = sums.lines().find_map(|l| l.strip_prefix("release "))?.trim();
    let want = sums.lines().find_map(|line| {
        let (sha, file) = line.split_once("  ")?;
        // The release writes `./libopus-…`; accept it with or without the prefix.
        (Path::new(file.trim()).file_name()? == asset).then(|| sha.trim().to_string())
    })?;
    Some((release.to_string(), want))
}

fn run(cmd: &mut Command) {
    let program = cmd.get_program().to_string_lossy().into_owned();
    match cmd.status() {
        Ok(status) if status.success() => {}
        Ok(status) => panic!("{program} failed: {status}"),
        Err(e) => panic!("cannot run {program}: {e}"),
    }
}

/// `$CARGO_HOME/libopus-prebuilt/`, so the download happens once per machine rather
/// than once per project — and so the many Docker builds that already cache
/// `~/.cargo` get it for free with no extra configuration.
fn cache_root() -> PathBuf {
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".cargo")))
        .expect("neither CARGO_HOME nor a home directory is set");
    home.join("libopus-prebuilt")
}

/// The repo's target names are not Rust triples — they name *artifacts*, and there are
/// more of them than triples, because x86_64 has two CPU floors.
fn prebuilt_dir(target: &str) -> &'static str {
    // Coffee Lake by default on x86_64, which is what the projects consuming this asked
    // for. `x86-baseline` trades AVX2 for running on any x86_64 — which includes,
    // unintuitively, the Celeron and Pentium parts *of* the Coffee Lake generation,
    // where AVX2 is fused off.
    let baseline = cfg!(feature = "x86-baseline");
    match target {
        "aarch64-apple-darwin" => "macos-arm64",
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => {
            if baseline { "linux-x86_64-baseline" } else { "linux-x86_64" }
        }
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => "linux-aarch64",
        "x86_64-pc-windows-msvc" => {
            if baseline { "windows-x86_64-msvc-baseline" } else { "windows-x86_64-msvc" }
        }
        "x86_64-apple-darwin" => panic!(
            "no prebuilt libopus for Intel macOS: the macOS artifact is arm64, tuned for \
             apple-m1. Set LIBOPUS_PREBUILT_DIR to a prefix holding your own libopus.a, \
             or add the target to build.sh."
        ),
        other => panic!(
            "no prebuilt libopus for {other}. Supported: aarch64-apple-darwin, \
             x86_64-unknown-linux-{{gnu,musl}}, aarch64-unknown-linux-{{gnu,musl}}, \
             x86_64-pc-windows-msvc. Set LIBOPUS_PREBUILT_DIR to a prefix holding your \
             own libopus.a for anything else."
        ),
    }
}

/// Read one setting out of `opus.env`, which is where the shell build keeps the same
/// values — parsed rather than duplicated, so the two halves of this repository cannot
/// disagree about which opus this is or where its archives live.
fn opus_env(manifest: &Path, key: &str) -> String {
    let env_file = manifest.join("../../opus.env");
    println!("cargo:rerun-if-changed={}", env_file.display());
    let text = std::fs::read_to_string(&env_file).unwrap_or_else(|e| {
        panic!("cannot read {}: {e} — is this crate outside its repository?", env_file.display())
    });
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no {key} in opus.env"))
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------- sha256
//
// Written out rather than pulled in, because a build-dependency is something every
// consumer of this crate compiles, and this is the only thing we would want from one.
// FIPS 180-4; the self-test in `sha256_hex` is what keeps that claim honest.

fn sha256_hex(bytes: &[u8]) -> String {
    // Known-answer test on every call. It costs a few microseconds and it means a
    // mistake in the code below can never quietly turn checksum verification into a
    // formality that passes whatever it is given.
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    if bytes != b"abc" {
        assert_eq!(sha256_hex(b"abc"), ABC, "the sha256 implementation is broken");
    }

    let mut hex = String::with_capacity(64);
    for byte in sha256(bytes) {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

fn sha256(message: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Message, 0x80, zero padding to 56 mod 64, then the bit length big-endian.
    let mut padded = message.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&(message.len() as u64 * 8).to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }

    let mut out = [0u8; 32];
    for (chunk, word) in out.chunks_exact_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}
