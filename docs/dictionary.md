# 词库：导入、结构、维护

[← README](../README.md) · 相关：[实现](internals.md#一条反直觉的结论拼音查询不能交给-sql) · [配置](config.md#词典与候选数量)

词库不在仓库里（150 万词，上百 MB），得自己生成：拿一份拼音词表 + 一份词频表灌进 SQLite。

## 生成（只做一次）

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

导入一次大概几分钟（`--release` 快很多），生成的库 ~130 MB。

码表方案（五笔/郑码/仓颉）走另一个入口：`--table wubi.txt --table-scheme wubi`，
每行 `词<TAB>码[<TAB>权重]`（rime 的 .txt 码表就是这个格式），见[配置](config.md#五笔--码表方案)。

## 单字与多音字

一个字在不同词里读音可能不一样（长 = zhang/chang）。导入时**每个字挑一个主读音**
（给全权重），另外把"常用"的次要读音也补上（权重按用量打折）。门槛是"这个读音要占该字条目数的 7% 以上"，
拿真数据校准出来的：

| 该收 | 条目占比 | | 不该收 | 条目占比 |
| --- | --- | --- | --- | --- |
| 长 = chang | 23% | | 的 = di | 5% |
| 还 = huan | 12% | | 了 = liao | 5% |
| 重 = chong | 11% | | 给 = ji | 5% |
| 行 = hang | 9% | | 着 = zhao | 6% |
| 乐 = yue | 8% | | | |

为什么看**条目数**而不是"用量"：的=di 的用量占比有 35%（「目的」「的确」都是高频词），
比行=hang 的 10% 还高 —— 因为「的」当助词的用量根本不在任何词表里，光看词频分不开这两类。
条目数反而干净：的=di 只有 31 条，行=hang 有 88 条。收进来的一共 284 个次要读音。

## 表结构

```sql
word(scheme, code, text, weight)    -- 词库本体：scheme='pinyin' / 'wubi' / …
user_word(text, count, last_used)   -- 用户选过多少次（调频用）
user_phrase(code, text, count, last_used)  -- 用户自己分段拼出来的句子
user_hidden(code, text)             -- 用户按 Del 删掉的词（黑名单）
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
