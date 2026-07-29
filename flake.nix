{
  description = "limux — Ghostty-powered terminal workspace with WebKit browser panels";

  # For AppImage releases, use `scripts/build-appimage.sh`. It runs
  # inside the nix dev shell, fetches the official appimagetool from
  # AppImageKit, and produces a real self-contained Type 2 AppImage
  # that runs on any modern Linux distribution. The build script does
  # all the heavy lifting; the flake just provides the build tools.

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = pkgs.lib;

      # Runtime + build libraries shared by the dev shell. The build
      # script also uses some of these to compute the runtime closure
      # for the AppImage.
      runtimeLibs = with pkgs; [
        gtk4
        glib
        gtk-layer-shell
        gobject-introspection
        gdk-pixbuf
        cairo
        pango
        webkitgtk_6_0
        libsoup_3
        libsecret
        glib-networking
        libglvnd
        mesa
        libgbm
        libepoxy
        graphene
        libxkbcommon
        libX11
        libXi
        libxcursor
        libxinerama
        libxrandr
        wayland
        freetype
        harfbuzz
        fontconfig
        libxml2
        openssl
        shared-mime-info
        desktop-file-utils
      ];

      buildTools = with pkgs; [
        rustc
        cargo
        rustfmt
        zig_0_15
        go
        pkg-config
        gcc
        gnumake
        python3
      ];
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = buildTools ++ [
          pkgs.uv
          pkgs.xvfb-run
          # build-appimage.sh uses these:
          pkgs.squashfsTools      # mksquashfs / unsquashfs
          pkgs.curl               # downloading appimagetool
          pkgs.file               # ELF detection
          pkgs.patchelf           # fix rpaths
        ] ++ runtimeLibs;

        shellHook = ''
          export LIBGL_ALWAYS_SOFTWARE=''${LIBGL_ALWAYS_SOFTWARE:-1}
          export GDK_DEBUG=gl-egl
          echo ""
          echo "limux dev shell ready."
          echo "  Dev build:  cd ghostty && zig build -Dtarget=x86_64-linux-gnu -Dcpu=x86_64_v3 -Dapp-runtime=none -Demit-lib-vt=false -Doptimize=ReleaseFast && cd .. && cargo build"
          echo "  Release:    cargo build --release"
          echo "  Test:       scripts/run-tests-linux.sh"
          echo "  AppImage:   cargo build --release && scripts/build-appimage.sh"
          echo ""
          echo "Note: -Dtarget=x86_64-linux-gnu -Dcpu=x86_64_v3 keeps"
          echo "  AVX2/FMA enabled (good perf on Haswell+, 2013+) but stops"
          echo "  short of AVX-512. Without these flags, ghostty uses the"
          echo "  host's native CPU features, which can include AVX-512"
          echo "  and crash with SIGILL on CPUs that lack those features."
          echo ""
        '';
      };
    };
}
