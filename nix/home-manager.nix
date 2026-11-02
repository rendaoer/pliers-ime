# pliers 的 Home Manager 模块。
#
#   imports = [ inputs.pliers.homeManagerModules.default ];
#   services.pliers.enable = true;
#
# 默认做三件事：装包、写配置（如果给了 settings）、起一个 systemd user service。
# 词库不在 Nix store 里（太大，而且输入法要往里写 WAL），第一次装完手动跑一次
# `pliers --init` 或 `pliers fetch all`。
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.pliers;

  system = pkgs.stdenv.hostPlatform.system;
  defaultPackage = lib.attrByPath [
    "packages"
    system
    "default"
  ] (throw "pliers：这个 flake 没有 ${system} 的包（它只支持 Linux）") self;

  toml = pkgs.formats.toml { };
  configFile = toml.generate "pliers-config.toml" cfg.settings;

  # 跑着的输入法改了配置之后，下次 home-manager switch 要不要覆盖回去。
  # 默认覆盖：Nix 里的 settings 是声明式的；想保留托盘里改的值就设成 false，
  # 那样只在文件不存在时放一份初始配置。
  configPath = "${config.xdg.configHome}/pliers/config.toml";
  installConfig =
    if cfg.overwriteConfig then
      ''
        # 先删掉旧的（可能是以前 xdg.configFile 留下的只读 symlink），再放一份可写的
        $DRY_RUN_CMD ${pkgs.coreutils}/bin/rm -f ${lib.escapeShellArg configPath}
        $DRY_RUN_CMD ${pkgs.coreutils}/bin/install -Dm644 ${configFile} ${lib.escapeShellArg configPath}
      ''
    else
      ''
        if [ ! -e ${lib.escapeShellArg configPath} ]; then
          $DRY_RUN_CMD ${pkgs.coreutils}/bin/install -Dm644 ${configFile} ${lib.escapeShellArg configPath}
        fi
      '';

  environment = [
    "XDG_CONFIG_HOME=${config.xdg.configHome}"
    "XDG_DATA_HOME=${config.xdg.dataHome}"
  ]
  ++ lib.optional (!cfg.tray) "PLIERS_NO_TRAY=1"
  ++ lib.optional (cfg.waylandDisplay != null) "WAYLAND_DISPLAY=${cfg.waylandDisplay}"
  ++ lib.mapAttrsToList (name: value: "${name}=${toString value}") cfg.environment;
in
{
  options.services.pliers = {
    enable = lib.mkEnableOption "pliers Wayland 中文输入法";

    package = lib.mkOption {
      type = lib.types.package;
      default = defaultPackage;
      defaultText = lib.literalExpression "inputs.pliers.packages.\${pkgs.stdenv.hostPlatform.system}.default";
      description = "pliers 包。";
    };

    autoStart = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        是否起一个 systemd user service 自动启动。关掉的话只装包/写配置，
        适合自己在合成器的 `spawn-at-startup` 里启动。
      '';
    };

    tray = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "是否注册 StatusNotifierItem 托盘图标（Noctalia / waybar / KDE）。";
    };

    waylandDisplay = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "wayland-1";
      description = ''
        systemd user service 里固定 WAYLAND_DISPLAY。一般不用设：合成器会
        把 WAYLAND_DISPLAY import 进用户 systemd 环境；如果你的合成器没做，
        或者 service 报「连不上 Wayland」，再设这个。
      '';
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf (
        lib.types.oneOf [
          lib.types.str
          lib.types.int
          lib.types.bool
        ]
      );
      default = { };
      example = {
        PLIERS_FONT = "/run/current-system/sw/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
      };
      description = "传给 pliers 的额外环境变量。";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        {
          scheme = {
            kind = "double-pinyin";
            layout = "flypy";
          };
          engine.toggle_keys = [ "ctrl+space" ];
          dict.max_candidates = 9;
          tray.menu = "grouped";
        }
      '';
      description = ''
        写进 `~/.config/pliers/config.toml` 的配置（TOML 结构）。
        留空 = 不碰配置文件，适合保留 `pliers --init` / 托盘菜单里改的那份。
      '';
    };

    overwriteConfig = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        `settings` 非空时，每次 home-manager switch 是否覆盖已有的 config.toml。
        默认覆盖（声明式）；设成 false 则只在文件不存在时放一份初始配置，
        保留运行时的改动。
      '';
    };

    systemdTarget = lib.mkOption {
      type = lib.types.str;
      default = "graphical-session.target";
      description = "service 挂在哪个 systemd user target 上（PartOf/After/WantedBy）。";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    home.activation.pliersConfig = lib.mkIf (cfg.settings != { }) (
      lib.hm.dag.entryAfter [ "writeBoundary" ] installConfig
    );

    systemd.user.services.pliers = lib.mkIf cfg.autoStart {
      Unit = {
        Description = "pliers Wayland input method";
        Documentation = [ "https://github.com/rendaoer/pliers-ime" ];
        PartOf = [ cfg.systemdTarget ];
        After = [ cfg.systemdTarget ];
        # 词库没装时 pliers 会反复退出；别无限重启把 journal 刷满
        StartLimitBurst = 5;
        StartLimitIntervalSec = 60;
      };

      Service = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/pliers";
        Restart = "on-failure";
        RestartSec = 3;
      }
      // lib.optionalAttrs (environment != [ ]) {
        Environment = environment;
      };

      Install.WantedBy = [ cfg.systemdTarget ];
    };
  };
}
