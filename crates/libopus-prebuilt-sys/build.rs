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
//   3. the repository's **latest** GitHub release, downloaded once per machine into
//      `$CARGO_HOME/libopus-prebuilt/`.
//
// (3) is what makes a fresh clone of a consuming project build with nothing installed;
// (1) is what makes it work with no network at all.
//
// No checksum file, deliberately. The checksum worth having is the *third-party* one:
// opus.env pins the SHA-256 of Xiph's source tarball and build.sh refuses to unpack
// anything else, because that is the supply chain this repository does not control.
// Hashing our own release assets on the way back out meant a file that had to be
// regenerated and committed after every release, restating what GitHub already publishes
// as a digest beside each asset and serves over TLS.
//
// `releases/latest/download/…` rather than a pinned tag, for the same reason: a tag written
// in here is a tag that has to be updated in here. The asset name carries the opus version,
// so a release of a *different* version cannot satisfy the URL — it 404s naming the version
// rather than quietly returning the wrong library.
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

    (fetch(manifest, name, version, &cached), format!("latest release asset for {name}"))
}

/// Download the latest release's archive for one target and unpack it into the cache.
fn fetch(manifest: &Path, name: &str, version: &str, cached: &Path) -> PathBuf {
    assert!(
        std::env::var("CARGO_NET_OFFLINE").as_deref() != Ok("true"),
        "libopus for {name} is not cached and cargo is offline. Run ./build.sh {name} && \
         ./sync-prebuilt.sh, or set LIBOPUS_PREBUILT_DIR to a prefix containing lib/."
    );

    let repo = opus_env(manifest, "PREBUILT_REPO");
    let asset = format!("libopus-{version}-{name}.tar.gz");
    let url = format!("https://github.com/{repo}/releases/latest/download/{asset}");

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

    // `tar` rather than a Rust tar crate: it is present on macOS, on every Linux image that
    // can run cargo, and in System32 on Windows 10 1803 and later, and a build dependency
    // here would be one every consumer compiles. It is also the integrity check that
    // remains — a truncated or corrupt download does not extract.
    run(Command::new("tar").arg("xzf").arg(&tarball).arg("-C").arg(&staging));
    std::fs::remove_file(&tarball).ok();

    std::fs::create_dir_all(cached.parent().unwrap()).ok();
    if std::fs::rename(&staging, cached).is_err() {
        // Either another build populated the cache first — fine, use theirs — or the rename
        // genuinely failed, which the caller's `lib/` check will report.
        let _ = std::fs::remove_dir_all(&staging);
    }
    cached.to_path_buf()
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
    // One archive per target. The x86_64 ones require AVX2 — Coffee Lake or newer, which is
    // the floor the consuming projects specified — so there is deliberately no variant here
    // for a CPU below it. Anything that old wants its own libopus, which is what
    // LIBOPUS_PREBUILT_DIR is for.
    match target {
        "aarch64-apple-darwin" => "macos-arm64",
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => "linux-aarch64",
        "x86_64-pc-windows-msvc" => "windows-x86_64-msvc",
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
