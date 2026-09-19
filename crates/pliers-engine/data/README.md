# 内置英文候选词表

`english.txt` 是**编译进二进制**的英文词表（`include_str!`，不用下载、也不用进 SQLite 词库）。
一行一个词，**按词频从高到低**：同一个前缀下先列出来的先出候选。

它是 [`tools/build_english_list.py`](../../../tools/build_english_list.py) 生成的：

| 位置 | 内容 | 来源 |
| --- | --- | --- |
| 1 .. 1000 | 日常英语最常用的 1000 个词 | 上游词频表 |
| 1001 .. ~1535 | 开发常用词（`config`/`stdout`/`iterator`/`kubernetes`…） | 本项目自己写的 [`tools/english-extra.txt`](../../../tools/english-extra.txt) |
| 其余 | 上游词频表剩下的词（取到第 25000 名） | 上游词频表 |

为什么掺一份自己写的：上游是**影视字幕**词频，口语词（`gonna`/`yeah`）排得很前，而敲代码
最常打的那批词要么没有（`config`、`async`、`stdout`）、要么在两万名开外（`compiler`、`docker`）。
把开发词插在"最常用的一千个词"之后，是让 `the`/`you`/`work` 这些日常词照样赢，
剩下的位置让给开发词。

生成时做了这几件事（都是为了让候选干净）：

* 只留纯小写字母的词 —— 输入法只认 a-z，`don't`、`café`、`hyeon-to` 匹配不上；
* 去掉一个字母的词：一个字母跟拼音的"首字母联想"完全分不开，运行时也不会拿它匹配英文；
* 去掉缩写被切掉撇号之后的残渣（`don't` → `don`、`we're` → `re`、`I'll` → `ll`）——
  它们会排到很前面（`don` 是第 31 名），给候选很奇怪。正经英文单词（`can`/`won`/`ill`/`its`/`id`）
  不在此列。

## 许可

* **1 .. 25000 名那份**（也就是文件里除了开发词以外的部分）：
  [hermitdave/FrequencyWords](https://github.com/hermitdave/FrequencyWords) 的 `en_50k`，
  词频来自 [OpenSubtitles2018](http://opus.nlpl.eu/OpenSubtitles2018.php)，内容按
  **CC-BY-SA-4.0** 授权（仓库本身是 MIT：代码 MIT、内容 CC-BY-SA-4.0）。
  这里是它的**衍生作品**（截取前 25000 个纯小写字母词、去掉缩写残渣），同样按 CC-BY-SA-4.0 提供。
* **开发词那份**（`tools/english-extra.txt`）：本项目自己写的，跟代码一样 MIT OR Apache-2.0。

也就是说：`english.txt` 是 CC-BY-SA-4.0 的**数据文件**，而 pliers 的**代码**是 MIT/Apache-2.0，
两者互不影响（词库资产是 GPL-3.0，又是另一回事，见 [docs/dictionary.md](../../../docs/dictionary.md)）。

## 重新生成 / 换成别的词表

```nushell
# 默认从 GitHub 下上游词频表
python3 tools/build_english_list.py

# 已经有一份本地词频表（每行 `词 次数` 或只有词）
python3 tools/build_english_list.py --source ~/en_50k.txt --limit 30000
```

改完 `tools/english-extra.txt` 也要跑一次。生成脚本只认第一列，所以上游换成正体字、
BNC、自己统计的词频表都行。

**想加自己的词不用改这里**：配置里写

```toml
[english]
path = "~/.config/pliers/words.txt"   # 一行一个词，会排在所有内置词前面
```

详细行为（什么时候出英文候选、为什么 `song` 不会出英文）见 [docs/usage.md](../../../docs/usage.md)。
