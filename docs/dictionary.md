# 词库：装、换、自己构建

[← README](../README.md) · 相关：[配置](config.md#词典与候选数量) · [实现](internals.md#一条反直觉的结论拼音查询不能交给-sql)

词库是几百 MB 的派生物，所以**不进仓库**（GitHub 单文件上限 100 MB），
但也不用你自己找词表、守着跑导入 —— 一条命令装好：

```bash
pliers --init          # 写配置 + 下载词库
```

> 这篇讲的是**中文词库**（SQLite 里的 word / syllable / meta 那几张表）。
> 打英文单词时的补全**不在词库里** —— 它是另一个独立的库（`english.db`，
> 跟词库一起挂在同一个 Release 上，但可以单独更新：`pliers fetch english`），
> 见[使用](usage.md#英文单词补全英文候选)。

## 几条命令

命令行是**动词在前、种类在后**（种类：`pinyin` / `wubi` / `english` —— 字典是个总概念）：

| 想干的事 | 命令 |
| --- | --- |
| 新机器装一份能用的 | `pliers --init` |
| 只装（或重装）拼音词库 | `pliers fetch pinyin [--force]` |
| 只更新英文词表 | `pliers fetch english` |
| 两个预构建的都要 | `pliers fetch all`（也可以不写种类） |
| 哪个旧更新哪个 | `pliers update`（先比 Release 上的 sha256，一样就跳过） |
| 自己下语料、自己构建拼音词库 | `pliers build pinyin [--refresh]` |
| 用自己的码表建五笔库 | `pliers build wubi <码表.txt>` |
| 看现在用的是哪个库、多少词、什么来源 | `pliers status pinyin`（不给种类 = 连实例和别的库一起看） |
| 拿到某个库的路径（脚本用） | `pliers path dict` / `pliers path wubi` |
| 手动下载一份放进去 | 放到 `pliers path dict` 打印的那个路径 |

`--init` 和 `pliers fetch pinyin` 是从**仓库 Release 的资产**拿的：

```
https://github.com/rendaoer/pliers-ime/releases/latest/download/dict.db.zst
```

`releases/latest/download/` 永远指向最新 Release 里的同名文件，所以这个地址不用跟着版本号改。
换源（公司镜像、自己搭的服务器、本地文件都行）：

```bash
PLIERS_DICT_URL=https://内网镜像/dict.db.zst pliers fetch pinyin
pliers fetch pinyin --url file:///mnt/u盘/dict.db.zst      # 离线装
```

下载落在 `<目标>.part`，解压到 `<目标>.unpacked`，最后一步才改名 —— 中途断网不会把
已经装好的词库弄坏。装完它会打开来验一遍（表缺了、音节表空了都会报错），并打印词条数。

## 默认的词库是什么

[**白霜拼音 rime-frost**](https://github.com/gaboolic/rime-frost)（GPL-3.0）的词表，
用 7.4 亿字高质量语料重新统计过字频、词频并归一化。导入的是这六份：

| 文件 | 内容 |
| --- | --- |
| `8105.dict.yaml` | 通用规范汉字表 8105 字（单字，多音字一行一个读音） |
| `41448.dict.yaml` | 扩展字表（41448 字） |
| `base.dict.yaml` | 核心词库 |
| `ext.dict.yaml` | 扩展词库 |
| `tencent.dict.yaml` | 腾讯词库 |
| `others.dict.yaml` | 杂项、口语读音、容错词 |

（`corrections.dict.yaml` 是"错词 → 正词"的纠错表，不是候选词库，不导入。）

导入时会顺手筛掉两种脏数据：**一眼假的音节**（`均订` 的码在语料里写成了 `junding`，本是 `jun ding`）和**偏门音节**（比如 `lvan`，三套双拼键位都打不出来）—— 这些留在音节表里会把双拼的启动自检带沟里，报出「这套双拼键位缺韵母：van」这种冤枉话。被筛掉的词条一般另有常规读音（`luan`）能打。

rime 词库的格式是 `词<TAB>拼音<TAB>权重`，拼音用空格分音节 —— 跟 `word.code` 存的
**一模一样**，所以除了跳过 YAML 文件头，几乎不用转换。两个白送的好处：

* **多音字不用猜**：`长` 就是两行（`chang` 一行、`zhang` 一行），各自带语料统计出来的权重。
  所以打 `chang` 第一个就是「长」、打 `hang` 第一个就是「行」，不用靠什么启发式去推
* **不用再挂一张词频表**：权重是语料统计出来的

导入时权重会**等比缩放到 8000 万的上限**（`RIME_MAX_WEIGHT`），这个数是照"用户调频
选一次 +100 万"定的：太小则选过一次的词永远第一、连「的」「你」都翻不了身，
太大则调频等于没调。

## 英文词表（同一个 Release 里的另一个资产）

Release 上除了 `dict.db.zst`，还有一个 **`english.db.zst`**（约 500 KB）：英文候选用的词表库
（一张 `english(word, weight)` 表）。上游是
[FrequencyWords](https://github.com/hermitdave/FrequencyWords) 的 `en_50k`（字幕词频）
加上仓库里 `tools/english-extra.txt` 那份开发常用词：`tools/build_english_list.py` 合成文本，
再由 `pliers-dict --english` 导成库。

它故意跟词库分开，好处是**能各自独立更新**：

```nushell
pliers fetch english     # 只更新词表（约 500 KB），不用重装输入法、也不用重下 27 MB 词库
pliers status english    # 现在用的是哪份、多少词
pliers build english 词表.txt   # 用自己的一份词表重建（不给文件就是重建兜底那份）
```

`pliers build english <文件|目录>` 把活交给 `pliers-dict --english`（格式解析跟引擎共用一份实现），
一行一个词、`#` 开头的行是注释、按词频从高到低排。不给文件的话就用二进制里那份兜底重建 ——
那条路**不需要** `pliers-dict`，离线也能跑。

词表库不在时引擎会自动用二进制里那份兜底（编译时嵌进去的同一份数据），
所以"没装"只会让 `pliers status` 显示成「内置兜底」，不会让英文候选消失。
**运行时是启动时一次性把它读进内存的**（两万五千行、几十毫秒），每次按键依旧在内存里扫 ——
SQLite 只当"存储和发布格式"，不参与每次按键的查询。

## 导入工具认识哪些格式

`pliers-dict` 有三种输入（`pliers-dict --help` 里也写着）：

### `--rime`：rime 词库（打拼音必须用它）

`.dict.yaml`。`---` 到 `...` 之间是 YAML 头（**跳过**），之后每行是
`词<TAB>拼音<TAB>权重`，**用制表符分开**；拼音是空格分开的小写音节，权重可以省（默认 1）：

```text
---
name: mydict
version: "1"
sort: by_weight
...
你	ni	500000
你好	ni hao	3000000
长	chang	900
长	zhang	100
```

规则和"坑"：

* `#` 开头的行和空行跳过；给目录就读里面所有 `.dict.yaml`（`corrections*` 这类纠错表不读）；
* **音节表（412 个合法音节）是从这些拼音码里收集的**，所以想打中文就必须给 `--rime`
  —— 只给 `--table` 会做出一个"没有音节表"的库，输入法起来会直接报错；
* 多音字就写多行（`长` 上两行），权重各自算；
* 权重只用来排序，导入时会**等比缩放到上限 8000 万**（跟别的语料一致），
  所以拿别的词频表来也行，量级不用对；
* 读不动的行**不会静默丢掉**：导入时会打一句「跳过 N 行」并把前几行连原因贴出来；
* 偏门音节（三套双拼键位都打不出来的，比如 `lvan`）会被筛掉，同样会报一句。

### `--table`：码表方案（五笔/郑码/仓颉）

每行 `词<TAB>码[<TAB>权重]`：

```text
你	nin
好	vbg	200
```

跟 `--table-scheme wubi` 一起用 —— 这个名字就是库里的 `word.scheme`，配置里
`[scheme] kind = "wubi"` + `name = "wubi"` 对得上它。

**它写出来的是自己的一个库**（默认 `~/.local/share/pliers/wubi.db`，`pliers build wubi`
就是调这个）。跟拼音词库分开是故意的：码是另一套东西，各建各的、各换各的 ——
重导（或下载一份新的）拼音词库不会把五笔一起抹掉。结构跟拼音词库一样
（`word` / `syllable` / `meta` 三张表），所以两边共用同一套读写代码。

```bash
pliers-dict --table 我的五笔.txt --table-scheme wubi --out ~/.local/share/pliers/wubi.db
```

### `--english`：英文候选词表

一行一个词（`#` 注释），**按词频从高到低**；上游那种 `词 次数` 的两列格式也认（只看第一列）：

```text
# 我自己的词表
hello
kubernetes
```

只收纯小写字母、长度 ≥ 2 的词（一个字母跟拼音的首字母联想分不开）。
写出来是 `english.db`（一张 `english(word, weight)` 表，权重按名次折算）。

### 一次只导一种

三种输入各写各的库（`dict.db` / `wubi.db` / `english.db`），所以分几次跑：

```bash
pliers-dict --rime ~/.cache/pliers/rime-frost --out ~/.local/share/pliers/dict.db
pliers-dict --table 我的五笔.txt --table-scheme wubi --out ~/.local/share/pliers/wubi.db
pliers-dict --english 词表.txt --out ~/.local/share/pliers/english.db
```

（以前拼音和码表能塞进同一个文件里，靠 `word.scheme` 区分；现在不行了 ——
分开之后重导拼音不会碰到五笔，各自的文件也能各自备份、各自重建。）

## 自己构建

（`pliers build pinyin` 会去调 `pliers-dict` 这个导入程序：`cargo install pliers-dict`，
或者在仓库里 `cargo build --release -p pliers-dict`。用发布版装的话它是单独一个包，
不会跟着 `pliers-ime` 一起装。）

不用等 Release，也不用信任何人的二进制：

```bash
pliers build pinyin            # 下载语料（缓存在 ~/.cache/pliers/rime-frost）→ 构建
pliers build pinyin --refresh  # 语料重新下一遍（上游更新了）
```

它做三件事：下六个 `.dict.yaml` → 调 `pliers-dict --rime <语料目录> --out <词库>` → 验一遍。
语料先试 `raw.githubusercontent.com`，连不上**自动换 jsDelivr 镜像**（国内直连 raw 常常不通），
下到 `.part` 再改名，半截的下载不会被下次当成「已经有了」。

要 `pliers-dict` 在跟 `pliers` 同一个目录或 PATH 里。注意**两个都要编 release**
（debug 版导入 100 万条要好几分钟，release 只要几秒）：

```bash
cargo build --release          # pliers + pliers-dict 一起编
```

想完全手工：

```bash
cargo run -p pliers-dict --release -- --rime ~/.cache/pliers/rime-frost --out ~/.local/share/pliers/dict.db
```

`--rime` 给目录（读里面所有 `.dict.yaml`）或单个文件都行，也可以给多个：

```bash
# 只用核心词库 + 单字（小一点、快一点）
cargo run -p pliers-dict --release -- --rime cn_dicts/base.dict.yaml --rime cn_dicts/8105.dict.yaml --out dict.db
```

## 码表方案（五笔 / 郑码 / 仓颉）

码表在自己的一个库里：`~/.local/share/pliers/wubi.db`（`pliers path wubi` 问路径）。
一行 `词<TAB>码[<TAB>权重]`（rime 那种 .txt 码表就是这个格式），构建一条命令：

```bash
pliers build wubi 我的五笔.txt                    # 默认 scheme 名字就叫 wubi
pliers build wubi 我的郑码.txt --scheme zhengma   # 别的码表：名字跟配置里的 scheme.name 对上
```

然后切过去（`pliers config set scheme.kind wubi`，或者写文件）：

```toml
[scheme]
kind = "wubi"             # 码表方案（五笔/郑码/仓颉都走这条）
name = "wubi"             # 跟构建时的 --table-scheme 对上
```

**为什么不是一个文件**：码和拼音是两套东西，谁也不用管谁 —— 重导拼音词库
（或者 `pliers fetch pinyin --force` 下载一份新的）不会把五笔抹掉，反过来也一样；
两个文件各自备份、各自重建。代价是"改一行码表也要重导整个 `wubi.db`"（几 MB 的码表，
几秒的事）。

`pliers status wubi` 看这个库的现状，`pliers path wubi` 拿路径。
**仓库里没有码表数据**（各家的码不一样、许可也各不相同），得用你手上那份 ——
见[配置](config.md#五笔--码表方案)。

## 字典是"总概念"：pinyin / wubi / english

命令行里 **dict 指的是整套字典**，它按方案分成几块，**各有各的文件、各有各的来路**：

| 种类 | 存在哪 | 资产 | 怎么更新 |
| --- | --- | --- | --- |
| `pinyin` | `dict.db`（`word` 表里 `scheme='pinyin'`） | `dict.db.zst` | `pliers fetch pinyin` / `pliers build pinyin` |
| `wubi` 等码表 | `wubi.db`（自己的库，行是 `scheme='wubi'` 这种） | 没有资产（各家码表不同） | `pliers build wubi <码表.txt>` |
| `english` | `english.db`（一张 `english(word, weight)` 表） | `english.db.zst` | `pliers fetch english` / `pliers build english` |

所以 `pliers fetch all`（或不写种类）= 两个预构建的资产；`pliers update` = 看哪块不是最新的就更新哪块。
`pliers status` 不带种类 = 正在跑的实例 + 每个库的现状（码表库没建就不占地方，
`pliers status wubi` 随时能单独看）。

## 表结构：每个库一个文件，用户数据单独一个

| 文件 | 里面有什么 | 谁写的 |
| --- | --- | --- |
| `~/.local/share/pliers/dict.db`（86 MB） | 拼音：`word` 词条 + 权重、`syllable` 音节表、`meta` 来源 | 导入工具（`pliers-dict --rime`） |
| `~/.local/share/pliers/wubi.db`（看码表多大） | 码表：同样的三张表，行是 `scheme='wubi'` 那种 | 导入工具（`pliers-dict --table`） |
| `~/.local/share/pliers/user.db`（几十 KB） | `user_word` 选过多少次、`user_phrase` 你自己拼的整句、`user_hidden` 按 Del 拉黑的词 | 输入法运行时 |
| `~/.local/share/pliers/english.db`（约 900 KB） | 英文候选词表（一张 `english(word, weight)` 表，25223 个词） | `pliers --init` 装，`pliers fetch english` 更新 |

```sql
-- dict.db / wubi.db：派生物，随时可以重新生成 / 下载覆盖。两张库结构一模一样
word(scheme, code, text, weight)    -- 词条本体（一个库里一种方案：pinyin 或 wubi）
syllable(syl)                       -- 412 个合法音节，切词用（码表用不着，但结构保持一致）
meta(key, value)                    -- 来源、权重来源、导入时间、词条数

-- user.db：你自己的东西，换词库不会碰它。**两种库共用这一份**
user_word(text, count, last_used)   -- 选过多少次（调频用）
user_phrase(code, text, count, last_used)  -- 你自己分段拼出来的句子
user_hidden(code, text)             -- 按 Del 删掉的词（黑名单，英文候选那条是 `#english`）
```

**为什么分成两个文件**：它们的生命周期完全不一样。词库 86 MB、是别人整理的数据、
`pliers fetch pinyin --force` 和 `pliers build pinyin` 会把它整个换掉（导入工具是先把输出文件
删了重建的）；码表库是你自己那份码表导出来的，跟拼音无关；用户数据只有几十 KB，
是你自己的东西 —— 混在一个文件里，换一次拼音词库就把"选过的词、自己拼的句子、
拉黑的词"全丢了，五笔也会跟着没。分开之后**每个文件都随便删、随便换**。

库和用户数据用 SQLite 的 `ATTACH` 连起来（turso 的 `experimental_attach`），所以排序还是
一句 SQL，不用把用户词频搬到内存里再排：

```sql
SELECT w.text, w.weight + MIN(COALESCE(u.count, 0), 50) * 1000000 AS score
FROM word w LEFT JOIN user.user_word u ON u.text = w.text
WHERE w.scheme = ?1 AND w.code = ?2
ORDER BY score DESC LIMIT ?3
```

* `scheme` 字段是"这个库里是哪套方案"的标签：查词永远带 `WHERE scheme = ?`，
  所以一个库里就算放了几套码也不会串（我们的构建是一条命令一套，一个库一种）
* 权重 = 语料权重（缩放后）+ 用户选过的次数 ×100 万（最多算 50 次）。
  所以选过十次的词能压过绝大多数常用词，但压不过「的」「你」这种顶级高频词 ——
  避免误选一次就再也翻不了身
* 选中的词会在 `Engine::pick()` 里写一笔 `user.user_word`，下次它自己就往前排了 ——
  这就是"用户使用频率权重"。`user_phrase` / `user_hidden` 的语义见[使用](usage.md#分段上屏--记性)
* 用户数据是**两种库共用**的：记的是"你选过哪个词"，跟用哪套码无关 —— 拼音下选过的词，
  切到五笔也照样排在前面
* 升级上来的老库（用户表还混在词库里）会在第一次打开时**自动搬**到 `user.db`，
  终端上会打一句"把词库里的用户数据搬到了 …（N 条）"。两边都有数据的话不合并，
  免得替你乱做决定
* **老布局的五笔不再读了**：以前 `scheme='wubi'` 的行是导在 `dict.db` 里的，
  现在码表方案开的是 `wubi.db`。拿原来那份码表跑一次 `pliers build wubi <码表.txt>`
  就行（拼音词库不用动），`pliers status wubi` 会提醒你还有多少老数据留在 `dict.db` 里

## 维护

**改词库之前先把输入法退掉**：turso 开着的时候会一直占着这个库，别的进程连只读连接都进不去
（`database is locked`）。用户数据现在是另一个文件，所以"清空用户词频"不用碰词库：

```bash
# 加词 / 改权重（改的是词库）
sqlite3 ~/.local/share/pliers/dict.db "INSERT OR REPLACE INTO word (scheme, code, text, weight) VALUES ('pinyin','ni hao','你好',3000000);"

# 想把用户词频清零（改的是用户数据）
sqlite3 ~/.local/share/pliers/user.db "DELETE FROM user_word;"

# 想让输入法彻底忘掉你的习惯：直接删文件就行（词库不用动）
rm ~/.local/share/pliers/user.db
```

只想看看数据（输入法开着也能读，因为读的是副本）。
**一定要连 `-wal` 一起拷**：最新的写入还在 WAL 里，只拷 `.db` 会看到旧数据。

```bash
mkdir -p /tmp/dbcopy
cp ~/.local/share/pliers/user.db     /tmp/dbcopy/          # 用户数据（小，一眼能看完）
cp ~/.local/share/pliers/user.db-wal /tmp/dbcopy/ 2>/dev/null   # 没有就是刚 checkpoint 过

sqlite3 /tmp/dbcopy/user.db "SELECT text, count, last_used FROM user_word ORDER BY last_used DESC LIMIT 10;"
```

## 维护

**改之前先把输入法退掉**：turso 开着的时候会一直占着这个库，别的进程连只读连接都进不去
（`database is locked`）。

```bash
# 加词 / 改权重
sqlite3 ~/.local/share/pliers/dict.db "INSERT OR REPLACE INTO word (scheme, code, text, weight) VALUES ('pinyin','ni hao','你好',3000000);"

# 想把用户词频清零
sqlite3 ~/.local/share/pliers/dict.db "DELETE FROM user_word;"
```

只想看看数据（输入法开着也能读，因为读的是副本）。
**一定要连 `-wal` 一起拷**：最新的写入还在 WAL 里，只拷 `.db` 会看到旧数据。

```bash
mkdir -p /tmp/dbcopy
cp ~/.local/share/pliers/dict.db     /tmp/dbcopy/
cp ~/.local/share/pliers/dict.db-wal /tmp/dbcopy/    # 没有这个文件就是刚 checkpoint 过，跳过

sqlite3 /tmp/dbcopy/dict.db "SELECT text, count, last_used FROM user_word ORDER BY last_used DESC LIMIT 10;"
```

## `lookup`：不开输入法看候选

想确认"打某个拼音到底会出哪些候选、每个键花多久"：

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

## 许可

**发布的词库资产（`dict.db.zst`）按 GPL-3.0 分发** —— 它是
[rime-frost](https://github.com/gaboolic/rime-frost)（GPL-3.0）那份语料的衍生作品。
上游仓库和构建脚本（`.github/workflows/dict.yml`）都是公开的，符合 GPL 对"随附相应源码"的要求。
`pliers` 自己的代码不受影响。

不想碰这个许可就用 `pliers build pinyin` 自己从上游构建 —— 那只是下载语料，不涉及再分发。
