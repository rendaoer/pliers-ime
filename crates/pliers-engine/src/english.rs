//! 英文候选：打英文单词的时候给补全（`hel` + 空格 → `hello`）。
//!
//! 词表**编译进二进制**（`data/english.txt`，两万五千个词，按词频排好），不进 SQLite 词库：
//!
//! * 它小（192 KB）、只读、一次装好永远不变 —— 没有 schema、没有下载、没有 WAL 那一套；
//! * 查法是"扫一遍看看谁以这串字母开头"，不需要索引，也不该为了它去动词库的表结构；
//! * 用户想加自己的词，配置里指一个 txt 就行（`[english] path`），不用重新导入词库。
//!
//! **什么时候轮得到英文候选不归这里管**：引擎先问方案"这串字母还像不像在打拼音"
//!（[`crate::Scheme::looks_pinyin`](crate::scheme::Scheme::looks_pinyin)），不像才来查这份词表。
//! 所以 `shou` 永远只会出「手/受」，而 `hello` 的第一个候选就是 `hello`。
//! 词表的来历、许可、怎么重新生成见 `data/README.md`。

use std::path::Path;

/// 内置词表。`include_str!` = 编译进去，装好就有，不联网也能用
const BUILTIN: &str = include_str!("../data/english.txt");

/// 词表里最短的词。一个字母的词（`a`/`i`）不收：一个字母跟拼音的"首字母联想"
/// 完全分不开，运行时也不会拿一个字母去匹配英文（见 [`Words::candidates`]）
const MIN_LEN: usize = 2;

/// 英文词表：按优先级排好的一串词（用户自己那份在前，内置的在后）
pub struct Words {
    words: Vec<String>,
}

impl Words {
    /// 只有内置词表
    pub fn builtin() -> Self {
        Self::load(None)
    }

    /// 内置词表 + 用户自己的那份（`extra` 里的词排在前面）。
    ///
    /// 自己那份读不了（路径写错、没权限）不该让输入法起不来 —— 吼一句，然后只用内置的
    pub fn load(extra: Option<&Path>) -> Self {
        let mut words = Vec::with_capacity(26_000);
        if let Some(path) = extra {
            match std::fs::read_to_string(path) {
                Ok(text) => words.extend(parse(&text)),
                Err(e) => eprintln!(
                    "pliers: 读不了英文词表 {}：{e}（只用内置的）",
                    path.display()
                ),
            }
        }
        words.extend(parse(BUILTIN));
        Self { words }
    }

    /// 词表里有多少词（`pliers status` 报数用）
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// 以 `prefix`（小写字母）开头的词，最多 `limit` 个，按优先级从高到低。
    ///
    /// 就是顺着词表扫一遍。词表已经按词频排好，所以"先扫到的就是更常用的"，
    /// 不需要再排序；一个按键扫两万多个词、每个词比几个字符，实测在几十微秒这个量级，
    /// 跟一次数据库查询差不多 —— 换来的是没有索引、没有额外数据结构
    pub fn candidates(&self, prefix: &str, limit: usize) -> Vec<String> {
        let mut out = Vec::with_capacity(limit);
        if prefix.len() < MIN_LEN || limit == 0 {
            return out;
        }
        for word in &self.words {
            if word.starts_with(prefix) && !out.contains(word) {
                out.push(word.clone());
                if out.len() >= limit {
                    break;
                }
            }
        }
        out
    }
}

/// 解析一份词表：一行一个词，`#` 开头是注释。
///
/// 也认 `词 次数` 这种词频表（上游就是这格式）—— 只取第一列，顺序就是优先级。
/// 只留纯小写字母、长度 ≥ [`MIN_LEN`] 的词（别的匹配不上：输入法只认 a-z）
fn parse(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(field) = line.split_whitespace().next() else {
            continue;
        };
        let word = field.to_ascii_lowercase();
        if word.len() >= MIN_LEN && word.bytes().all(|b| b.is_ascii_lowercase()) {
            out.push(word);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 内置词表读得出来() {
        let words = Words::builtin();
        assert!(words.len() > 20_000, "内置词表只有 {} 个词？", words.len());
        // 日常英语
        assert_eq!(words.candidates("hello", 1), ["hello"]);
        assert_eq!(words.candidates("the", 1), ["the"]);
        // 开发词（tools/english-extra.txt 补的，上游字幕词频表里根本没有）
        for word in ["config", "stdout", "iterator", "kubernetes", "nushell"] {
            assert_eq!(words.candidates(word, 1), [word], "{word} 该在词表里");
        }
    }

    #[test]
    fn 补全按词频排() {
        let words = Words::builtin();
        // `hel` → 先把常用的给出来（help 比 hello 常用），里面得有 hello
        let hits = words.candidates("hel", 5);
        assert_eq!(hits[0], "help", "{hits:?}");
        assert!(hits.contains(&"hello".to_string()), "{hits:?}");
        // 打全了就是精确匹配那一个
        assert_eq!(words.candidates("hello", 5), ["hello"]);
        // 打了一半也有（这就是"智能匹配"）
        assert!(
            words
                .candidates("kuber", 5)
                .contains(&"kubernetes".to_string())
        );
    }

    #[test]
    fn 匹配不到就是空的() {
        let words = Words::builtin();
        assert!(words.candidates("zzzz", 5).is_empty());
        assert!(
            words.candidates("nihaoma", 5).is_empty(),
            "拼音串不该匹配到英文"
        );
    }

    #[test]
    fn 一个字母不给候选() {
        // 一个字母是拼音的"首字母联想"地盘，英文不掺和
        let words = Words::builtin();
        assert!(words.candidates("a", 9).is_empty());
        assert!(words.candidates("t", 9).is_empty());
        // 两个字母开始才给
        assert!(!words.candidates("th", 9).is_empty());
    }

    #[test]
    fn 个数有上限() {
        let words = Words::builtin();
        assert_eq!(words.candidates("s", 9).len(), 0);
        assert_eq!(words.candidates("st", 3).len(), 3);
        assert_eq!(words.candidates("st", 0).len(), 0);
    }

    #[test]
    fn 自己那份词表排在前面() {
        let dir = std::env::temp_dir().join(format!("pliers-english-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("words.txt");
        // 注释、空行、词频格式、大小写、脏数据混在一起
        std::fs::write(
            &path,
            "# 我自己的词表\n\nhello   12345\nHELLO\nconfig\nnot a word\n工具\nx\n",
        )
        .unwrap();

        let words = Words::load(Some(&path));
        // 自己那份排前面：hello 在内置表里本来排第 3，现在第一
        assert_eq!(words.candidates("hel", 3)[0], "hello");
        // 同一个词在两份表里都有，只出一个（dedup）
        assert_eq!(words.candidates("hello", 9), ["hello"]);
        // 自己那份的词照样参与前缀补全，而且排在前面
        assert_eq!(words.candidates("conf", 9)[0], "config");
        // 脏数据被丢掉：不是纯 a-z、或者只有一个字母
        assert!(words.candidates("工具", 9).is_empty());
    }

    #[test]
    fn 词表读不了就只用内置的() {
        // 路径写错：不 panic，内置词表照旧能用
        let words = Words::load(Some(Path::new("/nonexistent/words.txt")));
        assert!(words.len() > 20_000);
        assert_eq!(words.candidates("hello", 1), ["hello"]);
    }

    #[test]
    fn 扫描够快() {
        // 每个按键都要扫一遍词表，别慢到能感觉出来（两万多个词，目标：几百微秒以内）
        let words = Words::builtin();
        let start = std::time::Instant::now();
        let rounds = 100;
        for _ in 0..rounds {
            // 挑一个"扫到底也没有"的前缀：最坏情况
            assert!(words.candidates("zzzz", 9).is_empty());
        }
        let each = start.elapsed() / rounds;
        // cargo test --release -p pliers-engine 扫描够快 -- --nocapture 能看到实际数字
        eprintln!("扫一遍 {} 个词：{each:?}", words.len());
        assert!(each.as_micros() < 2000, "扫一遍要 {each:?}，太慢了");
    }
}
