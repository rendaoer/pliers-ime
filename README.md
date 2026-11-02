# pliers（钳子）

一个从零手写的 Wayland 中文输入法。敲 `nihao` + 空格 → 「你好」；组词时输入框旁边
弹出候选框，`←`/`→` 挪选中的那个、`↑`/`↓` 整页翻，也可以直接按数字 `1`–`9`；空格上屏。
中文模式下打英文单词也有补全：`hel` + 空格 → `help`（`kuber` → `kubernetes`）。

没有 GUI 框架：候选框是自己往 `wl_shm` 画像素，直接用 `wayland-client` 跟合成器说协议。
候选框要用 `zwp_input_popup_surface_v2`，而那个 surface 必须在**创建它的那条连接**上画，
所以没有套 imekit 之类的封装。

候选框长这样（mock 合成器截图，`nihao` → 你好）：

![候选框：你好、拟好、你、尼、逆……](docs/assets/popup.png)

## 特性

* **全拼 / 双拼 / 五笔**：全拼和双拼都有音节切分、整句候选、词频排序和用户调频；
  双拼支持自然码 / 小鹤 / 微软；五笔、郑码、仓颉这类码表方案走独立的 `wubi.db`
* **候选框不会空着**：一个候选都查不到时把原文贴出来（`wm` → `[wm]`），终端里也看得见
  自己敲了啥；单个字母会覆盖该字母开头的全部音节（`s` 不会漏掉「是」）
* **英文候选**：两万五千个词的词表是自己的 SQLite 库，打一半就补全（`hel` → help/hello）。
  打 `shou` 这种还挂在拼音上的串永远只出中文，不会冒出 `should`
* **托盘图标**：像 Steam 一样注册 `StatusNotifierItem`，Noctalia / waybar / KDE 的 tray
  自动发现；单色「中」/「英」，左键切中英文，右键是配置菜单（方案、双拼键位、切换键、
  候选个数、常用开关，写回 `config.toml`）。菜单可选分组或平铺；不想要就
  `PLIERS_NO_TRAY=1 pliers`
* **词库在 SQLite 里**：用 [turso](https://github.com/tursodatabase/turso)（Rust 写的 SQLite）
  读写，带权重字段；加词、调权重、看数据都能直接用 SQL
* **命令行不折腾**：`pliers doctor` 自检（字体/词库/Wayland/工具），`pliers status` 秒回，
  改配置有上下选的交互模式；词库下载支持镜像、断点续传、sha256 校验，一个源不行自动换下一个
* **Nix**：自带 flake 和 Home Manager 模块，装包/写配置/自动启动都能声明式搞定
* **测试**：247 个单测 + 30 个 mock 合成器场景，不需要真合成器、不需要词库

## 安装

### Nix / NixOS

```bash
nix run github:rendaoer/pliers-ime              # 直接跑
nix profile install github:rendaoer/pliers-ime  # 装进 profile
```

Home Manager 里：

```nix
imports = [ inputs.pliers.homeManagerModules.default ];
services.pliers.enable = true;
```

细节（NixOS `/etc/nixos` 的改法、service、词库）见 [docs/nix.md](docs/nix.md)。
Nix 只装程序；Home Manager 模块才会写配置。**词库要单独装一次**（见下）。

### 源码 / crates.io

需要 **libxkbcommon** 的开发包和 `pkg-config`（Debian/Ubuntu：`libxkbcommon-dev`；
Arch：`libxkbcommon`；Fedora：`libxkbcommon-devel`）。少了它链接会报
`unable to find library -lxkbcommon`。

```bash
cargo run -- --init      # 写配置 + 下载词库（几十 MB）
cargo run                # 跑起来
```

装完自检：

```bash
pliers doctor            # 字体、词库、Wayland、合成器协议、下载工具，一项一项告诉你缺什么
```

（还在仓库里直接 `cargo run` 的话，把 `pliers` 换成 `cargo run --`，比如 `cargo run -- doctor`。）

已发布到 crates.io（版本可能比仓库旧）：

```bash
cargo install pliers-ime     # 命令叫 pliers；一样需要 libxkbcommon
pliers --init
pliers
```

从仓库构建发布版：

```bash
cargo build --release
./target/release/pliers --init
./target/release/pliers
```

### 词库

`--init` 会装拼音词库和英文词表；也可以只装词库：

```bash
pliers fetch all         # 拼音 + 英文
pliers fetch pinyin      # 只装拼音
pliers update            # 哪个旧就更新哪个（已经一样就跳过）
```

词库默认用[白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)（约 100 万条，
自带字频词频；装好后约 86 MB，Release 上的压缩资产 27 MB）。不想下载现成的、想自己从
语料构建：`cargo install pliers-dict` 之后 `pliers build pinyin`；换源或离线装：
`PLIERS_PINYIN_URL=... pliers fetch pinyin --url file:///...`；直连 GitHub 太慢就加镜像：
`pliers fetch pinyin --mirror ghproxy`（自动在官方地址和内置镜像之间重试，断点续传，
下完用 Release 上的 `.sha256` 校验）。词库资产按 GPL-3.0 分发（语料是 GPL-3.0 的），
代码本身不受影响。

## 跑起来

把焦点放进输入框，敲 `n i h a o` 再按空格。合成器要支持 `zwp_input_method_v2` 和
虚拟键盘（`pliers doctor` 会检查）；应用那边要自己实现 text-input 协议：
Firefox / GTK / Qt 应用都行，alacritty 0.17（基于 winit）实测也可以。

**装好了却打不出字？** 先跑 `pliers doctor`。两种最常见的情况它都会直接指出来：

* **没有词库** —— `pliers --init` 没跑成功（下载断了）。重跑同一条命令会接着下；
* **候选框弹出来但里面没字** —— 系统里没有中文字体，或者字体在 NixOS 的 `/nix/store`
  这种非常规目录。候选框现在会读 fontconfig 的配置去找字体，找不到时用
  `PLIERS_FONT=/path/to/font.ttc pliers` 手动指定。

## 常用命令

命令都是**动词在前、种类在后**（种类就三个：`pinyin` / `wubi` / `english` —— 字典是个总概念）：

```bash
pliers status [种类]         # 看现状：不给种类 = 正在跑的实例 + 所有库
pliers doctor                # 自检：字体、词库、Wayland、下载工具（打不出字先看它）
pliers path   [名字]         # 文件都在哪（config / pinyin / wubi / user / english / corpus / socket）
pliers build  <种类> [文件]  # 自己构建（pinyin 下语料；wubi 用自己的码表；english 可给自己的词表）
pliers fetch  <种类|all>     # 从 Release 下载预构建的（--mirror 换镜像、--force 重装）
pliers update                # 哪个旧就更新哪个（已经一样就跳过）
```

运行中也能改（`set` 只改内存，重启就回去；`config set` 写文件并保留注释）：

```bash
pliers set                          # 交互模式：上下选着改
pliers set scheme.kind double-pinyin
pliers config set scheme.layout flypy
pliers reload
pliers mode [chinese|english]
pliers toggle
```

区别和每一项的取值见 [docs/config.md](docs/config.md)。

## 文件

每个库一个文件（外加用户数据），各管各的：

| 文件 | 是什么 | 怎么更新 |
| --- | --- | --- |
| `~/.local/share/pliers/pinyin.db` | 拼音词库 + 音节表（约 86 MB） | `pliers fetch pinyin --force` / `pliers build pinyin` |
| `~/.local/share/pliers/wubi.db` | 码表库（五笔/郑码/仓颉，用自己的码表建） | `pliers build wubi <码表.txt>` |
| `~/.local/share/pliers/user.db` | 你选过的词、自己拼的句子、按 `Del` 拉黑的词（几个库共用） | 自动写；想清空就删掉它 |
| `~/.local/share/pliers/english.db` | 英文候选词表（SQLite，25223 词） | `pliers fetch english` / `pliers build english 词表.txt` |

老版本里拼音词库叫 `dict.db`，第一次用新版本时会自动改名成 `pinyin.db`；改名失败
（文件系统只读之类）就继续用老名字那份。

重导拼音词库不会弄丢你的习惯（`user.db` 是另一个文件），也不会把五笔抹掉。英文词表
没装/读不了时，引擎会用二进制里那份兜底。表结构、权重和 SQL 用法见
[docs/dictionary.md](docs/dictionary.md)。

## 配置与托盘

配置默认是全拼，不用写任何文件；要一份带注释的模板就 `pliers --init-config`。
改完不用重启：跑着的实例每 0.7 秒看一眼文件 mtime，自动重读。

托盘图标是**自动**的：输入法启动时注册一个 `org.kde.StatusNotifierItem`，桌面带的
`StatusNotifierWatcher`（Noctalia 的 tray 就是）会自己发现它 —— 跟 Steam 出现在状态栏
是同一条路。图标显示当前「中」/「英」，左键点一下切中英文；没在跑/读不到状态时显示「?」。
右键菜单里改的会写回 `~/.config/pliers/config.toml`；菜单样式在 `[tray] menu`
（`grouped` 分组 / `flat` 平铺，默认分组）。不想开：`PLIERS_NO_TRAY=1 pliers`。

如果更想要 bar 里的一行文字（而不是 tray 里的小图标），`contrib/noctalia` 里还有一个
文字版插件，二选一即可。

**同一个 seat 上只能有一个输入法**：起第二个实例时旧实例会打印「有另一个输入法接管了
这个 seat」然后退出，所以别同时跑两个。

## 按键速查

| 按键 | 行为 |
| --- | --- |
| `a`–`z`（小写） | 组词；**只有小写参与匹配**，大写一律当英文字符 |
| 打英文单词 | 给补全候选（`hel` → help/hello/hell，`kuber` → kubernetes）：切不出拼音时才出，空格上屏选中的那个 |
| 空格 | 上屏选中的候选（可能是"只匹配了前面一段"的分段候选） |
| `←` `→` / `Tab` / `Shift+Tab` | 挪选中的候选（挪出这一页会自动翻页） |
| `↑` `↓` / `,` `.` / `-` `=` / `PageUp` `PageDown` | 整页翻，页内位置保持 |
| `1`–`9` / 小键盘 `1`–`9` | 选这一页的第几个 |
| 其他符号 | **先把候选上屏、再把符号转给应用**（反了屏幕上就是 `,你好`） |
| 大写字母 | 没组词 → 原样转发；组词当中 → 并进预编辑，整串当英文原文 |
| 回车 / `Esc` | 原样提交 / 取消这次组词（这个键不给应用） |
| 退格 | 删掉一个字符；**按住不放会一直删**（跟普通键盘一样） |
| `Del` | 删掉**自己拼出来的**候选（词库里原本就有的删不了）；按住不放会重复 |
| `Ctrl` + 空格 | 切中英文（可在配置/托盘菜单里改；切换时半截拼音先上屏） |
| `Ctrl` / `Alt` / `Super` + 任何键 | 不组词，原样转发 |

完整行为、中文标点、英文候选的规则、分段上屏的"记性"见 [docs/usage.md](docs/usage.md)。

## 开发与测试

```bash
cargo test --workspace        # 247 个单测，不需要合成器、不需要词库
./tools/run_mock_tests.sh     # mock 合成器跑 30 个场景（含在线改配置）
nix build .#                  # 只编包（同时产出 pliers 和 pliers-dict）
nix develop                   # Rust + libxkbcommon + wayland + bindgen
```

`PLIERS_DEBUG=1 pliers` 会把每个按键的判定过程打到 stderr；`pliers doctor` 先查环境。
更细的测试说明见 [docs/testing.md](docs/testing.md)。

## 文档

| 文档 | 讲什么 |
| --- | --- |
| [docs/usage.md](docs/usage.md) | 按键全表、大写字母为什么不参与匹配、中英文切换、中文标点、分段上屏 + 记性、`Del` 的语义 |
| [docs/config.md](docs/config.md) | `config.toml` 每一项（全拼/双拼/码表/engine/english）、`pliers set` vs `pliers config set`、自动重读、交互模式 |
| [docs/dictionary.md](docs/dictionary.md) | 词库怎么装、换源、自己从语料构建、表结构与权重、用 SQL 加词、`lookup` 看候选 |
| [docs/internals.md](docs/internals.md) | crate 划分、Wayland 协议、**为什么拼音查询不能交给 SQL**、整句候选、长按重复、候选框怎么画 |
| [docs/pitfalls.md](docs/pitfalls.md) | 实测踩出来的坑（Ctrl+A 被吃掉、松手后应用以为键还按着……）+ 已知不足 |
| [docs/testing.md](docs/testing.md) | 单测、跑起来、`PLIERS_DEBUG`、`pliers doctor`、mock 场景都验了什么 |
| [docs/nix.md](docs/nix.md) | flake 安装、Home Manager 模块、NixOS `/etc/nixos` 改法、词库和 service 的注意事项 |

## 代码结构

一个 cargo workspace，按"谁管什么"切成五块（详细文件分工见 [docs/internals.md](docs/internals.md)）：

| crate | 管什么 |
| --- | --- |
| `crates/pliers-engine` | 按键状态机 + 输入方案（全拼/双拼/码表）+ 词库 + 英文词表 + 配置 |
| `crates/pliers-popup` | 候选框长什么样：找字体、排版、画像素、共享内存文件 |
| `crates/pliers-wayland` | 跟合成器说协议：注册、抓键盘、转发按键、贴候选框、托盘图标 |
| `crates/pliers-ime` | `main()`：读配置、把上面几个拼起来（命令是 `pliers`） |
| `crates/pliers-dict` | 导入工具：rime 词库（`.dict.yaml`）→ SQLite 词库 |

这么切是为了**输入方案、词库、候选框长相都能脱离合成器跑测试**，协议层里只剩下
"Wayland 要求这么做"的东西：

* 换输入方案 → 改 `~/.config/pliers/config.toml`；加一套新方案 → 在 `scheme.rs` 里实现 `Scheme`
* 加词 / 调词频 → 用 SQL 改词库，或者重新跑一遍 `pliers-dict`
* 换候选框长相（甚至换成 egui/Slint 画）→ `crates/pliers-popup`
* 加协议功能（比如 `delete_surrounding_text`）→ `crates/pliers-wayland`

## 许可

* **代码**：[MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE)，随你挑一个用
* **词库资产**（Release 里的 `pinyin.db.zst`）：**GPL-3.0** —— 它是
  [rime-frost](https://github.com/gaboolic/rime-frost) 那份 GPL-3.0 语料的衍生作品，
  跟代码的许可是两回事。不想碰它就 `pliers build pinyin` 自己从上游构建（那只是下载语料，
  不涉及再分发）—— 见 [docs/dictionary.md](docs/dictionary.md#许可)
* **英文词表数据**（`crates/pliers-engine/data/english.txt`，编译进二进制那份兜底；发布出去的
  `english.db.zst` 也是它的衍生作品）：
  **CC-BY-SA-4.0** —— 词频来自 [FrequencyWords](https://github.com/hermitdave/FrequencyWords)
  的 `en_50k`（OpenSubtitles2018 词频）；里面掺的开发常用词
  （`tools/english-extra.txt`）是本项目自己写的，跟代码同许可。出处和重新生成的步骤见
  [data/README.md](crates/pliers-engine/data/README.md)

## 名字

**pliers / 钳子** —— 螃蟹那对钳子。crates.io 上 `pliers` 已被占，所以主包叫
`pliers-ime`，但**命令还是 `pliers`**（配置目录 `~/.config/pliers/`、环境变量 `PLIERS_*` 也一样短）。
