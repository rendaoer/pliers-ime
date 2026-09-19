# 实现内幕：结构、协议、查询、候选框

[← README](../README.md) · 相关：[词库](dictionary.md) · [踩坑](pitfalls.md) · [测试](testing.md)

## 代码结构

一个 cargo workspace，按"谁管什么"切成四块：

| crate | 管什么 | 依赖 |
| --- | --- | --- |
| `crates/pliers-engine` | 按键状态机 + 输入方案（全拼/双拼/码表）+ 词库 + 配置 | turso / toml / serde |
| `crates/pliers-popup` | 候选框长什么样：找字体、排版、画像素、共享内存文件 | ab_glyph / fontdb |
| `crates/pliers-wayland` | 跟合成器说协议：注册、抓键盘、转发按键、贴候选框 | wayland-client / xkbcommon |
| `crates/pliers-ime` | `main()`：读配置、把上面几个拼起来（包名是 `pliers-ime`，命令是 `pliers`） | pliers-engine + pliers-wayland |
| `crates/pliers-dict` | 导入工具：rime 词库（`.dict.yaml`）→ SQLite 词库 | turso |

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
| `scheme.rs` | 输入方案：`Scheme` trait + 全拼 / 双拼 / 码表三套实现 |
| `pinyin.rs` | 音节切分：`nihao` → `ni hao`，`nih` → `ni ha?` 补全 |
| `sentence.rs` | 整句候选：音节序列上的最短路径（Viterbi） |
| `dict.rs` | SQLite 词库：建表、查词、记用户词频 |
| `english.rs` | 英文候选：编译进二进制的词表 + 前缀补全 |
| `config.rs` | 读 TOML 配置 |
| `fetch.rs` | 下载 + 解压（`pliers --init` / `pliers build pinyin` 装词库用） |

这么切的好处：**输入方案、词库、候选框长相都能脱离合成器跑测试**（222 个单测），
协议层里剩下的全是"Wayland 要求这么做"的东西。想改哪块就只动哪块：

* 加词 / 调词频 → 用 SQL 改词库，或者重新跑一遍 `pliers-dict`
* 换输入方案（双拼、五笔）→ 改 `~/.config/pliers/config.toml`
* 加一套新方案（郑码、仓颉、注音）→ 在 `crates/pliers-engine/src/scheme.rs` 里实现 `Scheme`
* 换个候选框长相（甚至换成 egui/Slint 画）→ `crates/pliers-popup`
* 加协议功能（比如 `delete_surrounding_text`）→ `crates/pliers-wayland`

`crates/pliers-wayland` 内部也分了几个文件：`lib.rs`（状态 + 各个 Dispatch 实现）、
`keyboard.rs`（keycode→keysym、修饰键状态）、`popup.rs`（候选框 surface）、
`control.rs` + `poll.rs`（那条 Unix socket 和 `poll(2)` 主循环）。

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

## 按住不放：重复得自己做

Wayland 里**键盘自动重复是客户端的事**，不是合成器的：合成器只发一次按下、一次抬起，
另外发一个 `repeat_info`（延迟多少毫秒开始、每秒重复几次），谁收谁自己定时重发。
普通应用（GTK/Qt 的输入框）都各自实现了这一段；输入法抓走键盘之后也一样 ——
抓取接口 `zwp_input_method_keyboard_grab_v2` 就是照抄 `wl_keyboard` 的规矩，
smithay 那边建好抓取对象之后只发一句 `instance.repeat_info(rate, delay)` 就完了。

不实现的表现：按住退格只删一个字母、按住方向键只挪一格、在终端里按住退格也只退一格。
实现是"到点了把那个键当成又按了一次"（`crates/pliers-wayland/src/repeat.rs`），
而且**走同一条路**：引擎吃掉的还是吃掉、该转给应用的还是转发 —— 所以不用知道哪个键是干吗的。
`poll(2)` 的超时取"配置文件轮询"「提示到点」「下一拍重复」三者里最早的那个，
主循环不用加线程也不用加定时器。

两个例外：修饰键不重复（真键盘也不重复，而且按住 Shift 会被当成"轻按 Shift"切模式），
输入法自己的切换键（Ctrl+空格）也不重复（按住不放来回切中英文说不通）。
另外系统卡了一下之后**只补一拍**，不把欠的几十拍一口气补上 —— 不然卡 200ms 会删掉一大段；
要是碰上"既发 repeat_info 又自己重发按键"的合成器（协议上不该，但真有），
收到同一个键的第二次按下就让位，免得更快。

## 一条反直觉的结论：拼音查询不能交给 SQL

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
| 建索引（20 万行） | 40 s | 只跟「重新导入」有关，日常用不到 |

`ni%` 这种宽前缀是致命的：**输入法每敲一个键都要查一次**。所以正确的分工是：

* **切词在内存里做**（`pinyin.rs`，412 个音节，几微秒）：`nihao` → `ni hao`，
  `nih` → 尾巴 `h` 补成 `ha/hai/…/hao`
* **数据库只回答"这个完整的码对应哪些词"**（`dict.rs`，每次 60 µs 上下）
* 一次输入对应好几个码（`xian` 既是「先」也是「西安」；`nih` 要试 20 多个补全），
  查完在 Rust 里**按分数合并去重**（`scheme.rs` 的 `merge()`）

一次按键最多查 32 个码（`MAX_CODES`），最坏情况 2 ms 上下 —— 打不卡。

### 全拼的几条规矩

| 输入 | 查什么 | 为什么 |
| --- | --- | --- |
| `nihao` | `ni hao` | 完整切分，精确匹配 |
| `xian` | `xian` + `xi an` | 歧义切法都查，「先」和「西安」都能出来 |
| `nih` | `ni ha`…`ni hao` | 尾巴没打完，补全音节 |
| `shiji` | `shi ji` + `shi jian`/`shi jie`… | 多音节时最后一个音节往后补，「时间」「世界」提前出现 |
| `ni` | 只查 `ni` | **单个完整音节不联想**，否则打 `ni` 会冒出一堆「年/牛/您」 |

### 整句候选

库里没有整词时，把音节切成词拼一句：

| 输入 | 候选 | 怎么来的 |
| --- | --- | --- |
| `nihaoma` | 你好 + 吗 | 库里没有「你好吗」这个词条 |
| `uijm`（小鹤） | 时间 | 「是」+「见」的单字权重加起来比「时间」高，靠长词奖励压过去 |
| `woshiyigexuesheng` | 我是一个学生 | 我 + 是 + 一个 + 学生 |
| `zhonghuarenmingongheguo` | 中华人民共和国 | 库里正好有整词，直接命中 |

做法是最短路径（Viterbi）：音节序列上每个位置枚举 1–6 个音节的词，每个查一次库（还是精确查询），
边权 `ln(词权重) + 长词奖励 ×(字数-1)`，取总分最高的几条路。**长词奖励是必须的**：
单字的词频天生比词高（`shijian` 里「是」796 万 × 「见」59 万 > 「时间」33 万），
没有奖励就会切成"一个字一个字"的傻句子。参数在 `sentence.rs` 里，注释写了怎么定出来的。

一元词频模型的局限见[已知不足](pitfalls.md#已知不足)——想要"只出库里真有的词"，
配置里 `sentence = false` 关掉。分段上屏（用户自己挑前缀）见[使用](usage.md#分段上屏--记性)。

### 每个库一个文件，用户数据再单独一个 + `ATTACH`

拼音词库（86 MB、别人整理的数据、`pliers fetch pinyin --force` / `pliers build pinyin` 会整个换掉）、
码表库（`wubi.db`，你自己那份码表导出来的）和用户数据（几十 KB、你自己选过的词和拼过的句子）
**生命周期完全不一样**，所以各占一个文件。以前全混在一个文件里：换一次拼音词库，
用户习惯和五笔条目一起没 —— 导入工具是先把输出文件删了重建的。

分家之后最大的问题是排序：中文候选的顺序是**一句 SQL** 算出来的
（`weight + 用户次数 ×100 万`）。库和用户数据怎么在一句 SQL 里 JOIN？用 SQLite 的 `ATTACH`：

```sql
ATTACH '~/.local/share/pliers/user.db' AS user;
SELECT w.text, w.weight + MIN(COALESCE(u.count, 0), 50) * 1000000 AS score
FROM word w LEFT JOIN user.user_word u ON u.text = w.text
WHERE w.scheme = ?1 AND w.code = ?2 ORDER BY score DESC LIMIT ?3
```

turso 里这个功能还叫 `experimental_attach`（得在 `Builder` 上显式打开），但本地 attach
一个文件、跨库建表/写入/JOIN 实测都正常。另一条路是开两个连接、把用户词频读进内存再在
Rust 里重排 —— 那样"权重最高的前 N 个"就得先按语料权重截断，一个被你选过很多次、
但语料权重排在窗口外的词会浮不上来。`ATTACH` 保住了原来的语义，代价只有一个实验性开关。

打开哪个库由方案决定（`Config::active_dict_path()`，见 `config.rs`）：拼音那几套开 `dict.db`，
`kind = "wubi"` 开 `wubi.db` —— 一次只开一个，所以同一套 `Dict` 代码两边通用
（两个库的表结构一模一样，连那 412 个音节都是导入时一起写进去的）。

老布局（用户表还混在词库里）会在第一次打开时自动搬过去，见
[词库](dictionary.md#表结构每个库一个文件用户数据单独一个)。

### 英文候选：另一半不进词库

英文补全（`hel` → help/hello/hell）用的是自己一个 SQLite 库
（`~/.local/share/pliers/english.db`，一张 `english(word, weight)` 表；跟词库一起挂在 Release 上，
但可以 `pliers fetch english` 单独更新）。**关键是它只在启动时被读一次**：
一次性把两万五千行读进内存，之后每次按键都在这份内存词表上扫（实测 57 µs）。
二进制里还嵌着同一份文本（`include_str!`）**只当兜底**：库没装或读不了时用它，
保证"装完就能用、离线也能用"。

也就是说：SQLite 在这里是**存储和发布格式**，不是每次按键的查询引擎 —— 那样才谈得上跟下面那条对照：

* 它**只读、永不修改**：没有用户词频要写进去，就不需要事务、WAL、"重新导入"那一套；
* 查法是"谁以这串字母开头"，词表按词频排好之后**顺着扫一遍就是答案**（先扫到的更常用），
  不需要排序 —— 实测扫完两万五千个词在几十微秒这个量级，跟一次精确查询（60 µs）同档；
  而同样的活交给 SQL 就是上面那张表里 280 ms 的 `LIKE 'ni%'`；
* 用户想加词就指一个 txt（`[english] path`），不用为一个词表去重新构建词库。

引擎里候选池的顺序是：**用户自己拼过的整句 → 英文补全 → 方案给的中文候选**。
英文能排到中文前面，是因为它只在"这串字母已经不像拼音"时才出现：

| 输入 | `looks_pinyin` | 道理 |
| --- | --- | --- |
| `nihao` `shou` `xian` | true（切得完） | 完整拼音，只出中文 |
| `sho` `zho` `shij` | true（**还挂在某个音节上**） | `sho` 是 `shou` 的半截：正在打「手」，不该冒出 `should` |
| `hello` `config` `kub` | false | 切不动、也不是任何音节的开头 → 轮英文 |

"还挂在音节上"这一条（`Segmenter::is_syllable_prefix`，412 个音节扫一遍）是**关键的一小步**：
只看"能不能切完"的话，`sho`、`cho`、`zho` 这些打到一半的拼音会一路闪英文候选。
双拼方案没法这么判（键位跟拼音字母不是一回事），用"两键一组解不解得完"代替；
码表方案干脆永远算"在打中文"（敲的每个字母都是码）。

`Del` 也按词的粒度记：英文候选在 `con`/`conf`/`confi` 上都会冒出来，所以黑名单
（`user_hidden`）里 `code` 固定写 `#english`，效果是"这个词以后别给我了"。
`#` 不是拼音字符，不会跟真实输入撞车。

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

不想开输入法就想看候选框长什么样：

```bash
cargo run -p pliers-popup --example dump_popup   # 存成 target/popup.png
```
