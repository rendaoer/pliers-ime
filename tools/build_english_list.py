#!/usr/bin/env python3
"""生成内置英文候选词表：`crates/pliers-engine/data/english.txt`。

英文候选不需要下载、也不需要进词库（它是编译进二进制的），所以这份文件就是
"英语词库"本身。合成规则：

    1..1000   上游词频表里最常用的 1000 个词（日常英语先赢）
    1001..    `tools/english-extra.txt`（开发常用词，按"打字时最可能想要哪个"排序）
    之后      上游词频表剩下的词（默认取到第 25000 名）

为什么要掺一份自己写的 `english-extra.txt`：上游是影视字幕词频，`config`、`stdout`、
`iterator`、`kubernetes` 这些要么没有、要么在两万名开外，而它们恰恰是敲代码时最常打的。

用法（默认从 GitHub 下，也可以喂一份本地文件）：

    python3 tools/build_english_list.py
    python3 tools/build_english_list.py --source ~/en_50k.txt --limit 30000

上游：hermitdave/FrequencyWords 的 en_50k（OpenSubtitles2018 词频），内容 CC-BY-SA-4.0，
许可见 crates/pliers-engine/data/README.md。
"""
import argparse
import re
import sys
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXTRA = ROOT / "tools" / "english-extra.txt"
OUT = ROOT / "crates" / "pliers-engine" / "data" / "english.txt"
SOURCE_URL = (
    "https://raw.githubusercontent.com/hermitdave/FrequencyWords"
    "/master/content/2018/en/en_50k.txt"
)

# 只要纯小写字母的词：词表里的 `don't`、`café`、`hyeon-to` 匹配不上（输入法只认 a-z）
WORD = re.compile(r"^[a-z]+$")
# 连字符/撇号被切掉之后剩下的半截：`don't` → don、`we're` → we re、`I'll` → ll。
# 它们会排在很前面（don 是第 31 名），当成正常词给候选很奇怪。真有这个词的
#（can、won、ill、its、id、cant、wont）不在名单里 —— 那些是正经英文单词。
STEMS = {
    "d", "ll", "m", "re", "s", "t", "ve",
    "ain", "aren", "couldn", "didn", "doesn", "don", "dont", "hadn", "hasn",
    "haven", "hes", "im", "isn", "ive", "lets", "shouldn", "shes", "thats",
    "theyre", "wasn", "weren", "whats", "wouldn", "youre",
}
# 词表里最短两个字母：一个字母的词（a / i）跟拼音的"首字母联想"完全分不开，
# 输入法也不会拿一个字母去匹配英文（见 crates/pliers-engine/src/english.rs）
MIN_LEN = 2


def read_list(text: str) -> list[str]:
    """上游词频表 → 按词频排好的词。每行 `词 次数`，取第一列"""
    words = []
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        word = line.split()[0].lower()
        if WORD.match(word) and len(word) >= MIN_LEN and word not in STEMS:
            words.append(word)
    return words


def read_extra(path: Path) -> list[str]:
    """自己写的那份开发词表：`#` 注释，词可以一行一个也可以一行一串。顺序就是优先级"""
    words = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        for word in line.split():
            word = word.lower()
            if WORD.match(word) and len(word) >= MIN_LEN and word not in STEMS:
                words.append(word)
    return words


def fetch(source: str) -> str:
    if source.startswith(("http://", "https://")):
        print(f"下载 {source}", file=sys.stderr)
        with urllib.request.urlopen(source, timeout=60) as response:
            return response.read().decode("utf-8", "replace")
    return Path(source).expanduser().read_text(encoding="utf-8")


def merge(general: list[str], extra: list[str], limit: int, head: int) -> list[str]:
    """日常最常用的 head 个词 + 开发词 + 剩下的词（全程去重，保持第一次出现的位置）"""
    out: list[str] = []
    seen: set[str] = set()

    def push(words: list[str]) -> None:
        for word in words:
            if word not in seen:
                seen.add(word)
                out.append(word)

    push(general[:head])
    push(extra)
    push(general[head:limit])
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description="生成内置英文候选词表")
    parser.add_argument(
        "--source",
        default=SOURCE_URL,
        help="上游词频表：URL 或本地文件（默认 FrequencyWords 的 en_50k）",
    )
    parser.add_argument("--limit", type=int, default=25000, help="通用词表最多取到第几名")
    parser.add_argument("--head", type=int, default=1000, help="最前面多少个词算'日常英语'")
    parser.add_argument("--out", type=Path, default=OUT, help=f"输出文件（默认 {OUT}）")
    args = parser.parse_args()

    general = read_list(fetch(args.source))
    extra = read_extra(EXTRA)
    if not general:
        print("上游词表是空的？", file=sys.stderr)
        return 1

    words = merge(general, extra, args.limit, args.head)
    head = f"""\
# pliers 内置英文候选词表。一行一个词，**按词频从高到低**：同一个前缀下先列的先出。
# 别手改这个文件 —— 它是 tools/build_english_list.py 生成的：
#   1..{args.head}     日常英语最常用的那些词（上游词频表）
#   {args.head + 1}..{args.head + len(extra)}   tools/english-extra.txt 里的开发常用词
#   其余      上游词频表剩下的词（取到第 {args.limit} 名）
# 上游：hermitdave/FrequencyWords en_50k（OpenSubtitles2018 词频，内容 CC-BY-SA-4.0）。
# 想加自己的词别改这里：配置里写 [english] path = "~/..../words.txt"（那份排在前面）。
# 许可与重新生成的步骤见 crates/pliers-engine/data/README.md
"""
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(head + "\n".join(words) + "\n", encoding="utf-8")
    print(
        f"写好 {args.out}：{len(words)} 个词"
        f"（通用 {min(len(general), args.limit)} 个，其中开发词 {len(extra)} 个），"
        f"{args.out.stat().st_size / 1024:.0f} KB",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
