fn main() {
    // Link libghostty
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let ghostty_lib = std::env::var("GHOSTTY_LIB")
        .unwrap_or_else(|_| format!("{manifest_dir}/../ghostty/zig-out/lib"));

    println!("cargo:rustc-link-search=native={ghostty_lib}");
    println!("cargo:rustc-link-lib=ghostty");
    println!("cargo:rerun-if-env-changed=GHOSTTY_LIB");

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
