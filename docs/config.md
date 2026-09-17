# 配置：TOML 文件 + 运行中改动

[← README](../README.md) · 相关：[使用](usage.md) · [词库](dictionary.md)

行为全在配置文件里 —— 换方案、换词库、改候选个数、改切换键都不用动代码。

## 配置文件在哪

`~/.config/pliers/config.toml` —— 注意是 **config** 目录，词库在 `~/.local/share/pliers/`。
没有这个文件就用内置默认值（全拼）。想要一份带注释的模板：

```bash
pliers --init-config        # 写到 ~/.config/pliers/config.toml，已存在就不覆盖（--force 强制）
```

模板本身也躺在仓库里：`crates/pliers-engine/config.example.toml`（`--init-config` 写的就是它，
代码里是 `include_str!` 嵌进去的，不会两边漂移）。最小的一份：

```toml
[scheme]
kind = "full-pinyin"      # full-pinyin | double-pinyin | table

[dict]
# path = "~/.local/share/pliers/dict.db"
# max_candidates = 9
```

改完**不用重启**：跑着的实例每 0.7 秒看一眼文件的 mtime，自动重读（见[下面](#跑着的时候改配置不用重启)）。

## 输入方案

### 全拼

默认就是它，什么都不用写。查询规则见[实现](internals.md#全拼的几条规矩)。

### 双拼

三套预设 + 自己写键位：

```toml
[scheme]
kind = "double-pinyin"
layout = "natural"        # natural(自然码) | flypy(小鹤) | mspy(微软双拼) | none(空表)

# 可选：在预设上改几个键（layout = "none" 时这里就是全部键位）
[scheme.keys]
ao = "c"                  # 把 ao 从 k 挪到 c
zh = "v"                  # zh/ch/sh 是声母，别的名字都当韵母
```

自然码下 `nihk` → 你好（`hao` 在 `k` 键上），小鹤是 `nihc`。零声母（`ang` → `ah`）
这类规则在 `crates/pliers-engine/src/scheme.rs` 的 `encode()` 里，一个音节两键。

键位写错了会在**启动时**报错，而不是等你打字打不出来才发现 —— 它会反推出你漏了哪个韵母：

```text
Error: "这套双拼键位缺韵母：ai an ang ei en eng ia ian iang iao ie in 等 26 个
        （把它们补进 [scheme.keys]，比如 `ai = \"x\"`）"
```

（单键的感叹词音节 `m`/`n`/`ng` 双拼本来就没法打，不算漏。）

零声母音节（安 爱 昂 欧 恩 儿 …）**两种打法都认**：小鹤自家的规则是"首字母 + 韵母键"
（安 = `aj`、爱 = `ad`），Rime / fcitx5 / 搜狗那边习惯直接打全拼（安 = `an`、爱 = `ai`）——
两种都收，按哪个习惯打都行（`ou` `en` `er` 本来就是全拼，重复的那个自动跳过）。

### 五笔 / 码表方案

五笔（以及郑码、仓颉这类码表方案）走 `table`：

```toml
[scheme]
kind = "table"
name = "wubi"             # 词库里 word.scheme 用哪个名字
```

码表用 pliers-dict 的 `--table` 导入：`--table wubi.txt --table-scheme wubi`
（每行 `词<TAB>码[<TAB>权重]`，rime 那种 .txt 码表就是这个格式）。仓库里**没有**带码表数据，
所以这条路目前只有骨架 —— 见[已知不足](pitfalls.md#已知不足)。

### 整句候选（全拼/双拼共用）

库里没有整词时，把音节切成词拼一句（`nihaoma` → 你好吗），默认开着：

```toml
[scheme]
sentence = true           # 想要"只出词库里真有的词"就设 false
```

做法和局限见[实现](internals.md#整句候选)。

## 词典与候选数量

```toml
[dict]
path = "~/.local/share/pliers/dict.db"   # 也可以指向任何一份词库副本
max_candidates = 9                       # 一页显示几个候选
pool_size = 90                           # 一次准备多少个候选 = 最多能翻多少页（90 就是 10 页，得 ≥ max_candidates）
```

`path` 认 `~`；也可以整个用环境变量顶掉：`PLIERS_DICT=/path/to/dict.db`。
词库怎么生成见[词库](dictionary.md)。

## 输入习惯（engine）

```toml
[engine]
toggle_keys = ["ctrl+space"]   # 中英切换键：ctrl+space / shift（轻按）；[] = 不切
start_mode = "chinese"         # 启动模式：chinese | english
indicator = true               # 切换时那个「中」/「英」小方块
chinese_punctuation = true     # 中文模式下 , → ，
```

这几项的运行时效果见[使用](usage.md#中英文切换)和[使用](usage.md#中文标点)。

## 跑着的时候改配置（不用重启）

输入法起来之后会在 `$XDG_RUNTIME_DIR/pliers.sock` 上听命令，另开一个终端就能问它、改它：

```bash
pliers status                             # 现在什么方案、哪个词库、几个候选、中还是英
pliers set                                # 交互模式：上下选着改（最省事）
pliers set scheme.kind double-pinyin      # 只改**正在跑的实例**（重启就回去了）
pliers config set scheme.layout flypy      # 改**配置文件**（保留注释，跑着的实例自动重读）
pliers config set                         # 交互模式，改的也是配置文件
pliers config show | path | edit           # 看内容 / 看路径 / 用 $EDITOR 打开
pliers reload                             # 手动让它重读一遍配置文件
```

**`set` 和 `config set` 是两条路**，想清楚再敲：

| | 改哪儿 | 什么时候失效 | 用途 |
| --- | --- | --- | --- |
| `pliers set …` | 只改内存里那份配置 | 重启就回去 | 试手感（"小鹤换微软试试"） |
| `pliers config set …` | 写进 `~/.config/pliers/config.toml` | 不会失效 | 定下来（写的时候注释、空行、行尾注释都留着） |

**配置文件改了就自动生效**：跑着的实例每 0.7 秒看一眼文件的 mtime，发现变了就重读
（`pliers config set`、`$EDITOR` 里保存、`echo >>` 都算），不用重启、也不用敲 `reload`。
改坏了不慌：新配置建不出引擎就**继续用旧的**，只在 stderr 上吼一句
（`pliers: 配置文件有问题，继续用旧的：…`）。

`pliers status` 一项一行（服务端给的是 tab 分隔的 `名=值`，排版在命令行这边做的，
所以 `socat` 接上去也是一眼看懂的数据）：

```
方案      双拼（小鹤）
词库      /home/dao/.local/share/pliers/dict.db（131 MB）
候选      一页 9 个，池子 90 个
中英切换  ctrl+space
中英提示  开
模式      中（启动时 chinese）
配置文件  /home/dao/.config/pliers/config.toml
socket    /run/user/1000/pliers.sock
```

### 交互模式

`pliers set` 不带参数就是交互模式 —— 上下选的列表，跟那些前端脚手架 CLI 一个手感
（↑↓ 或 j/k 挪、Enter 确认、q/Esc 退出）：

```
$ pliers set
现在跑着的是：
方案         双拼（小鹤）
…

? 要改哪一项？
    输入方案     double-pinyin      full-pinyin / double-pinyin / table
❯   双拼键位     flypy              natural / flypy / mspy / none
    整句候选     开                 true / false
    码表名       —                  手输
    词库文件     /home/dao/.local/share/pliers/dict.db   手输
    一页候选数   9                  1 / 2 / 3 / 4 / 5 / 6 / 7 / 8 / 9
    …
    重新读配置文件
    看完整现状
  ↑↓ 选择 · Enter 确认 · q 退出
```

选中一项之后：**有固定选项的**再给一个列表上下选，光标先停在现在这个值上（带绿点 ●）；
**要手输的**（词库路径、池子深度、码表名）给一行输入框、预填现在的值。
中文按两列算宽度，所以列是对齐的；`NO_COLOR=1` 或重定向到文件时自动不上色。

改完把结果和新的现状打在滚动历史里，回到列表时**光标停在你刚改的那项上** —— 连着调几项不用重新挪。

没终端时（管道、脚本、CI）自动退回行式：把项列出来（带 `<项>` 名字），输编号或直接
`<项> <值>` —— 这样交互模式也能自动化测（mock 那套测试就是两边都跑一遍）。

### 几个要点

* **`set` 只改正在跑的那个实例，不写配置文件** —— 下次启动还是配置文件里的值。
  想让它长期生效，就把同样的值抄进 `~/.config/pliers/config.toml`。
* 改完**立刻**生效：`set scheme.kind double-pinyin` + `set scheme.layout flypy` 之后，
  下一个键就按小鹤解。mock 测试验的就是这个：客户端跑着的时候切方案，再喂 `nihc` → 提交「你好」。
* 值写错了会**整个退回**，正在跑的实例不受影响：`scheme.layout 只认 natural（自然码）/
  flypy（小鹤）/ mspy（微软）/ none，给的是 "flpy"`（退出码 1）。`config set` 同一个校验，
  验不过**不写文件**。
* 中/英模式在 `reload`/`set` 之后**保持不变**（不会跳回 `start_mode`）；
  打到一半的拼音也留着 —— 重建引擎时会把它塞回去、按新方案重查候选。
* `pliers set` 不连合成器 —— 它只跟已经跑着的那个实例说话，不会抢 seat。

实现就一条 Unix socket 上的一问一答（纯文本，`socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/pliers.sock`
也能直接跟它说话）。主循环用 `poll(2)` 同时等 Wayland socket 和它，所以没开线程、也没加定时器 ——
见 `crates/pliers-wayland/src/control.rs` 和 `poll.rs`。socket 路径可以用 `PLIERS_SOCKET` 改。

## 命令与 shell（Nushell 等）

文档里所有命令在 bash 和 Nushell 里都能直接跑（在 Nushell 0.115 上验过）。
只有三处写法两边不一样，文档里已经都避开了：

| 想干的事 | bash / zsh | Nushell |
| --- | --- | --- |
| 临时设环境变量再跑 | `PLIERS_DEBUG=1 cargo run` | 一样能写（nushell ≥0.99）<br>老版本用 `with-env { PLIERS_DEBUG: 1 } { cargo run }` |
| 连着跑两条 | `a && b` | `a; b`（nushell **不支持** `&&`） |
| 命令换行接着写 | `cmd \` | nushell **不支持** `\` 续行，写成一行或用括号 |

`~` 在两边都会展开（包括传给 `cargo` / `curl` 这种外部命令的参数），`mkdir` 在 Nushell 里
本身就会建中间目录，不用 `-p`。
