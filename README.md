# pliers（钳子）

最小可用的 Wayland 中文输入法：敲 `nihao` + 空格 → 输出「你好」，组词时输入框旁边弹出
候选框，`↓`/`Tab` 或数字 `1`–`9` 挑，空格上屏选中的那个。

* **全拼**是完整实现的：150 万词的词库、音节切分、**整句候选**、词频排序、用户调频
* **双拼 / 五笔**留好了位置：双拼（自然码/小鹤/微软）已经能用，五笔是"码表方案"那条路
* **行为都在配置文件里**（TOML）：换方案、换词库、改候选个数都不用改代码
* **词库在 SQLite 里**：用 [turso](https://github.com/tursodatabase/turso)（Rust 写的 SQLite）读写，
  带权重字段，加词/调权重/看数据都能直接用 SQL

没有 GUI 框架：候选框是自己往 `wl_shm` 画像素。它直接用 `wayland-client` 跟合成器说协议，
不经过 imekit 之类的封装 —— 因为候选框要用 `zwp_input_popup_surface_v2`，而那个 surface
必须在**创建它的那条连接**上画。

## 快速开始

```bash
cargo run -- --init      # 装一份能用的：写配置 + 下载词库（几十 MB）
cargo run                # 跑起来
```

然后把焦点放进输入框，敲 `n i h a o` 再按空格。能不能用取决于**应用自己有没有实现
text-input 协议**：Firefox / GTK / Qt 应用都行，alacritty 0.17（基于 winit）实测也可以。

词库默认用[白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)（约 100 万条，自带字频词频，
多音字一行一个读音；资产 27 MB）。不想下载现成的、想自己从语料构建：`pliers dict build`；
换源或离线装：`PLIERS_DICT_URL=... pliers dict fetch --url file:///...`；
看现在装的是哪份：`pliers dict status` —— 见 [docs/dictionary.md](docs/dictionary.md)。
发布出来的词库资产按 GPL-3.0 分发（语料是 GPL-3.0 的），代码本身不受影响。

配置默认是全拼，不用写任何文件；要一份带注释的模板就 `pliers --init-config`。
跑着的时候另开一个终端 `pliers status` 看现状、`pliers set` 上下选着改 —— 不用重启。

**同一个 seat 上只能有一个输入法**：起第二个实例时旧实例会打印「有另一个输入法接管了这个
seat」然后退出，所以别同时跑两个。

想确认自己没跑歪：

```bash
cargo test --workspace        # 162 个单测，不需要合成器、不需要词库
./tools/run_mock_tests.sh     # mock 合成器跑 24 个场景 + 2 项在线改配置检查
```

## 按键速查

| 按键 | 行为 |
| --- | --- |
| `a`–`z`（小写） | 组词；**只有小写参与匹配**，大写一律当英文字符 |
| 空格 | 上屏选中的候选（可能是"只匹配了前面一段"的分段候选） |
| `←` `→` `↑` `↓` / `Tab` | 挪选中的候选，挪出这一页会自动翻页 |
| `,` `.` / `-` `=` / `PageUp` `PageDown` | 整页翻 |
| `1`–`9` | 选这一页的第几个 |
| 其他符号 | **先把候选上屏、再把符号转给应用**（反了屏幕上就是 `,你好`） |
| 大写字母 | 没组词 → 原样转发；组词当中 → 并进预编辑，整串当英文原文 |
| 回车 / Esc | 原样提交 / 取消这次组词（这个键不给应用） |
| 退格 | 删掉一个字符 |
| `Del` | 删掉**自己拼出来的**候选（词库里原本就有的删不了） |
| `Ctrl` + 空格 | 切中英文（切换时半截拼音先上屏） |
| Ctrl / Alt / Super + 任何键 | 不组词，原样转发 |

完整行为、中文标点、分段上屏的"记性"见 [docs/usage.md](docs/usage.md)。

## 文档

| 文档 | 讲什么 |
| --- | --- |
| [docs/usage.md](docs/usage.md) | 按键全表、大写字母为什么不参与匹配、中英文切换、中文标点、分段上屏 + 记性、`Del` 的语义 |
| [docs/config.md](docs/config.md) | `config.toml` 每一项（全拼/双拼/码表/engine）、`pliers set` vs `pliers config set`、自动重读、交互模式、Nushell 写法 |
| [docs/dictionary.md](docs/dictionary.md) | 词库怎么装（`pliers --init`）、换成别的源、自己从语料构建、表结构与权重、用 SQL 加词、`lookup` 看候选 |
| [docs/internals.md](docs/internals.md) | crate 划分、用了哪些 Wayland 协议、**为什么拼音查询不能交给 SQL**、整句候选、候选框是怎么画出来的 |
| [docs/pitfalls.md](docs/pitfalls.md) | 10 条实测踩出来的坑（Ctrl+A 被吃掉、焦点一走拼音就没了……）+ 已知不足 |
| [docs/testing.md](docs/testing.md) | 单测、跑起来、`PLIERS_DEBUG`、mock 合成器的 24 个场景都验了什么 |

## 代码结构

一个 cargo workspace，按"谁管什么"切成五块（详细的文件分工见 [docs/internals.md](docs/internals.md)）：

| crate | 管什么 |
| --- | --- |
| `crates/pliers-engine` | 按键状态机 + 输入方案（全拼/双拼/码表）+ 词库 + 配置 |
| `crates/pliers-popup` | 候选框长什么样：找字体、排版、画像素、共享内存文件 |
| `crates/pliers-wayland` | 跟合成器说协议：注册、抓键盘、转发按键、贴候选框 |
| `crates/pliers-ime` | `main()`：读配置、把上面几个拼起来（命令是 `pliers`） |
| `crates/pliers-dict` | 导入工具：rime 词库（`.dict.yaml`）→ SQLite 词库 |

这么切是为了**输入方案、词库、候选框长相都能脱离合成器跑测试**，协议层里只剩下
"Wayland 要求这么做"的东西：

* 换输入方案 → 改 `~/.config/pliers/config.toml`；加一套新方案 → 在 `scheme.rs` 里实现 `Scheme`
* 加词 / 调词频 → 用 SQL 改词库，或者重新跑一遍 `pliers-dict`
* 换候选框长相（甚至换成 egui/Slint 画）→ `crates/pliers-popup`
* 加协议功能（比如 `delete_surrounding_text`）→ `crates/pliers-wayland`

## 名字

**pliers / 钳子** —— 螃蟹那对钳子。crates.io 上 `pliers` 已被占，所以主包叫
`pliers-ime`，但**命令还是 `pliers`**（配置目录 `~/.config/pliers/`、环境变量 `PLIERS_*` 也一样短）。
