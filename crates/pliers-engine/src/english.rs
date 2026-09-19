//! 英文候选：打英文单词的时候给补全（`hel` + 空格 → `hello`）。
//!
//! 词表放在**自己的一个 SQLite 文件**里：`~/.local/share/pliers/english.db`
//! （表就一张：`english(word, weight)`），跟中文词库 `dict.db`、用户数据 `user.db`
//! 各管各的、可以各自更新：
//!
//! 1. `english.db` —— 运行时用的就是它。`pliers --init` 装一份，
//!    `pliers english fetch` 从 GitHub Release 更新（约 60 KB 的资产，能**单独更新**，
//!    不用重装输入法、也不用重下 27 MB 的中文词库）；
//! 2. `[english] extra` 指的文件（**文本**，一行一个词）—— 你自己额外加的词，排在最前面；
//! 3. `data/english.txt`（`include_str!` 编译进来那份文本）—— **只当兜底**：
//!    `english.db` 没装/读不了的时候用它，保证"装完就能用、离线也能用"。
//!
//! 关键的一条：**只在启动时把 `english.db` 读进内存**（两万五千行，几十毫秒），
//! 每次按键还是在这份内存词表上扫一遍（实测 57 µs）——
//! 用 SQLite 当"存储和发布格式"，不等于把每次按键的查询交给 SQL。
//! 那种"宽前缀 + 按权重排序"的查询正是这个项目一开始就绕开的坑（见 docs/internals.md）。
//!
//! **什么时候轮得到英文候选不归这里管**：引擎先问方案"这串字母还像不像在打拼音"
//!（[`crate::Scheme::looks_pinyin`](crate::scheme::Scheme::looks_pinyin)），不像才来查这份词表。
//! 所以 `shou` 永远只会出「手/受」，而 `hello` 的第一个候选就是 `hello`。
//! 词表的来历、许可、怎么重新生成见 `data/README.md`。

use std::path::Path;

use turso::Builder;

/// `english.db` 的表结构。导入工具（`pliers-dict --english`）和运行时共用这一份 DDL
///
/// `weight` 是按词频顺序折算出来的分数（越大越靠前）：第 0 名 = 总词数 ×1000，
/// 往后每名少 1000。现在读的时候只按它排序，留着它是为了这个库能当"能查的库"用
///（想按权重筛、想以后跟用户词频一起算，都有个字段可用）
pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS english (
    word   TEXT PRIMARY KEY,
    weight INTEGER NOT NULL
)";

/// 兜底词表。`include_str!` = 编译进去：外部那份没装/读不了的时候用它，
/// 保证"装完就能用、离线也能用"。`pliers --init` 装的那份就是从它写出去的
pub const BUILTIN: &str = include_str!("../data/english.txt");

/// 词表里最短的词。一个字母的词（`a`/`i`）不收：一个字母跟拼音的"首字母联想"
/// 完全分不开，运行时也不会拿一个字母去匹配英文（见 [`Words::candidates`]）
const MIN_LEN: usize = 2;

/// 这份词表是从哪儿来的（`pliers status` / `pliers english status` 报给人看）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// 外部那个库（正常情况：`~/.local/share/pliers/english.db`，`pliers english fetch` 更新它）
    Db(std::path::PathBuf),
    /// 二进制里那份兜底文本（库没装 / 读不了）
    Builtin,
}

impl Source {
    /// 给命令行看的一句话
    pub fn label(&self) -> String {
        match self {
            Source::Db(path) => path.display().to_string(),
            Source::Builtin => "内置兜底那份".to_string(),
        }
    }
}

/// 把一份词表写成 `english.db`（`pliers --init` 离线装兜底那份、以及导入工具都用它）。
///
/// `words` 的顺序就是优先级：第 0 名最靠前。表建好之后整批插进去
pub fn write_db(path: &Path, words: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // 从零开始：旧库（连它的 -wal / -shm）直接扔掉，免得混进旧词
    for extra in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{extra}", path.display()));
    }
    let db = pollster::block_on(Builder::new_local(&path.to_string_lossy()).build())?;
    let conn = db.connect()?;
    pollster::block_on(conn.execute(SCHEMA, ()))?;
    let total = words.len() as i64;
    let mut stmt = pollster::block_on(
        conn.prepare("INSERT OR REPLACE INTO english (word, weight) VALUES (?1, ?2)"),
    )?;
    pollster::block_on(conn.execute("BEGIN", ()))?;
    for (index, word) in words.iter().enumerate() {
        let weight = (total - index as i64) * 1000;
        pollster::block_on(stmt.execute(turso::params![word.as_str(), weight]))?;
    }
    drop(stmt);
    pollster::block_on(conn.execute("COMMIT", ()))?;
    // 把 WAL 并回主库：这个文件是要被**拷来拷去**的（打包资产、换机器、审核一份），
    // 只拷 .db 而落下 -wal 就会拿到一个空的库 —— 词库那边踩过同一个坑。
    // 这条 PRAGMA 会回一行结果，得用 query 读掉（execute 会报 unexpected row）
    {
        let mut stmt = pollster::block_on(conn.prepare("PRAGMA wal_checkpoint(TRUNCATE)"))?;
        let mut rows = pollster::block_on(stmt.query(()))?;
        while pollster::block_on(rows.next())?.is_some() {}
    }
    Ok(())
}

/// 从一个 `english.db` 里把词读出来（按权重从高到低 = 词频从高到低）。
/// 表缺了 / 库坏了都返回错误，调用方会退回兜底那份
pub fn read_db(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let db = pollster::block_on(Builder::new_local(&path.to_string_lossy()).build())?;
    let conn = db.connect()?;
    let mut stmt =
        pollster::block_on(conn.prepare("SELECT word FROM english ORDER BY weight DESC"))?;
    let mut rows = pollster::block_on(stmt.query(()))?;
    let mut words = Vec::with_capacity(26_000);
    while let Some(row) = pollster::block_on(rows.next())? {
        if let turso::Value::Text(word) = row.get_value(0)? {
            words.push(word);
        }
    }
    Ok(words)
}

/// 英文词表：按优先级排好的一串词（自己额外加的在前，主词表在后）
pub struct Words {
    words: Vec<String>,
    source: Source,
}

impl Words {
    /// 只有兜底词表
    pub fn builtin() -> Self {
        Self::load(None, None)
    }

    /// `main_db`（`english.db`，没有/读不了就用二进制里那份兜底）
    /// + `extra`（自己额外加的文本词表，排前面）。
    ///
    /// 两个都读不了都不该让输入法起不来：库读不了 → 退回兜底那份；`extra` 读不了 → 吼一句
    pub fn load(main_db: Option<&Path>, extra: Option<&Path>) -> Self {
        let (base, source) = match main_db {
            Some(path) if path.exists() => match read_db(path) {
                Ok(words) if !words.is_empty() => (words, Source::Db(path.to_path_buf())),
                Ok(_) => {
                    eprintln!(
                        "pliers: 英文词表 {} 是空的（先用内置那份顶着）",
                        path.display()
                    );
                    (parse_list(BUILTIN), Source::Builtin)
                }
                Err(e) => {
                    eprintln!(
                        "pliers: 读不了英文词表 {}：{e}（先用内置那份顶着）",
                        path.display()
                    );
                    (parse_list(BUILTIN), Source::Builtin)
                }
            },
            _ => (parse_list(BUILTIN), Source::Builtin),
        };

        let mut words = Vec::with_capacity(base.len() + 512);
        if let Some(path) = extra {
            match std::fs::read_to_string(path) {
                Ok(text) => words.extend(parse_list(&text)),
                Err(e) => eprintln!("pliers: 读不了自己加的英文词 {}：{e}", path.display()),
            }
        }
        words.extend(base);
        Self { words, source }
    }

    /// 词表里有多少词（`pliers status` 报数用）
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// 这份词表是从哪儿来的
    pub fn source(&self) -> &Source {
        &self.source
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
pub fn parse_list(text: &str) -> Vec<String> {
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
    fn 主词表文件不存在就退回兜底那份() {
        let words = Words::load(Some(Path::new("/nonexistent/english.txt")), None);
        assert_eq!(words.source(), &Source::Builtin);
        assert!(words.len() > 20_000, "兜底那份该是完整的");
        assert_eq!(words.candidates("hello", 1), ["hello"]);
    }

    #[test]
    fn 主词表库存在就用它() {
        let dir = std::env::temp_dir().join(format!("pliers-main-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("english.db");
        let list: Vec<String> = ["alpha", "beta", "gamma"]
            .iter()
            .map(|w| w.to_string())
            .collect();
        write_db(&path, &list).unwrap();

        let words = Words::load(Some(&path), None);
        assert_eq!(words.source(), &Source::Db(path.clone()));
        assert_eq!(words.len(), 3, "外部那个库说了算，不再掺兜底那些");
        assert_eq!(words.candidates("al", 5), ["alpha"]);
        assert!(words.candidates("hello", 5).is_empty());
        // 一个字母不掺和（"首字母联想"归拼音），两个字母起才给
        assert!(words.candidates("a", 5).is_empty());
        assert_eq!(words.candidates("be", 5), ["beta"]);
    }

    #[test]
    fn 权重按名次折算() {
        let dir = std::env::temp_dir().join(format!("pliers-weight-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("english.db");
        let list: Vec<String> = ["first", "second", "third"]
            .iter()
            .map(|w| w.to_string())
            .collect();
        write_db(&path, &list).unwrap();
        let db = pollster::block_on(Builder::new_local(&path.to_string_lossy()).build()).unwrap();
        let conn = db.connect().unwrap();
        let mut stmt = pollster::block_on(
            conn.prepare("SELECT word, weight FROM english ORDER BY weight DESC"),
        )
        .unwrap();
        let mut rows = pollster::block_on(stmt.query(())).unwrap();
        let mut got = Vec::new();
        while let Some(row) = pollster::block_on(rows.next()).unwrap() {
            let (turso::Value::Text(word), turso::Value::Integer(weight)) =
                (row.get_value(0).unwrap(), row.get_value(1).unwrap())
            else {
                panic!("列类型不对");
            };
            got.push((word, weight));
        }
        assert_eq!(
            got,
            [
                ("first".to_string(), 3000),
                ("second".to_string(), 2000),
                ("third".to_string(), 1000)
            ]
        );
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

        let words = Words::load(None, Some(&path));
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
    fn 自己那份读不了也不影响主词表() {
        let words = Words::load(None, Some(Path::new("/nonexistent/words.txt")));
        assert!(words.len() > 20_000);
        assert_eq!(words.source(), &Source::Builtin);
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
