# Nix / NixOS / Home Manager

[← README](../README.md) · 相关：[使用](usage.md) · [配置](config.md)

仓库带了 `flake.nix`。装包、写配置、自动启动都可以走 Home Manager，不需要手动
`cargo install`。

## 直接跑 / 装进 profile

```bash
nix run github:rendaoer/pliers-ime              # 从仓库跑
nix profile install github:rendaoer/pliers-ime  # 装进 profile（pliers + pliers-dict）
```

装完先跑一次：

```bash
pliers --init     # 下载拼音词库/英文词表到 ~/.local/share/pliers，写一份配置
pliers doctor     # 检查字体、词库、Wayland、合成器协议
```

词库是运行时下载的（几十 MB 的派生物，不进 Nix store；输入法还要往里写 WAL）。
`pliers --init` 或者 `pliers fetch all` 只需要跑一次。

## Home Manager

`flake.nix` 导出了 `homeManagerModules.default`（也导出旧名字 `homeModules.default`）。
在 flake 里加输入：

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    pliers.url = "github:rendaoer/pliers-ime";
  };

  outputs = { nixpkgs, home-manager, pliers, ... }: {
    homeConfigurations."你的用户名" = home-manager.lib.homeManagerConfiguration {
      pkgs = import nixpkgs { system = "x86_64-linux"; };
      modules = [
        pliers.homeManagerModules.default
        {
          home.username = "你的用户名";
          home.homeDirectory = "/home/你的用户名";
          home.stateVersion = "25.05";

          services.pliers = {
            enable = true;

            # 这些会写进 ~/.config/pliers/config.toml（TOML 结构，见 docs/config.md）
            settings = {
              scheme = {
                kind = "double-pinyin";
                layout = "flypy";
              };
              engine.toggle_keys = [ "ctrl+space" ];
              dict.max_candidates = 9;
              tray.menu = "grouped";
            };
          };
        }
      ];
    };
  };
}
```

`home-manager switch` 之后：

```bash
pliers --init                 # 装词库（配置已经有的话不会覆盖）
systemctl --user status pliers
```

> 启用 service 之后就别再手动跑 `pliers` 了：同一个 seat 上只能有一个输入法，
> 第二个会把第一个顶掉。先 `pkill -x pliers` 再 switch，或者 switch 完把旧终端里那个停掉。

### NixOS 的 `/etc/nixos` 改法

如果你不是 standalone Home Manager，而是 NixOS 的 home-manager 模块（`/etc/nixos`）：

```nix
# /etc/nixos/flake.nix —— 加到现有的 inputs = { ... }; 里面
pliers = {
  url = "github:rendaoer/pliers-ime";   # 还没推的话见下面“本地仓库”
  inputs.nixpkgs.follows = "nixpkgs";
};
```

```nix
# /etc/nixos/home.nix
{ pkgs, inputs, ... }:

{
  imports = [
    ./home/ui.nix
    # …
    inputs.pliers.homeManagerModules.default
  ];

  services.pliers = {
    enable = true;
    settings = {
      scheme = {
        kind = "double-pinyin";
        layout = "flypy";
      };
      engine.toggle_keys = [ "ctrl+space" ];
      dict.max_candidates = 9;
      tray.menu = "grouped";
    };
  };
}
```

`/etc/nixos` 是 root 的，改完用你平时的 `nixos-rebuild switch --flake /etc/nixos#nixos`。

**本地还没推仓库**：把 `url` 换成
`git+file:///home/你的用户名/Projects/pliers-ime`（会带上未提交的改动；用 root 跑
`nixos-rebuild` 时如果报 dubious ownership，给 root 加
`git config --global --add safe.directory /home/你的用户名/Projects/pliers-ime`）。
别用 `path:`：它会把这个目录（包括 `target/`）整份拷进 store。

### 模块选项

| 选项 | 默认 | 说明 |
| --- | --- | --- |
| `services.pliers.enable` | `false` | 装包 + 写配置 + 起 service |
| `services.pliers.package` | flake 里的包 | 想换成自己编的/别的版本时改 |
| `services.pliers.autoStart` | `true` | 是否起 `systemd --user` service；关掉就自己 `spawn-at-startup "pliers"` |
| `services.pliers.tray` | `true` | 关掉会设 `PLIERS_NO_TRAY=1`，不注册托盘图标 |
| `services.pliers.waylandDisplay` | `null` | 固定 `WAYLAND_DISPLAY`（合成器没把它 import 进 systemd 环境时用） |
| `services.pliers.environment` | `{}` | 额外环境变量，比如 `PLIERS_FONT` |
| `services.pliers.settings` | `{}` | 写进 `config.toml` 的 TOML；留空就不碰配置文件 |
| `services.pliers.overwriteConfig` | `true` | `settings` 非空时每次 switch 都覆盖；设 `false` 只在文件不存在时放一份初始配置 |
| `services.pliers.systemdTarget` | `"graphical-session.target"` | service 挂靠的 target |

### 配置写回和托盘

`services.pliers.settings` 是**声明式**的：每次 switch 会用 Nix 里这份覆盖
`~/.config/pliers/config.toml`。所以：

- 想保留托盘菜单/`pliers config set` 里改的值 → `overwriteConfig = false`，
  或者直接把 `settings` 留空、只用 `pliers --init` 生成的那份；
- 想让 Nix 里的配置永远生效 → 保持默认，运行时改的会在下次 switch 被覆盖。

托盘图标是 SNI，Noctalia / waybar / KDE 的 tray 会自动发现，不需要额外配置。
不想要就 `tray = false`。

### 起不来 / 连不上 Wayland

service 默认挂在 `graphical-session.target` 上。如果你的合成器没有启动这个 target，
或者 systemd 用户环境里没有 `WAYLAND_DISPLAY`：

1. 先看日志：`journalctl --user -u pliers -b`；
2. 合成器支持的话，在启动脚本里补一句
   `systemctl --user import-environment WAYLAND_DISPLAY`；
3. 或者直接固定：
   ```nix
   services.pliers.waylandDisplay = "wayland-1";
   ```
4. 也可以 `autoStart = false`，在合成器配置里自己启动（niri/umbriel 的
   `spawn-at-startup "pliers"`），这时 service 不参与。

词库没装时 service 会退出；模块把重启限制成 60 秒内最多 5 次，不会无限刷日志。
装完词库后：

```bash
systemctl --user reset-failed pliers
systemctl --user start pliers
```

## overlay（可选）

不想通过 `services.pliers.package` 指定包、想直接写 `pkgs.pliers`：

```nix
nixpkgs.overlays = [ inputs.pliers.overlays.default ];
# 然后
home.packages = [ pkgs.pliers ];
```

## 开发

```bash
nix develop          # Rust + libxkbcommon + wayland + bindgen
cargo test --workspace
nix build .#         # 只编包，不跑单测
```

> NixOS 上不要把 `cargo build` 出来的 `target/release/pliers` 直接放进 PATH：
> 它没有 rpath，会报 `libxkbcommon.so.0: cannot open shared object file`。
> 用 `nix develop`（会设 `LD_LIBRARY_PATH`）或 Nix 包/`/etc/profiles` 里的二进制。
