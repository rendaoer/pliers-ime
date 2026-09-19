# pliers（钳子）

最小可用的 Wayland 中文输入法：敲 `nihao` + 空格 → 输出「你好」，组词时输入框旁边弹出
候选框，`←`/`→` 挪选中的那个、`↑`/`↓` 整页翻，也可以直接按数字 `1`–`9`；空格上屏。
中文模式下打英文单词也有补全：`hel` + 空格 → `help`（`kuber` → `kubernetes`）。

* **全拼**是完整实现的：150 万词的词库、音节切分、**整句候选**、词频排序、用户调频
* **英文候选**：两万五千个词的词表是自己的一个 SQLite 库（`~/.local/share/pliers/english.db`），
  打一半就补全（`hel` → help/hello），可以 `pliers fetch english` **单独更新**，不用重装输入法；
  打 `shou` 这种还在拼音上的串永远只出中文（见[规则](docs/usage.md#英文单词补全英文候选)）
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

从源码构建要一个系统库：**libxkbcommon** 的开发包（Debian/Ubuntu 是 `libxkbcommon-dev`，
Arch 是 `libxkbcommon`，Fedora 是 `libxkbcommon-devel`）—— 少了它链接会报
`unable to find library -lxkbcommon`。用 `cargo install pliers-ime` 装的话，编的人是你自己，
所以一样要装。

（发到 crates.io 之后也可以直接装：`cargo install pliers-ime` —— 装出来的命令叫 `pliers`）

然后把焦点放进输入框，敲 `n i h a o` 再按空格。能不能用取决于**应用自己有没有实现
text-input 协议**：Firefox / GTK / Qt 应用都行，alacritty 0.17（基于 winit）实测也可以。

词库默认用[白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)（约 100 万条，自带字频词频，
多音字一行一个读音；资产 27 MB）。不想下载现成的、想自己从语料构建：`cargo install pliers-dict` 之后 `pliers dict build`；
换源或离线装：`PLIERS_DICT_URL=... pliers fetch pinyin --url file:///...`；
看现在装的是哪份：`pliers dict status`；想把词库和英文词表**一起更新到最新**：`pliers update`
（已经是最新的会跳过，不白下 27 MB）—— 见 [docs/dictionary.md](docs/dictionary.md)。
发布出来的词库资产按 GPL-3.0 分发（语料是 GPL-3.0 的），代码本身不受影响。

**词库、用户数据、英文词表是三个文件**，各管各的：

| 文件 | 是什么 | 怎么更新 |
| --- | --- | --- |
| `~/.local/share/pliers/dict.db` | 中文词库 + 音节表（86 MB） | `pliers fetch pinyin --force` / `pliers dict build` |
| `~/.local/share/pliers/user.db` | 你选过的词、自己拼的句子、按 `Del` 拉黑的词 | 自动写；想清空就删掉它 |
| `~/.local/share/pliers/english.db` | 英文候选词表（SQLite，25223 词） | `pliers fetch english`（**可以单独更新**） |

所以重新装词库不会弄丢你的习惯，换英文词表也不用动词库；两个库内部用 SQLite 的 `ATTACH`
连起来（见 [docs/dictionary.md](docs/dictionary.md#表结构词库和用户数据是两个文件)）。
英文词表没装/读不了时，引擎会用二进制里那份兜底，英文候选不会凭空消失。
英文那个库是**启动时一次性读进内存**的（两万五千行，几十毫秒），每次按键还是内存里扫一遍 ——
用 SQLite 当存储和发布格式，不等于把每次按键的查询交给 SQL。

配置默认是全拼，不用写任何文件；要一份带注释的模板就 `pliers --init-config`。
跑着的时候另开一个终端 `pliers status` 看现状、`pliers set` 上下选着改 —— 不用重启。

**同一个 seat 上只能有一个输入法**：起第二个实例时旧实例会打印「有另一个输入法接管了这个
seat」然后退出，所以别同时跑两个。

想确认自己没跑歪：

```bash
cargo test --workspace        # 213 个单测，不需要合成器、不需要词库
./tools/run_mock_tests.sh     # mock 合成器跑 27 个场景 + 2 项在线改配置检查
```

## 按键速查

| 按键 | 行为 |
| --- | --- |
| `a`–`z`（小写） | 组词；**只有小写参与匹配**，大写一律当英文字符 |
| 打英文单词 | 给补全候选（`hel` → help/hello/hell，`kuber` → kubernetes）：切不出拼音时才出，空格上屏选中的那个 |
| 空格 | 上屏选中的候选（可能是"只匹配了前面一段"的分段候选） |
| `←` `→` / `Tab` / `Shift+Tab` | 挪选中的候选（挪出这一页会自动翻页） |
| `↑` `↓` / `,` `.` / `-` `=` / `PageUp` `PageDown` | 整页翻，页内位置保持 |
| `,` `.` / `-` `=` / `PageUp` `PageDown` | 整页翻 |
| `1`–`9` | 选这一页的第几个 |
| 其他符号 | **先把候选上屏、再把符号转给应用**（反了屏幕上就是 `,你好`） |
| 大写字母 | 没组词 → 原样转发；组词当中 → 并进预编辑，整串当英文原文 |
| 回车 / Esc | 原样提交 / 取消这次组词（这个键不给应用） |
| 退格 | 删掉一个字符；**按住不放会一直删**（跟普通键盘一样） |
| `Del` | 删掉**自己拼出来的**候选（词库里原本就有的删不了）；按住不放会重复 |
| `Ctrl` + 空格 | 切中英文（切换时半截拼音先上屏） |
| Ctrl / Alt / Super + 任何键 | 不组词，原样转发 |

完整行为、中文标点、英文候选的规则、分段上屏的"记性"见 [docs/usage.md](docs/usage.md)。

## 文档

| 文档 | 讲什么 |
| --- | --- |
| [docs/usage.md](docs/usage.md) | 按键全表、大写字母为什么不参与匹配、中英文切换、中文标点、分段上屏 + 记性、`Del` 的语义 |
| [docs/config.md](docs/config.md) | `config.toml` 每一项（全拼/双拼/码表/engine/english）、`pliers set` vs `pliers config set`、自动重读、交互模式、Nushell 写法 |
| [docs/dictionary.md](docs/dictionary.md) | 词库怎么装（`pliers --init`）、换成别的源、自己从语料构建、表结构与权重、用 SQL 加词、`lookup` 看候选 |
| [docs/internals.md](docs/internals.md) | crate 划分、用了哪些 Wayland 协议、**为什么拼音查询不能交给 SQL**、整句候选、长按重复为什么得自己做、候选框是怎么画出来的 |
| [docs/pitfalls.md](docs/pitfalls.md) | 11 条实测踩出来的坑（Ctrl+A 被吃掉、松手后应用以为键还按着……）+ 已知不足 |
| [docs/testing.md](docs/testing.md) | 单测、跑起来、`PLIERS_DEBUG`、mock 合成器的 27 个场景都验了什么 |

## 代码结构

一个 cargo workspace，按"谁管什么"切成五块（详细的文件分工见 [docs/internals.md](docs/internals.md)）：

| crate | 管什么 |
| --- | --- |
| `crates/pliers-engine` | 按键状态机 + 输入方案（全拼/双拼/码表）+ 词库 + 英文词表 + 配置 |
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

## 许可

* **代码**：[MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE)，随你挑一个用
* **词库资产**（Release 里的 `dict.db.zst`）：**GPL-3.0** —— 它是
  [rime-frost](https://github.com/gaboolic/rime-frost) 那份 GPL-3.0 语料的衍生作品，
  跟代码的许可是两回事。不想碰它就 `pliers dict build` 自己从上游构建（那只是下载语料，
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
