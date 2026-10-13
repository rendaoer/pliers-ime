{
  inputs = {
    naersk.url = "github:nix-community/naersk";
    nixpkgs.url = "https://channels.nixos.org/nixpkgs-unstable/nixexprs.tar.zst";
    utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      naersk,
      rust-overlay,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };

        naersk-lib = pkgs.callPackage naersk {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        isLinux = pkgs.lib.strings.hasInfix "linux" system;

        # 编译期需要:头文件、.pc 文件、clang 链接器
        nativeDeps = with pkgs; [
          pkg-config
          clang
          lld
        ];

        # Linux 上需要链接的系统库
        linuxLibs = with pkgs; [
          libxkbcommon
          wayland
        ];
      in
      {
        packages.default = naersk-lib.buildPackage {
          src = ./.;
          meta.mainProgram = "pliers";
          nativeBuildInputs = nativeDeps;
          buildInputs = pkgs.lib.optionals isLinux linuxLibs;
          cargoBuildOptions = x: x ++ [ "--no-default-features" ];
        };

        devShells.default =
          with pkgs;
          mkShell {
            buildInputs = [
              rustToolchain
              pre-commit
              pkg-config
              watchexec
            ]
            ++ lib.optionals isLinux (
              linuxLibs
              ++ [
                clang
                lld
              ]
            );

            RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
            LD_LIBRARY_PATH = lib.makeLibraryPath (lib.optionals isLinux linuxLibs);
          };
      }
    );
}
