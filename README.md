# pliers

最小可用的 Wayland 中文输入法：敲 `nihao` + 空格 → 输出「你好」，组词时输入框旁边弹出
候选框，`↓`/`Tab` 或数字 `1`–`9` 挑，空格上屏选中的那个。

* **全拼**是完整实现的：150 万词的词库、音节切分、词频排序、用户调频
* **双拼 / 五笔**留好了位置：双拼（自然码/小鹤/微软）已经能用，五笔是"码表方案"那条路
* **行为都在配置文件里**（TOML）：换方案、换词库、改候选个数都不用改代码
* **词库在 SQLite 里**：用 [turso](https://github.com/tursodatabase/turso)（Rust 写的 SQLite）读写，
  带权重字段，加词/调权重/看数据都能直接用 SQL

没有 GUI 框架：候选框是自己往 `wl_shm` 画像素（见「候选框是怎么做出来的」）。

名字：**pliers / 钳子** —— 螃蟹那对钳子。crates.io 上 `pliers` 已被占，所以主包叫
`pliers-ime`，但**命令还是 `pliers`**（配置目录 `~/.config/pliers/`、环境变量 `PLIERS_*` 也一样短）。

它直接用 `wayland-client` 跟合成器说协议，不经过 imekit 之类的封装 —— 因为候选框要用
`zwp_input_popup_surface_v2`，而那个 surface 必须在**创建它的那条连接**上画。

## 代码结构

一个 cargo workspace，按"谁管什么"切成四块：

| crate | 管什么 | 依赖 |
| --- | --- | --- |
| `crates/pliers-engine` | 按键状态机 + 输入方案（全拼/双拼/码表）+ 词库 + 配置 | turso / toml / serde |
| `crates/pliers-popup` | 候选框长什么样：找字体、排版、画像素、共享内存文件 | ab_glyph / fontdb |
| `crates/pliers-wayland` | 跟合成器说协议：注册、抓键盘、转发按键、贴候选框 | wayland-client / xkbcommon |
| `crates/pliers` | `main()`：读配置、把上面几个拼起来 | pliers-engine + pliers-wayland |
| `crates/pliers-dict` | 导入工具：词表 + 词频表 → SQLite 词库 | turso |

```
pliers-engine  ←──┬──  pliers-popup  ──┐
               └────────────────┴──  pliers-wayland  ←──  pliers (bin)
     ▲
     └──  pliers-dict (bin)：往词库里灌数据
```

`pliers-engine` 里面按"一件小事一个文件"分：

| 文件 | 管什么 |
| --- | --- |
| `lib.rs` | 按键状态机：什么键进 buffer、什么时候上屏、按键账本 |
| `scheme.rs` | 输入方案：[`Scheme`] trait + 全拼 / 双拼 / 码表三套实现 |
| `pinyin.rs` | 音节切分：`nihao` → `ni hao`，`nih` → `ni ha?` 补全 |
| `dict.rs` | SQLite 词库：建表、查词、记用户词频 |
| `config.rs` | 读 TOML 配置 |

这么切的好处：**输入方案、词库、候选框长相都能脱离合成器跑测试**（52 个单测），
协议层里剩下的全是"Wayland 要求这么做"的东西。想改哪块就只动哪块：

* 加词 / 调词频 → 用 SQL 改词库，或者重新跑一遍 `pliers-dict`
* 换输入方案（双拼、五笔）→ 改 `~/.config/pliers/config.toml`
* 加一套新方案（郑码、仓颉、注音）→ 在 `crates/pliers-engine/src/scheme.rs` 里实现 `Scheme`
* 换个候选框长相（甚至换成 egui/Slint 画）→ `crates/pliers-popup`
* 加协议功能（比如 `delete_surrounding_text`）→ `crates/pliers-wayland`

`crates/pliers-wayland` 内部也分了三个文件：`lib.rs`（状态 + 各个 Dispatch 实现）、
`keyboard.rs`（keycode→keysym、修饰键状态）、`popup.rs`（候选框 surface）。

## 用了哪些协议

| 协议 | 作用 |
| --- | --- |
| `zwp_input_method_manager_v2` / `zwp_input_method_v2` | 以输入法身份注册；输入框获得焦点时收到 `activate` |
| `zwp_input_method_keyboard_grab_v2` | 抓键盘：抓到手之后所有按键先送到我们这里，合成器不再自己处理 |
| `zwp_virtual_keyboard_v1` | 虚拟键盘：不归我们管的按键，用它原样发回给应用 |
| `zwp_input_popup_surface_v2` + `wl_shm` | 候选框：自己往共享内存画像素，位置交给合成器 |
| `wl_output` | 只为了问一句"屏幕缩放多少"：1.5x/2x 的屏幕要按倍数多画像素，不然字是糊的 |

按键判断用的是 **keysym**（X11 的字符编号：空格 `0x20`、`a`–`z` 是 `0x61..=0x7a`、
退格 `0xff08`），但合成器只给 **keycode**（Linux evdev，空格是 57）。中间的翻译靠合成器
发来的 XKB 键盘布局 + xkbcommon；同一份布局还会原样转给虚拟键盘，这样应用收到的字符
跟真实键盘布局一致。

## 先导入词库（只做一次）

词库不在仓库里（150 万词，上百 MB），得自己生成：拿一份拼音词表 + 一份词频表灌进 SQLite。

```bash
# 词频表：jieba 的（MIT 许可，35 万词带频次）。没有它也能导入，
# 只是所有词的权重都一样，排序会很难看（打 shijian 第一个出「世鉴」）
curl -o jieba-dict.txt https://raw.githubusercontent.com/fxsjy/jieba/master/jieba/dict.txt

cargo run -p pliers-dict --release -- --source ~/Downloads/CustomPinyinDictionary_IBus.txt --freq jieba-dict.txt --out ~/.local/share/pliers/dict.db
```

三份数据各管一件事（`--source` 是**唯一**必需的参数）：

| 来源 | 提供 | 为什么需要 |
| --- | --- | --- |
| IBus 拼音词表 | 150 万条「词 + 拼音」 | 词库本体 |
| jieba 词频表 | 权重 | 词表本身**没有词频**，不给权重就只能按拼音字典序排 |
| 词表自己 | 单字 | 表里全是 2 字以上的词，单字靠"字↔音节"对齐推出来 |

导入一次大概几分钟（`--release` 快很多），生成的库 ~130 MB。之后想加词就直接写 SQL ——
**但得先把输入法退掉**：turso 开着的时候会一直占着这个库，别的进程连只读连接都进不去
（`database is locked`）。只想看看数据就拷一份出来读：

```bash
# 加词 / 改权重（先退出输入法，否则 database is locked）
sqlite3 ~/.local/share/pliers/dict.db "INSERT OR REPLACE INTO word (scheme, code, text, weight) VALUES ('pinyin','ni hao','你好',3000000);"

# 想把用户词频清零（同样要先退出输入法）
sqlite3 ~/.local/share/pliers/dict.db "DELETE FROM user_word;"

# 只想看看数据（输入法开着也能读，因为读的是副本）。
# 一定要连 -wal 一起拷：最新的写入还在 WAL 里，只拷 .db 会看到旧数据
mkdir -p /tmp/dbcopy
cp ~/.local/share/pliers/dict.db     /tmp/dbcopy/
cp ~/.local/share/pliers/dict.db-wal /tmp/dbcopy/    # 没有这个文件就是刚 checkpoint 过，跳过

sqlite3 /tmp/dbcopy/dict.db "SELECT text, count, last_used FROM user_word ORDER BY last_used DESC LIMIT 10;"
```

## 运行与测试

```bash
cargo run                  # 跑主程序（workspace 里默认就跑 pliers）
cargo test --workspace     # 引擎 + 候选框的单测（52 个，不需要合成器、不需要词库）
cargo test -p pliers-engine   # 只看引擎：切词、方案、词库、按键状态机
./tools/run_mock_tests.sh  # 拿 mock 合成器把 15 个场景跑一遍（含候选框像素）
```

只想看候选框长什么样、不想开输入法：

```bash
cargo run -p pliers-popup --example dump_popup   # 存成 target/popup.png
```

然后把焦点放到输入框里，敲 `n i h a o` 再按空格。能不能用取决于**应用自己有没有
实现 text-input 协议**：Firefox / GTK / Qt 应用都行，alacritty 0.17（基于 winit）
实测也可以。

排查按键问题时加个环境变量，它会把每个按键的判定过程打到 stderr：

```bash
PLIERS_DEBUG=1 cargo run
# pliers: 收到 keycode=30 keysym=0x0061 按下（shift=false caps=false ctrl/alt/super=false 预编辑=""）
# pliers:   → 预编辑 "a"
```

注意**同一个 seat 上只能有一个输入法**：起第二个实例时，合成器会给旧的那个发
`unavailable`（smithay 的 `InputMethodHandle::add_instance`），旧实例会打印
「有另一个输入法接管了这个 seat」然后退出。所以别同时跑两个。

（词库本身不会打架：turso 允许同一个库开多个连接，我试过两个连接一个写一个读都正常。
真正冲突的只有 Wayland 那个 seat。）

| 按键 | 行为 |
| --- | --- |
| `a`–`z` / `A`–`Z`（Shift、Caps Lock 打出来的大写也一样） | 攒进 buffer，用 `set_preedit_string` 显示为预编辑文本 —— 打字中途**不会**往应用里塞任何字符 |
| 空格 | 有内容就把**选中的候选**提交（`nihao` → 你好，大小写不敏感，`NIHAO` 也算），否则当普通空格转发给应用 |
| `←` `→` `↑` `↓` / `Tab` / `Shift+Tab` | 挪选中的候选（绕圈）。**挪出这一页会自动翻页**；组词当中方向键不去动输入框里的光标 |
| `,` `.` / `-` `=` / `PageUp` `PageDown` | 整页翻，页内位置保持（`-` `=` 是微软拼音的习惯） |
| `1`–`9` | 选**这一页**的第几个上屏；这一页没那么多就什么也不做 |
| 回车 / 小键盘回车 | 有内容就原样提交，**不**把回车给应用（免得顺手提交表单） |
| Esc | 有内容就取消这次组词，这个键谁也不给（免得顺手退出全屏） |
| 退格 | 删掉一个字符 |
| `Ctrl` + 空格 | **切中英文**（这个键不给应用）。想换成轻按 Shift 或两个都要，见下面的 `[engine]` |
| Ctrl / Alt / Super + 任何键 | **不组词**，原样转发（Ctrl+A 全选、Ctrl+C 中断、Alt+F 菜单……） |
| 其他键 | 用虚拟键盘原样转发，普通打字不受影响 |

所以 `aaaa` 之后按住 Shift 敲 `AAAA`，预编辑里就是 `aaaaAAAA`（候选框跟着变长），
按空格/回车才整体落到输入框；查不到的内容原样提交，大小写照旧保留。

### 中英文切换

**Ctrl + 空格** 切换（中文输入法的老习惯），切换时在光标处弹一个小方块显示「中」/「英」：

| 模式 | 行为 |
| --- | --- |
| 中文 | 现在这样：字母进组词，空格上屏候选 |
| 英文 | 输入法等于不存在：**所有按键原样转发** —— 打英文、写代码、按快捷键都不受影响 |

提示只是复用候选框画的一个方块（不带序号），你按下下一个键它就收掉；切换时正在组的词会取消。
配置：

```toml
[engine]
# 可以写多个，也可以只留一个；空数组 [] = 不要切换键（那就一直是中文）
toggle_keys = ["ctrl+space", "shift"]   # shift = 轻按一下（按下到抬起之间没按别的键才算）
start_mode = "chinese"                  # 也可以 "english"：启动就进英文
indicator = true                        # 不想要那个「中」/「英」提示就设 false
```

为什么不做成"自动识别英文"：猜错了比不让切更烦人。要打英文时按一下 Ctrl+空格，是确定的。

### 输入方案（配置文件）

行为定义在 `~/.config/pliers/config.toml` —— 注意是 **config** 目录，词库在 `~/.local/share/pliers/`。
没有这个文件就用内置默认值（全拼）。想要一份带注释的模板：

```bash
pliers --init-config        # 写到 ~/.config/pliers/config.toml，已存在就不覆盖（--force 强制）
```

模板本身也躺在仓库里：`crates/pliers-engine/config.example.toml`（`--init-config` 写的就是它，
代码里是 `include_str!` 嵌进去的，不会两边漂移）。配置内容：

```toml
[scheme]
kind = "full-pinyin"      # full-pinyin | double-pinyin | table

[dict]
# path = "~/.local/share/pliers/dict.db"
# max_candidates = 9
```

**双拼**已经能用，三套预设 + 自己写键位：

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

**五笔**（以及郑码、仓颉这类码表方案）走 `table`：

```toml
[scheme]
kind = "table"
name = "wubi"             # 词库里 word.scheme 用哪个名字
```

码表用 pliers-dict 的 `--table` 导入：`--table wubi.txt --table-scheme wubi`
（每行 `词<TAB>码[<TAB>权重]`，rime 那种 .txt 码表就是这个格式）。仓库里**没有**带码表数据，
所以这条路目前只有骨架。

### 用 Nushell？

上面所有命令在 bash 和 Nushell 里都能直接跑（我在这台机器上用 nushell 0.115 验过）。
只有三处写法两边不一样，README 里已经都避开了：

| 想干的事 | bash / zsh | Nushell |
| --- | --- | --- |
| 临时设环境变量再跑 | `PLIERS_DEBUG=1 cargo run` | 一样能写（nushell ≥0.99）<br>老版本用 `with-env { PLIERS_DEBUG: 1 } { cargo run }` |
| 连着跑两条 | `a && b` | `a; b`（nushell **不支持** `&&`） |
| 命令换行接着写 | `cmd \` | nushell **不支持** `\` 续行，写成一行或用括号 |

`~` 在两边都会展开（包括传给 `cargo` / `curl` 这种外部命令的参数），`mkdir` 在 Nushell 里
本身就会建中间目录，不用 `-p`。

## 词库为什么是 SQLite，以及全拼是怎么查的

这一节是踩出来的，如果你要自己写一个输入法，**建议照这个思路走**。

### 一条反直觉的结论：拼音查询不能交给 SQL

最自然的写法是把拼音存成 `nihao`（或者 `ni hao`），然后：

```sql
SELECT text FROM word WHERE code LIKE 'ni%' ORDER BY weight DESC LIMIT 9
```

在 20 万行的表上实测（`cargo run -p pliers-engine --example turso_bench` 可以自己跑一遍）：

| 查询 | 耗时 | 说明 |
| --- | --- | --- |
| `code = 'ni hao'` | **64 µs** | 精确匹配，走索引，随便打 |
| `code LIKE 'ni ha%'` | 46 µs | 范围很窄，还行 |
| `code LIKE 'ni%'` | **280 ms** | 扫出几万行再排序 —— 打一个字母卡半秒 |
| 建索引（20 万行） | 40 s | 所以导入是分钟级的 |

`ni%` 这种宽前缀是致命的：**输入法每敲一个键都要查一次**。所以正确的分工是：

* **切词在内存里做**（`pinyin.rs`，412 个音节，几微秒）：`nihao` → `ni hao`，
  `nih` → 尾巴 `h` 补成 `ha/hai/…/hao`
* **数据库只回答"这个完整的码对应哪些词"**（`dict.rs`，每次 60 µs 上下）
* 一次输入对应好几个码（`xian` 既是「先」也是「西安」；`nih` 要试 20 多个补全），
  查完在 Rust 里**按分数合并去重**（`scheme.rs` 的 `merge()`）

一次按键最多查 32 个码（`MAX_CODES`），最坏情况 2 ms 上下 —— 打不卡。

### 表结构

```sql
word(scheme, code, text, weight)    -- 词库本体：scheme='pinyin' / 'wubi' / …
user_word(text, count, last_used)   -- 用户选过多少次（调频用）
syllable(syl)                       -- 412 个合法音节，切词用
meta(key, value)                    -- 词库来源、导入时间
```

* `scheme` 字段就是"多方案"的落点：一套方案一批行，互不干扰
* 词库是**只读的派生物**：重新导入不会碰 `user_word`，用户习惯留得住
* 权重 = 词频（jieba 频次 ×10）+ 用户选过的次数 ×100 万（最多算 50 次）。
  所以选过十次的词能压过绝大多数常用词，但压不过「的」「你」这种顶级高频词 ——
  避免误选一次就再也翻不了身

排序就是一句 SQL：

```sql
SELECT w.text, w.weight + MIN(COALESCE(u.count, 0), 50) * 1000000 AS score
FROM word w LEFT JOIN user_word u ON u.text = w.text
WHERE w.scheme = ?1 AND w.code = ?2
ORDER BY score DESC LIMIT ?3
```

选中的词会在 `Engine::pick()` 里写一笔 `user_word`，下次它自己就往前排了 ——
这就是"用户使用频率权重"。

### 全拼的几条规矩

| 输入 | 查什么 | 为什么 |
| --- | --- | --- |
| `nihao` | `ni hao` | 完整切分，精确匹配 |
| `xian` | `xian` + `xi an` | 歧义切法都查，「先」和「西安」都能出来 |
| `nih` | `ni ha`…`ni hao` | 尾巴没打完，补全音节 |
| `shiji` | `shi ji` + `shi jian`/`shi jie`… | 多音节时最后一个音节往后补，「时间」「世界」提前出现 |
| `ni` | 只查 `ni` | **单个完整音节不联想**，否则打 `ni` 会冒出一堆「年/牛/您」 |

多音字只保留**主读音**（从 150 万条词里统计哪个读音出现最多：的 → de 602 次 vs di 31 次），
所以「的确」这种次读音打不出来 —— 见「已知不足」。

## 几个容易踩的坑

1. **抓到的键盘要自己转发。** 抓取一旦生效，合成器就不再处理任何按键，全丢给我们。
   没有输入框在用（焦点在 XWayland 应用、或不支持 text-input 的应用上）时也必须原样
   转发，否则那些应用直接打不了字 —— 见 `pliers-engine` 的 `Engine::on_key()` 开头那个
   `!key.active` 分支。
2. **`key_get_one_sym()` 不做 Control 变换。** 按住 Ctrl 时 `a` 解出来**还是 `0x61`**
   （只有 `key_get_utf8()` 会给出 `\x01`）。所以如果只按 keysym 判断字母，Ctrl+A 会变成
   往拼音里塞一个 `a`：Firefox 全选失灵、终端 Ctrl+C 也发不出去。必须靠修饰键状态区分
   （`shortcut_mods`），这是实测出来的：

   ```
   无修饰键： key_get_one_sym = 0x61 ('a')
   按住 Ctrl： key_get_one_sym = 0x61 ('a')      ← 坑在这
   按住 Ctrl： key_get_utf8    = b'\x01'
   ```

   反过来，**大写的 keysym 也不等于按了 Shift**：Caps Lock 打开时 `a` 同样解成 `A`。
   这个玩具输入法把大小写都收进预编辑，所以暂时不需要用这个区别做判断；但以后要做
   "Shift 切英文模式"之类，就必须靠修饰键状态（`shift_held` / `caps_lock`）而不是看 keysym。
3. **修饰键状态自己算，别指望合成器的 `Modifiers` 事件。** smithay 只在"修饰键状态刚变"
   的那一次按键上才带这个事件（`mods_changed.then_some(...)`），拿它当唯一来源非常脆：
   一旦收不到，`shift_held`/`ctrl` 全是 false —— 表现就是 **Ctrl+A 被当成拼音的 a 吃掉**。
   正确做法跟普通客户端一样：自己拿 keymap 建一个 `xkb::State`，每个按键事件喂
   `update_key(keycode, Down/Up)`，修饰键、Caps Lock 全都从这推出来
   （`crates/pliers-wayland/src/keyboard.rs`）。
   注意 xkb 文档说 `update_key` 和 `update_mask` 不要混用，所以 `Modifiers` 事件只用来
   转发给虚拟键盘，不再喂我们自己的状态。
4. **预编辑状态下别把字符键转发给应用，也别中途往应用里塞字符。** 应用这时正处在
   "组词"状态里，对转发过去的按键反应不可靠：实测 Firefox 里输入 `aaa`（预编辑挂着）
   再按 `Shift+A`，我们明明把 `key(Shift)` + `modifiers(0x1)` + `key(a)` 都发了出去，
   输入框里却 **A 和 a 都不出现**。所以字母（含大写）一律进 buffer 当预编辑，只在
   空格/回车时用 `commit_string` 整体交给应用。转发只留给"命令类"按键：方向键、
   Ctrl/Alt/Super 快捷键，以及没有输入框时的全部按键。
5. **应用的大写字母靠虚拟键盘的 `modifiers` 请求。** smithay 的 `zwp_virtual_keyboard_v1.key()`
   只发 `wl_keyboard.key`，**不会**顺手更新修饰键状态；只有 `modifiers()` 请求才会给应用
   发 `wl_keyboard.modifiers`。所以输入法自己推出掩码后要用
   `serialize_mods()` 算出来再发一遍（变了才发），否则应用收到 `Shift+a` 也只会打小写。
   （字母走预编辑之后，这条主要影响快捷键，比如 Ctrl+C。）
6. **Caps Lock 是靠按键事件翻转的。** 合成器可能只给你一个 `modifiers` 事件说"Lock 位亮了"，
   但真键盘上是先有 Caps Lock 键（keycode 58）的按下，xkb 才翻转 Lock。自己喂 `update_key`
   两种来源就都能覆盖。
7. **提交是双缓冲的。** `set_preedit_string` / `commit_string` 只改「待生效」状态，
   要再调 `commit(serial)` 才生效。`serial` 必须等于合成器已经发过的 `done` 次数
   （协议原文：*must be equal to the number of done events already issued*），
   所以代码里每收到一个 `done` 就 `serial += 1`。
8. **抓取只需要抓一次。** `deactivate` 之后抓取并不会自己消失（只有连接断开或对象
   被销毁才会），每次 `activate` 都重新抓一遍会攒出好几个 grab，按键可能被重复处理。
9. **不用手动 flush。** 自己管连接的好处：`blocking_dispatch()` 内部会先 flush 再读
   socket，事件处理里发出的请求（预编辑、提交、转发按键、贴候选框）下一轮就会写出去。
   用 imekit 时那个「拿 `commit(serial)` 当 flush 用」的 hack 可以删掉了。

## 候选框是怎么做出来的

**协议那半边**（`crates/pliers-wayland/src/popup.rs`）：

1. 有输入框时 `im.get_input_popup_surface(&surface, qh, ())`：合成器给这个 surface
   指派 `input_popup` role。role 一辈子只能定一次（协议：*If the surface already has
   an assigned role, the compositor must issue a protocol error*），所以**不能用任何
   GUI 框架的窗口**——Slint / GTK / Qt / eframe 建的窗口早就是 `xdg_toplevel` 了。
2. **位置不用你管**：niri 会先水平纠偏，再尝试摆在光标下方，放不下就翻到上方
   （`src/handlers/xdg_shell.rs` 的 `PopupKind::InputMethod` 分支）。合成器还会发
   `text_input_rectangle` 告诉你光标相对候选框的位置，那只是个提示，你没法自己挪窗口。
3. **大小 = 你 attach 的 buffer 大小**，内容 = 你自己往 `wl_shm` 里画的像素；
   `attach(None, 0, 0)` 就是隐藏。
4. **缓冲区要轮换。** 候选词每敲一个字就变，不能一边被合成器显示一边改写那块内存。
   所以只在收到 `wl_buffer.release` 之后才重画那块 buffer（`Slot.busy`）：宽度没变
   就原地重画，宽度变了才新建一块，不用的旧块扔掉——这才是双缓冲该有的样子。
5. **HiDPI 自己处理。** 屏幕是 1.5x / 2x 时按倍数多画几倍像素（本机 niri 是 1.5x，
   它报 2），再用 `set_buffer_scale(2)` 告诉合成器"这块 buffer 是 2 倍密度"。
   注意 `damage()` 用的是 surface 坐标（逻辑像素），不是 buffer 像素，要除回去。

**内容那半边**（`crates/pliers-popup`）：

6. **字体去系统里找**：`fontdb` 扫 `/usr/share/fonts` 这些目录，优先 Noto Sans CJK SC
   之类的常见中文字体；找不到就只画个空框，不报错。`PLIERS_FONT=/path/to/font.ttc`
   可以指定一套。
7. **光栅化用 `ab_glyph`**（字符 → 轮廓 → 灰度图）。这里有个坑：它的 `PxScale` 是
   **行高**不是字号，而中文行高是字号的 1.4 倍上下，直接填字号字会小一圈 ——
   代码里先乘一个 `height_unscaled / units_per_em` 换算（`font.rs` 的 `ratio`）。
8. **竖直居中按"墨迹"算**：把所有字的覆盖率图的上下边界并起来取中点，而不是用字体的
   ascent/descent —— 中文行高留白很大，按行高居中看着会偏上。
9. **画布就是一块 ARGB8888**：`wl_shm` 的 ARGB8888 在内存里是 **B,G,R,A**，而且 alpha
   是**预乘**的（50% 透明的白要存成 `(128,128,128,128)`，存成 `(255,255,255,128)`
   会亮一截）。圆角用"点到圆角矩形的距离"当覆盖率，天然抗锯齿（`paint.rs`）。

## 已知不足

* **没有简拼**（`nh` → 你好）、**没有模糊音**（zh=z、an=ang）、**没有联想/整句**。
  这三样都得再动 `scheme.rs` 和词库索引。
* **多音字只有主读音**：单字是从双字词里统计出来的（的→de 对，但「的确」的 di 就没了）。
  真要全，得在导入时给每个字写多条读音。
* **五笔只有骨架**：码表方案（`Table`）读的是 `scheme='wubi'` 的行，但仓库里没有码表数据；
  而且码表查询是**前缀**查询（打 `w` 要出所有 w 开头的字），正好是最慢的那种，
  真要用得在导入时按前缀预先算好 top-N。
* **导入要几分钟**、库 ~100 MB：turso 建索引不快（20 万行 40 秒）。
  词库是一次性投入，但如果你改词频想重新导入，得等。
* 组词当中按 Ctrl/Alt/Super 快捷键时，按键虽然正确转发给应用了，预编辑串还挂在那儿
  （正经输入法会先把拼音提交或取消掉）。
* 翻页只翻 `pool_size`（默认 90 个 = 10 页）那么深，再往后的候选没取。
* 缩放只取整数倍（1.5x 的屏幕用 2 倍 buffer 再让合成器缩回去，够清楚但不是像素级完美）。
* 字体只加载一套，没有逐字回退：某个字缺字形就是空白。

## 联调工具（可选，不需要就删掉 `tools/`）

`tools/mock_compositor.py` 是一个 mock 合成器，实现了 wl_display / wl_registry / wl_seat /
wl_compositor / wl_shm / zwp_input_method_v2 / zwp_virtual_keyboard_v1 的够用部分，
可以在没有真合成器、没有物理键盘的情况下跑通整条协议链路。它不只检查提交了什么文本，
还会**把候选框那块 shm 读回来**（数笔画像素、确认真的画了字），并模拟合成器发
`wl_buffer.release`：

```bash
./tools/run_mock_tests.sh                       # 15 个场景，全绿才算过
./tools/run_mock_tests.sh --png target/popup.png      # 顺便存一张候选框实拍
```

词库默认依次找 `target/dict.db`、`~/.local/share/pliers/dict.db`，也可以 `--dict <路径>` 指定。
测试里的选词会写进 `user_word`，**不想污染自己的词频就指向一份副本**：

```bash
cp ~/.local/share/pliers/dict.db target/dict-copy.db
./tools/run_mock_tests.sh --dict target/dict-copy.db
```

也可以单跑一个场景，第二、三个参数是「要喂的按键」和「期望提交的文本」：

```bash
python3 tools/mock_compositor.py /tmp/mock-wl "nihao " 你好 &
PLIERS_DICT=target/dict.db WAYLAND_DISPLAY=/tmp/mock-wl cargo run
# mock: pre-edit updates: ['n', 'ni', 'nih', 'niha', 'nihao', '']
# mock: popup shown     : [(90, 42), (168, 42), (318, 42), (318, 42), (318, 42)]
# mock: 框里数出 581 个笔画像素（不透明 13298）
# mock: PASS: committed '你好', expected '你好', popup ok
```

第四个参数是场景（`active` / `inactive` / `shortcut` / `shift` / `mixed` / `enter` /
`escape` / `caps` / `pick` / `nav` / `hidpi`，都可以加 `_nomods` 后缀表示"合成器一个
modifiers 事件都不发"）：

| 场景 | 喂什么 | 验什么 |
| --- | --- | --- |
| `active` | `nihao ` | 提交「你好」，候选框宽度随候选变化、框里有字 |
| `pick` | `nihao2` | 数字选词直接上屏第 2 个候选（词库里是「倪浩」），按键连抬起都不转发 |
| `nav` | `nihao` + `↓` + 空格 | 换候选不改预编辑串，提交第 2 个候选 |
| `hidpi` | 同 `active`，但客户端带 `PLIERS_SCALE=2` | 候选框按 2 倍像素画（高 84）且发了 `set_buffer_scale(2)` |
| `inactive` | `nihao ` | 先发 `deactivate`：12 个按键事件全部原样转发、零提交、候选框一次都不弹 |
| `shortcut` | Ctrl+A / Ctrl+C | 8 个事件全部转发，不能被当成拼音吃掉 |
| `mixed` | `aaa` + `Shift+A` + 空格 | 整个 `aaaA` 只在最后提交一次 |
| `caps` | Caps Lock 打开后 `nihao ` | 大写照样能查表 → 「你好」 |
| `enter` / `escape` | `nihao` + 回车 / Esc | 原样提交 / 取消，按键不给应用 |

（`\x08` 是退格、`\x1b` 是 Esc。）

## 调词库/看候选：`lookup`

想确认"打某个拼音到底会出哪些候选、每个键花多久"，不用开输入法：

```bash
cargo run -p pliers-engine --release --example lookup -- shijian
# 方案 pinyin，词库 /home/dao/.local/share/pliers/dict.db（打开用了 1.2ms）
#
# s             1.50ms   [上] 说 三 省 手 谁 受 水 山
# sh            1.13ms   [是] 上 说 时 使 事 市 省 手
# shi          154.46µs  [是] 时 使 事 市 式 师 石 十
# shij         974.04µs  [时间] 世界 世纪 实际 事件 实践 始建 时机 使劲
# shiji        775.36µs  [时间] 世界 世纪 实际 事件 实践 始建 时机 使劲
# shijia       566.44µs  [时间] 事件 实践 始建 世间 世家 施加 视角 市郊
# shijian      842.39µs  [时间] 事件 实践 始建 世间 石匠 识见 诗笺 尸检
#
# 空格 → Commit("时间")
```

加词、改权重之后用它看效果最快。`--config 别的.toml` 可以换一套方案/词库试试。
