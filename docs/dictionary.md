# 词库：装、换、自己构建

[← README](../README.md) · 相关：[配置](config.md#词典与候选数量) · [实现](internals.md#一条反直觉的结论拼音查询不能交给-sql)

词库是几百 MB 的派生物，所以**不进仓库**（GitHub 单文件上限 100 MB），
但也不用你自己找词表、守着跑导入 —— 一条命令装好：

```bash
pliers --init          # 写配置 + 下载词库
```

> 这篇讲的是**中文词库**（SQLite 里的 word / user_word / … 那几张表）。
> 打英文单词时的补全**不在词库里** —— 那份词表编译在二进制里（`data/english.txt`），
> 不用下载也不用导入，见[使用](usage.md#英文单词补全英文候选)。

## 几条命令

| 想干的事 | 命令 |
| --- | --- |
| 新机器装一份能用的 | `pliers --init` |
| 只装（或重装）词库 | `pliers dict fetch [--force]` |
| 自己下语料、自己构建 | `pliers dict build` |
| 看现在用的是哪个库、多少词、什么来源 | `pliers dict status` |
| 拿到词库路径（脚本用） | `pliers dict path` |
| 手动下载一份放进去 | 放到 `pliers dict path` 打印的那个路径 |

`--init` 和 `dict fetch` 是从**仓库 Release 的资产**拿的：

```
https://github.com/rendaoer/pliers-ime/releases/latest/download/dict.db.zst
```

`releases/latest/download/` 永远指向最新 Release 里的同名文件，所以这个地址不用跟着版本号改。
换源（公司镜像、自己搭的服务器、本地文件都行）：

```bash
PLIERS_DICT_URL=https://内网镜像/dict.db.zst pliers dict fetch
pliers dict fetch --url file:///mnt/u盘/dict.db.zst      # 离线装
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

## 自己构建

不用等 Release，也不用信任何人的二进制：

```bash
pliers dict build                    # 下载语料（缓存在 ~/.cache/pliers/rime-frost）→ 构建
pliers dict build --refresh          # 语料重新下一遍（上游更新了）
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

码表用 `--table` 导入：`--table wubi.txt --table-scheme wubi`，
每行 `词<TAB>码[<TAB>权重]`（rime 那种 .txt 码表就是这个格式）。见[配置](config.md#五笔--码表方案)。

## 表结构

```sql
word(scheme, code, text, weight)    -- 词库本体：scheme='pinyin' / 'wubi' / …
user_word(text, count, last_used)   -- 用户选过多少次（调频用）
user_phrase(code, text, count, last_used)  -- 用户自己分段拼出来的句子
user_hidden(code, text)             -- 用户按 Del 删掉的词（黑名单）
syllable(syl)                       -- 412 个合法音节，切词用
meta(key, value)                    -- 词库来源、权重来源、导入时间、词条数
```

* `scheme` 字段就是"多方案"的落点：一套方案一批行，互不干扰。一个库里可以同时有
  `pinyin` 和 `wubi` 的条目（`pliers dict status` 会分行数）
* 词库是**只读的派生物**：重新导入不会碰 `user_word`，用户习惯留得住
* 权重 = 语料权重（缩放后）+ 用户选过的次数 ×100 万（最多算 50 次）。
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
这就是"用户使用频率权重"。`user_phrase` / `user_hidden` 的语义见[使用](usage.md#分段上屏--记性)。

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

不想碰这个许可就用 `pliers dict build` 自己从上游构建 —— 那只是下载语料，不涉及再分发。
