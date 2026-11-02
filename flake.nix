{
  description = "pliers：一个从零手写的 Wayland 中文输入法";

  inputs = {
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
      rust-overlay,
    }:
    utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        lib = pkgs.lib;

        # 打包直接用 nixpkgs 的 rustPlatform（当前锁定的 nixpkgs 是 Rust 1.99），
        # 不把 devShell 那套 rust-analyzer / rust-src 拖进安装闭包。
        #
        # 开发环境还是用 rust-overlay，顺手带 rust-src + rust-analyzer。
        devToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
          ];
        };

        version = (lib.importTOML ./Cargo.toml).workspace.package.version;

        # 一个包装两个可执行文件：`pliers` 和 `pliers-dict`。
        # `pliers build pinyin` / `build wubi` 会在自己旁边找 `pliers-dict`，
        # 所以两个必须在同一个 $out/bin 里（拆成两个包反而会找不到）。
        pliers = pkgs.rustPlatform.buildRustPackage {
          pname = "pliers";
          inherit version;
          # 只把编译需要的东西放进源码树：文档/截图/contrib 改动不该让整个包重编
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates
            ];
          };

          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "--workspace" ];

          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.wayland-scanner
            pkgs.rustPlatform.bindgenHook
          ];
          buildInputs = [
            pkgs.libxkbcommon
            pkgs.wayland
          ];

          # 单测要词库/字体，属于开发期的事；nix build 只负责编出可执行文件
          doCheck = false;
          installPhase = ''
            runHook preInstall
            mkdir -p $out/bin
            install -m755 target/${pkgs.stdenv.hostPlatform.rust.cargoShortTarget}/release/pliers $out/bin/pliers
            install -m755 target/${pkgs.stdenv.hostPlatform.rust.cargoShortTarget}/release/pliers-dict $out/bin/pliers-dict
            runHook postInstall
          '';

          meta = {
            description = "一个从零手写的 Wayland 中文输入法（全拼 / 双拼 / 五笔）";
            homepage = "https://github.com/rendaoer/pliers-ime";
            license = lib.licenses.mit;
            mainProgram = "pliers";
            platforms = lib.platforms.linux;
          };
        };
      in
      {
        packages = {
          default = pliers;
          pliers = pliers;
        };

        apps.default = {
          type = "app";
          program = "${pliers}/bin/pliers";
          meta.description = "pliers Wayland 中文输入法";
        };

        devShells.default = pkgs.mkShell {
          buildInputs = [
            devToolchain
            pkgs.pkg-config
            pkgs.watchexec
            pkgs.pre-commit
            pkgs.clang
            pkgs.lld
            pkgs.libxkbcommon
            pkgs.wayland
            pkgs.wayland-scanner
            pkgs.rustPlatform.bindgenHook
          ];

          RUST_SRC_PATH = pkgs.rustPlatform.rustLibSrc;
          LD_LIBRARY_PATH = lib.makeLibraryPath [
            pkgs.libxkbcommon
            pkgs.wayland
          ];
        };

        formatter = pkgs.nixfmt;
      }
    )
    // {
      # `pkgs.pliers`：把这个 flake 的包塞进 nixpkgs，方便 Home Manager / NixOS
      # 里直接写 `pkgs.pliers`（不写 overlay 的话也能用 `inputs.pliers.packages...`）
      overlays.default = final: prev: {
        pliers = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      # `inputs.pliers.homeManagerModules.default`
      # （`homeModules.default` 是新名字，两个都导出一份）
      homeManagerModules.default = import ./nix/home-manager.nix { inherit self; };
      homeModules.default = self.homeManagerModules.default;
    };
}
