# Noctalia 状态栏插件（可选）

> **pliers 现在自带托盘图标**：启动时注册 `StatusNotifierItem`，Noctalia 的 tray 会自动显示
> 「中」/「英」，点击切换 —— 不需要这个插件。这个插件只适合"想在 bar 里显示一行文字，
> 而不是 tray 里的小图标"的情况；二选一，不要同时开。

在 Noctalia 的 bar 上显示 pliers 现在是中文还是英文，点一下切中英文。

插件不调用 `pliers` 二进制，而是直接通过 Unix socket 跟正在跑的输入法说话：

* 每 1 秒读一次 `status` 里的 `mode=`（只回一行「中」/「英」）；
* 左键发一条 `toggle`（跟 `Ctrl+空格` 走同一条路：正在打的半截拼音会先上屏，再切换）。

这样不用管 `pliers` 在不在 Noctalia 的 PATH 上，也不用处理 Nix 动态库路径。

## 安装

把 `plugin.toml` 和 `widget.luau` 放进 Noctalia 的本地插件目录：

```bash
mkdir -p ~/.local/share/noctalia/plugins/pliers-ime
cp contrib/noctalia/plugin.toml contrib/noctalia/widget.luau \
   ~/.local/share/noctalia/plugins/pliers-ime/
```

然后启用（`noctalia msg plugins list` 里应该能看到 `rendaoer/pliers-ime`）：

```bash
noctalia msg plugins enable rendaoer/pliers-ime
```

## 加到 bar 上

在 Noctalia 的配置里定义 widget，并把它加进某条 bar：

```toml
[widget.pliers]
type = "rendaoer/pliers-ime:ime"

[bar.default]
end = ["media", "tray", "pliers", "notifications", "clipboard", "network",
       "bluetooth", "volume", "brightness", "battery", "control-center", "session"]
```

home-manager / NixOS 的话，把上面两段并进 `programs.noctalia.settings` 就行。
也可以不改文件，用 IPC 写进 Noctalia 的 `settings.toml`：

```bash
noctalia msg config-set widget.pliers.type "'rendaoer/pliers-ime:ime'"
noctalia msg config-set bar.default.end "['media','tray','pliers','notifications','clipboard','network','bluetooth','volume','brightness','battery','control-center','session']"
```

（`[plugins].enabled` 由 `noctalia msg plugins enable` 写；本地插件目录是
`~/.local/share/noctalia/plugins/`，Noctalia 会把它当成 `local` 源。）

## 行为

| 状态 | 显示 | tooltip |
| --- | --- | --- |
| 中文 | 中 | pliers：中文（左键切英文） |
| 英文 | 英 | pliers：英文（左键切中文） |
| 没在跑 / socket 连不上 | 拼 | 具体的 `nc` 报错 |

* socket 位置：`$PLIERS_SOCKET` 优先，否则 `$XDG_RUNTIME_DIR/pliers.sock`（跟 pliers 的规则一样）。
* 依赖系统里有 `nc`（NixOS 默认有，`/run/current-system/sw/bin/nc`）。没有的话把
  `widget.luau` 里的 `nc -U` 换成你喜欢的 Unix socket 客户端。
* `toggle` 是新加的遥控命令：**旧版 pliers 实例只认 `status`/`reload`/`set`，要重启一次**
  （`Ctrl+C` 再启动）才能点一下切换；在那之前状态显示还是正常的。
