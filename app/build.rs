fn main() {
    // Link libghostty
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let ghostty_lib = std::env::var("GHOSTTY_LIB")
        .unwrap_or_else(|_| format!("{manifest_dir}/../ghostty/zig-out/lib"));

    println!("cargo:rustc-link-search=native={ghostty_lib}");
    // ghostty's zig build emits `ghostty-internal.so`/`.a` (no `lib` prefix),
    // AND embeds a SONAME of `libghostty.so` in the shared library (from
    // zig's internal library name, which `install()` doesn't rewrite when it
    // renames the output file). We need symlinks in the lib dir:
    //  - `libghostty-internal.so`/`.a` → so `-lghostty-internal` resolves at
    //    link time via the standard ELF `-lFOO` → `libFOO.{so,a}` search.
    //  - `libghostty.so` → so the dynamic linker finds the SONAME-declared
    //    dependency at runtime (ld.so searches for the SONAME, not for the
    //    filename we linked against).
    // All idempotent: if the link or a real file is already there, we skip.
    ensure_symlink(&ghostty_lib, "ghostty-internal.so", "libghostty-internal.so");
    ensure_symlink(&ghostty_lib, "ghostty-internal.a", "libghostty-internal.a");
    ensure_symlink(&ghostty_lib, "ghostty-internal.so", "libghostty.so");
    println!("cargo:rustc-link-lib=ghostty-internal");
    // Embed rpath so the binary finds libghostty.so at runtime without needing
    // LD_LIBRARY_PATH set. Absolute path (dev build, not release-distributable).
    println!("cargo:rustc-link-arg=-Wl,-rpath,{ghostty_lib}");
    println!("cargo:rerun-if-env-changed=GHOSTTY_LIB");
    println!("cargo:rerun-if-changed={ghostty_lib}/ghostty-internal.so");
    println!("cargo:rerun-if-changed={ghostty_lib}/ghostty-internal.a");

    // Compile GLAD (OpenGL loader) — libghostty expects these symbols
    // but only includes them in executable builds, not library builds.
    cc::Build::new()
        .file(format!("{manifest_dir}/../ghostty/vendor/glad/src/gl.c"))
        .include(format!("{manifest_dir}/../ghostty/vendor/glad/include"))
        .compile("glad");

    // Link WebKitGTK 6.0 for browser panels
    let webkit_lib = pkg_config("webkitgtk-6.0");
    for path in webkit_lib.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for lib in webkit_lib.libs {
        println!("cargo:rustc-link-lib={lib}");
    }
}

/// Create a relative symlink `dir/link_name` -> `target_filename`, if it
/// doesn't already exist. Silently no-ops if the target is missing; cargo
/// will produce the real link error later with a clearer message.
fn ensure_symlink(dir: &str, target_filename: &str, link_name: &str) {
    let target = std::path::Path::new(dir).join(target_filename);
    if !target.exists() {
        return;
    }
    let link = std::path::Path::new(dir).join(link_name);
    if link.exists() || link.symlink_metadata().is_ok() {
        return;
    }
    if let Err(e) = std::os::unix::fs::symlink(target_filename, &link) {
        eprintln!("cargo:warning=failed to symlink {}: {}", link.display(), e);
    }
}

struct PkgConfig {
    link_paths: Vec<std::path::PathBuf>,
    libs: Vec<String>,
}

fn pkg_config(name: &str) -> PkgConfig {
    let output = std::process::Command::new("pkg-config")
        .args(["--libs", name])
        .output()
        .unwrap_or_else(|e| panic!("pkg-config --libs {name} failed: {e}"));
    assert!(output.status.success(), "pkg-config --libs {name} failed");
    let flags = String::from_utf8(output.stdout).unwrap();

    let mut link_paths = Vec::new();
    let mut libs = Vec::new();
    for flag in flags.split_whitespace() {
        if let Some(path) = flag.strip_prefix("-L") {
            link_paths.push(std::path::PathBuf::from(path));
        } else if let Some(lib) = flag.strip_prefix("-l") {
            libs.push(lib.to_string());
        }
    }
    PkgConfig { link_paths, libs }
}
