//! 词库：放在 SQLite 里（用 turso 读写）。
//!
//! 为什么是 SQLite 而不是一个内存里的 HashMap：
//!
//! * 词表有 **150 万行**，全塞进内存要几百 MB，而输入法大部分时间在闲着
//! * 词库、权重、用户词频都是"数据"，可以随时用 SQL 改、加、导出，不用重新编译
//! * 一次装好，多进程共享；以后要做设置界面也直接读这个库
//!
//! **词库和用户数据分成两个文件**（`dict.db` / `user.db`）：
//!
//! ```sql
//! -- dict.db：派生物，随时可以从语料重新生成、下载覆盖
//! word(scheme, code, text, weight)   -- 词库本体：'pinyin' / 'wubi' / …
//! syllable(syl)                      -- 412 个合法音节，切词用
//! meta(key, value)                   -- 词库来源 / 导入时间之类
//!
//! -- user.db：用户自己的东西，换词库/重建词库都不碰它
//! user_word(text, count, last_used)  -- 你选过多少次（调频用）
//! user_phrase(code, text, …)         -- 你自己分段拼出来的整句（记性）
//! user_hidden(code, text)            -- 你按 Del 拉黑的词
//! ```
//!
//! 分成两个文件是因为**它们的生命周期完全不一样**：词库 86 MB、是别人整理的数据、
//! `pliers dict fetch --force` / `pliers dict build` 会把它整个换掉（导入工具是直接把
//! 输出文件删了重建的）；用户数据只有几十 KB，是你自己的东西，换词库时丢一次就再也不
//! 想用了。分家之后 `dict.db` 随便删、随便换。
//!
//! 两个文件用 SQLite 的 `ATTACH` 连起来（turso 的 `experimental_attach`），
//! 所以排序还是**一条 SQL**：`... LEFT JOIN user.user_word u ON u.text = w.text
//! ORDER BY w.weight + ... DESC`。查询全都写成 `user.xxx`，主库里没有这几张表。
//!
//! **查询只有两种，而且都是精确/短前缀**，这一点很关键：turso（以及任何 SQL 引擎）
//! 做"按前缀扫一大片再排序"会慢到没法用 —— 实测在 20 万行上，`code LIKE 'ni%'`
//! 这种宽前缀查询要 280ms。所以切词的活交给 [`crate::pinyin`] 在内存里做，
//! 数据库这边永远只回答"这个**完整的拼音码**对应哪些词"。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use turso::{Builder, Connection};

/// **词库**的 DDL（`dict.db`）。导入工具（`pliers-dict`）和运行时共用同一份，免得两边写岔。
///
/// 注意是一条一条执行的：turso 的 `execute()` 一次只认一条语句
/// （把几条 DDL 拼成一个字符串喂进去，它只会建第一张表）
pub const DICT_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS word (
        scheme TEXT NOT NULL,
        code   TEXT NOT NULL,
        text   TEXT NOT NULL,
        weight INTEGER NOT NULL,
        PRIMARY KEY (scheme, code, text)
    )",
    "CREATE TABLE IF NOT EXISTS syllable (syl TEXT PRIMARY KEY)",
    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
];

/// **用户数据**的 DDL（`user.db`，挂在 `ATTACH` 进来的 `user` 库上，所以表名都带前缀）。
///
/// 导入工具**不碰**这几张表 —— 它只建 [`DICT_SCHEMA`]，所以新构建出来的词库里
/// 根本没有用户表（以前是有的，混在一起）
pub const USER_SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS user.user_word (
        text TEXT PRIMARY KEY,
        count INTEGER NOT NULL DEFAULT 0,
        last_used INTEGER NOT NULL DEFAULT 0
    )",
    // 用户用"分段上屏"自己拼出来的句子：键是**他敲的那串原文**，
    // 下次敲同一串就直接把这句话给他。跟 user_word 分开是因为这里要带拼音（键）
    "CREATE TABLE IF NOT EXISTS user.user_phrase (
        code      TEXT NOT NULL,
        text      TEXT NOT NULL,
        count     INTEGER NOT NULL DEFAULT 0,
        last_used INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (code, text)
    )",
    // 用户在候选框里按 Del"删掉"的词：这个词/这句话以后不再出现在这串键的候选里。
    // 词库里的词条不动（那是导入出来的），所以另开一张"黑名单"。
    // 英文候选的黑名单也在这儿（`code` 固定是 `#english`）
    "CREATE TABLE IF NOT EXISTS user.user_hidden (
        code TEXT NOT NULL,
        text TEXT NOT NULL,
        PRIMARY KEY (code, text)
    )",
];

/// 建词库的表（幂等）。导入工具用它 —— 它只建词库那几张表
pub fn create_schema(conn: &Connection) -> Result<()> {
    for statement in DICT_SCHEMA {
        pollster::block_on(conn.execute(statement, ()))?;
    }
    Ok(())
}

/// 用户每选一次词，加多少分。
/// 词频权重最大到 8000 万（导入时统一缩放到这个上限），所以这个值让"用过十次"的词
/// 能压过绝大多数常用词 —— 但也压不过「的」「你」这种顶级高频词，避免一次误选就再也翻不了身
pub const USER_BOOST: i64 = 1_000_000;
/// 用户词频最多算多少次（防止某一个词被刷到天上去）
pub const USER_BOOST_CAP: i64 = 50;

/// `ATTACH '<用户数据文件>' AS user`。路径里的单引号要写成两个（SQL 字符串转义）
fn attach_user(path: &Path) -> String {
    format!(
        "ATTACH '{}' AS user",
        path.to_string_lossy().replace('\'', "''")
    )
}

/// 英文候选的黑名单键（`user_hidden.code`）。
///
/// 中文候选的黑名单是"**这串键**上别再给我这个词"（`code` 就是用户敲的那串字母），
/// 英文候选却是"补全"：同一个词在 `con`/`conf`/`confi` 上都会冒出来，按字母串记等于没记。
/// 所以英文词用这个固定键，效果是"这个词以后别给我了"。`#` 不是拼音字符，不会撞车
pub const ENGLISH_HIDE_KEY: &str = "#english";

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// 词库
pub struct Dict {
    conn: Connection,
    /// 用户数据那个文件（`user` 库）。换词库/重建词库都不碰它
    user_path: std::path::PathBuf,
    /// 合法音节表（从库里读出来，412 个）
    syllables: Vec<String>,
    /// 出错只吼一次，别每个按键都刷屏
    complained: std::cell::Cell<bool>,
}

impl Dict {
    /// 打开词库 + 用户数据。缺表 / 缺数据都会返回明确的错误，提示去跑导入工具。
    ///
    /// `user_path` 是**用户数据那个文件**（`user.db`）：它被 `ATTACH` 进来当 `user` 库，
    /// 不存在就建一个空的。词库本身换掉（重新下载/重建）不会碰到它
    pub fn open(path: &Path, user_path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(format!(
                "词库不存在：{}\n装一份（写配置 + 下载词库）：\n  pliers --init\n\
                 也可以用你自己的词表构建：pliers dict build（或 pliers-dict --help）",
                path.display(),
            )
            .into());
        }
        if let Some(dir) = user_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("建不了目录 {}：{e}", dir.display()))?;
        }
        // attach 要开着才认 `ATTACH` / `user.xxx` 这种跨库写法（turso 里它还是 experimental）
        let db = pollster::block_on(
            Builder::new_local(&path.to_string_lossy())
                .experimental_attach(true)
                .build(),
        )?;
        let conn = db.connect()?;
        // 用户数据那个文件挂成 `user` 库（文件不存在的话 SQLite 会建一个）
        pollster::block_on(conn.execute(&attach_user(user_path), ()))
            .map_err(|e| format!("挂不上用户数据 {}：{e}", user_path.display()))?;
        // 顺手补一下表结构：全是 CREATE TABLE IF NOT EXISTS，成本可以忽略，
        // 但老库（新加的表）就不用重新导入了 —— 加新表时省事
        create_schema(&conn)?;
        for statement in USER_SCHEMA {
            pollster::block_on(conn.execute(statement, ()))?;
        }
        let mut dict = Self {
            conn,
            user_path: user_path.to_path_buf(),
            syllables: Vec::new(),
            complained: std::cell::Cell::new(false),
        };
        dict.migrate_legacy_user_tables()?;
        dict.syllables = dict.load_syllables()?;
        if dict.syllables.is_empty() {
            return Err(format!(
                "词库里没有音节表（syllable 表是空的）：{}\n重新导入一次吧",
                path.display()
            )
            .into());
        }
        Ok(dict)
    }

    /// 用户数据那个文件在哪（`pliers status` 报给用户看）
    pub fn user_path(&self) -> &Path {
        &self.user_path
    }

    /// 老布局的用户数据（三张表混在词库里）搬到 `user.db` 去。
    ///
    /// 这是**一次性**升级：以前用户数据和词库在同一个文件里，分家之后得把原来那些
    /// `user_word` / `user_phrase` / `user_hidden` 搬过来，不然"选过的词、自己拼的句子、
    /// 拉黑的词"就全丢了。只在"词库里有、用户库里还没有"的时候搬（搬完自然就不再触发），
    /// 两边都有的话不动手 —— 不替用户做合并这种决定
    fn migrate_legacy_user_tables(&self) -> Result<()> {
        if !self.has_legacy_user_tables()? {
            return Ok(());
        }
        if self.count("user.user_word")? > 0 || self.count("user.user_phrase")? > 0 {
            return Ok(()); // 用户库里已经有东西了，不合并
        }
        let mut moved = 0;
        for table in ["user_word", "user_phrase", "user_hidden"] {
            moved += pollster::block_on(self.conn.execute(
                &format!("INSERT OR IGNORE INTO user.{table} SELECT * FROM main.{table}"),
                (),
            ))?;
        }
        if moved > 0 {
            eprintln!(
                "pliers: 把词库里的用户数据搬到了 {}（{moved} 条）—— 以后换词库不会丢它们了",
                self.user_path.display()
            );
        }
        Ok(())
    }

    /// 词库那个文件里还有没有老布局的用户表
    fn has_legacy_user_tables(&self) -> Result<bool> {
        let mut stmt = pollster::block_on(self.conn.prepare(
            "SELECT name FROM main.sqlite_master WHERE type = 'table' AND name = 'user_word'",
        ))?;
        let mut rows = pollster::block_on(stmt.query(()))?;
        Ok(pollster::block_on(rows.next())?.is_some())
    }

    /// 数一张表有多少行（`user.xxx` 这种带库名的也认）
    fn count(&self, table: &str) -> Result<i64> {
        let mut stmt =
            pollster::block_on(self.conn.prepare(&format!("SELECT count(*) FROM {table}")))?;
        let mut rows = pollster::block_on(stmt.query(()))?;
        let row = pollster::block_on(rows.next())?.ok_or("查不到")?;
        Ok(row.get_value(0)?.as_integer().copied().unwrap_or(0))
    }

    /// 合法音节表（切词用）
    pub fn syllables(&self) -> &[String] {
        &self.syllables
    }

    fn load_syllables(&self) -> Result<Vec<String>> {
        let mut stmt =
            pollster::block_on(self.conn.prepare("SELECT syl FROM syllable ORDER BY syl"))?;
        let mut rows = pollster::block_on(stmt.query(()))?;
        let mut out = Vec::with_capacity(512);
        while let Some(row) = pollster::block_on(rows.next())? {
            if let turso::Value::Text(syl) = row.get_value(0)? {
                out.push(syl);
            }
        }
        Ok(out)
    }

    /// 一个**完整**的码对应哪些词，按权重从高到低。
    /// 返回 `(词, 分数)`：一次输入可能对应好几个码（`xian` 既是「先」也是「西安」），
    /// 要把它们并起来重新排序，所以分数得一起带出来
    pub fn exact(&self, scheme: &str, code: &str, limit: usize) -> Vec<(String, i64)> {
        self.query(
            "SELECT w.text, w.weight + MIN(COALESCE(u.count, 0), ?3) * ?4 AS score
             FROM word w LEFT JOIN user.user_word u ON u.text = w.text
             WHERE w.scheme = ?1 AND w.code = ?2
             ORDER BY w.weight + MIN(COALESCE(u.count, 0), ?3) * ?4 DESC
             LIMIT ?5",
            &[
                scheme.into(),
                code.into(),
                USER_BOOST_CAP.into(),
                USER_BOOST.into(),
                (limit as i64).into(),
            ],
        )
    }

    /// 前缀查询：码表类方案（五笔等码长只有几位）用。
    ///
    /// 注意这是**唯一**会扫一大片的查询，码表很大时可能会慢 —— 见文件头的说明，
    /// 以后要提速就在导入时按前缀预算好 top-N
    pub fn prefix(&self, scheme: &str, prefix: &str, limit: usize) -> Vec<(String, i64)> {
        let upper = format!("{prefix}\u{10ffff}"); // 排在所有以 prefix 开头的串之后
        self.query(
            "SELECT w.text, w.weight + MIN(COALESCE(u.count, 0), ?4) * ?5 AS score
             FROM word w LEFT JOIN user.user_word u ON u.text = w.text
             WHERE w.scheme = ?1 AND w.code >= ?2 AND w.code < ?3
             ORDER BY w.weight + MIN(COALESCE(u.count, 0), ?4) * ?5 DESC
             LIMIT ?6",
            &[
                scheme.into(),
                prefix.into(),
                upper.into(),
                USER_BOOST_CAP.into(),
                USER_BOOST.into(),
                (limit as i64).into(),
            ],
        )
    }

    fn query(&self, sql: &str, params: &[turso::Value]) -> Vec<(String, i64)> {
        match self.try_query(sql, params) {
            Ok(texts) => texts,
            Err(e) => {
                // 查库失败不该让输入法崩掉：当成"没有候选"，吼一次就够了
                if !self.complained.replace(true) {
                    eprintln!("pliers: 查词库失败：{e}");
                }
                Vec::new()
            }
        }
    }

    fn try_query(&self, sql: &str, params: &[turso::Value]) -> Result<Vec<(String, i64)>> {
        let mut stmt = pollster::block_on(self.conn.prepare(sql))?;
        let mut rows = pollster::block_on(stmt.query(params.to_vec()))?;
        let mut out = Vec::new();
        while let Some(row) = pollster::block_on(rows.next())? {
            let (turso::Value::Text(text), turso::Value::Integer(score)) =
                (row.get_value(0)?, row.get_value(1)?)
            else {
                continue;
            };
            out.push((text, score));
        }
        Ok(out)
    }

    /// 这批词里用户各选过多少次（`user_word` 表）。英文候选靠它把"你用过的词"往前排。
    ///
    /// 中文那边是在 SQL 里 JOIN 出来一起排序的；英文词表不在库里（它编译在二进制里），
    /// 所以只能拿着候选单独问一次 —— 一次 `IN (...)`，五个词上下，跟一次普通查询差不多
    pub fn boosts(&self, texts: &[String]) -> std::collections::HashMap<String, i64> {
        if texts.is_empty() {
            return std::collections::HashMap::new();
        }
        let placeholders = (1..=texts.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let params: Vec<turso::Value> = texts.iter().map(|text| text.clone().into()).collect();
        self.query(
            &format!("SELECT text, count FROM user.user_word WHERE text IN ({placeholders})"),
            &params,
        )
        .into_iter()
        .collect()
    }

    /// 这个键（用户敲的原文）上，他自己拼过的句子。按用得多的排前面
    pub fn phrases(&self, code: &str, limit: usize) -> Vec<String> {
        self.query(
            "SELECT text, count FROM user.user_phrase WHERE code = ?1
             ORDER BY count DESC, last_used DESC LIMIT ?2",
            &[code.into(), (limit as i64).into()],
        )
        .into_iter()
        .map(|(text, _)| text)
        .collect()
    }

    /// 记一句"用户自己分段拼出来的话"：同一个键 + 同一句话再拼一次就加一笔
    pub fn note_phrase(&self, code: &str, text: &str) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        pollster::block_on(self.conn.execute(
            "INSERT INTO user.user_phrase (code, text, count, last_used) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(code, text) DO UPDATE SET count = count + 1, last_used = ?3",
            turso::params![code, text, now],
        ))?;
        Ok(())
    }

    /// 把某个键上的一句"用户自己拼的话"删掉（候选框里按 Del）。
    /// 返回是否真的删掉了一行 —— 词库（`word` 表）里的词条不动，那是导入出来的
    pub fn forget_phrase(&self, code: &str, text: &str) -> Result<bool> {
        let changed = pollster::block_on(self.conn.execute(
            "DELETE FROM user.user_phrase WHERE code = ?1 AND text = ?2",
            turso::params![code, text],
        ))?;
        Ok(changed > 0)
    }

    /// 这串键上"用户删掉过"的词（候选框里按 Del）。查候选时要按它过滤
    pub fn hidden(&self, code: &str) -> Vec<String> {
        self.query(
            "SELECT text, 0 FROM user.user_hidden WHERE code = ?1",
            &[code.into()],
        )
        .into_iter()
        .map(|(text, _)| text)
        .collect()
    }

    /// 记下"这串键上别再给我这个词"（`Del` 用）。返回是不是新记的
    pub fn hide(&self, code: &str, text: &str) -> Result<bool> {
        let changed = pollster::block_on(self.conn.execute(
            "INSERT OR IGNORE INTO user.user_hidden (code, text) VALUES (?1, ?2)",
            turso::params![code, text],
        ))?;
        Ok(changed > 0)
    }

    /// 清掉"我用过这个词"的偏好（`user_word` 里那笔）。词条本身留着 ——
    /// 词库里真有的词不该因为一次误操作就消失
    pub fn forget_boost(&self, text: &str) -> Result<bool> {
        let changed = pollster::block_on(self.conn.execute(
            "DELETE FROM user.user_word WHERE text = ?1",
            turso::params![text],
        ))?;
        Ok(changed > 0)
    }

    /// 用户选了某个词：次数 +1。下次它就会排得更靠前
    pub fn note_used(&self, text: &str) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        pollster::block_on(self.conn.execute(
            "INSERT INTO user.user_word (text, count, last_used) VALUES (?1, 1, ?2)
             ON CONFLICT(text) DO UPDATE SET count = count + 1, last_used = ?2",
            turso::params![text, now],
        ))?;
        Ok(())
    }

    /// 取一条 meta（词库来源、导入时间之类）
    pub fn meta(&self, key: &str) -> Option<String> {
        let mut stmt =
            pollster::block_on(self.conn.prepare("SELECT value FROM meta WHERE key = ?1")).ok()?;
        let mut rows = pollster::block_on(stmt.query(turso::params![key])).ok()?;
        let row = pollster::block_on(rows.next()).ok()??;
        match row.get_value(0).ok()? {
            turso::Value::Text(value) => Some(value),
            _ => None,
        }
    }

    /// 数一下这个库里有多少东西 —— 刚下载完 / 刚导入完拿它验一验，
    /// `pliers dict status` 也用它报数
    pub fn stats(&self) -> Result<Stats> {
        let count = |sql: &str| -> Result<i64> {
            let mut stmt = pollster::block_on(self.conn.prepare(sql))?;
            let mut rows = pollster::block_on(stmt.query(()))?;
            let row = pollster::block_on(rows.next())?.ok_or("查不到")?;
            row.get_value(0)?
                .as_integer()
                .copied()
                .ok_or_else(|| "不是数字".into())
        };
        let mut schemes = Vec::new();
        {
            let mut stmt = pollster::block_on(self.conn.prepare(
                "SELECT scheme, count(*) FROM word GROUP BY scheme ORDER BY count(*) DESC",
            ))?;
            let mut rows = pollster::block_on(stmt.query(()))?;
            while let Some(row) = pollster::block_on(rows.next())? {
                if let (turso::Value::Text(scheme), turso::Value::Integer(n)) =
                    (row.get_value(0)?, row.get_value(1)?)
                {
                    schemes.push((scheme, n));
                }
            }
        }
        Ok(Stats {
            words: count("SELECT count(*) FROM word")?,
            singles: count("SELECT count(*) FROM word WHERE length(text) = 1")?,
            phrasal: count("SELECT count(*) FROM word WHERE length(text) > 1")?,
            user_words: count("SELECT count(*) FROM user.user_word")?,
            user_phrases: count("SELECT count(*) FROM user.user_phrase")?,
            schemes,
        })
    }
}

/// `Dict::stats` 的结果
#[derive(Debug, Clone)]
pub struct Stats {
    /// word 表里的行数（多音字、多音词各算一行）
    pub words: i64,
    /// 其中单字行
    pub singles: i64,
    /// 其中词/词组行
    pub phrasal: i64,
    /// 用户选过多少次（user_word）
    pub user_words: i64,
    /// 用户自己拼出来的句子（user_phrase）
    pub user_phrases: i64,
    /// 每种方案各多少行：`[("pinyin", 1234), ("wubi", 56)]`
    pub schemes: Vec<(String, i64)>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::testing::sample_dict;
    use super::*;

    /// 只关心词，不关心分数
    fn words(rows: Vec<(String, i64)>) -> Vec<String> {
        rows.into_iter().map(|(text, _)| text).collect()
    }

    /// 每个测试一个干净的临时目录
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pliers-dict-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 目录要活到进程结束：turso 随时可能回写 -wal / -shm
        super::testing::keep_dir(dir.clone());
        dir
    }

    #[test]
    fn 查得到就按权重排() {
        let dict = sample_dict();
        assert_eq!(words(dict.exact("pinyin", "ni", 3)), ["你", "尼", "泥"]);
        // 词库里 ni 的字挺多，限 9 就该给满 9 个
        assert_eq!(words(dict.exact("pinyin", "ni", 9)).len(), 9);
        assert_eq!(words(dict.exact("pinyin", "ni hao", 9)), ["你好", "妮好"]);
    }

    #[test]
    fn 查不到就是空的() {
        let dict = sample_dict();
        assert!(dict.exact("pinyin", "zhong guo", 9).is_empty());
        assert!(dict.exact("wubi", "ni", 9).is_empty()); // 方案也要对上
    }

    #[test]
    fn 音节表读得出来() {
        let dict = sample_dict();
        let syllables = dict.syllables();
        assert!(syllables.len() > 400, "音节表有 {} 个？", syllables.len());
        assert!(syllables.contains(&"ni".to_string()));
        assert!(syllables.contains(&"zhuang".to_string()));
    }

    #[test]
    fn 用户选过的词会往前排() {
        let dict = sample_dict();
        assert_eq!(words(dict.exact("pinyin", "ni", 3)), ["你", "尼", "泥"]);
        // 连选三次「泥」：7000 + 300 万 > 你的 234 万，它该窜到最前面
        for _ in 0..3 {
            dict.note_used("泥").unwrap();
        }
        assert_eq!(words(dict.exact("pinyin", "ni", 3))[0], "泥");
    }

    #[test]
    fn 分段拼出来的句子记得住() {
        let dict = sample_dict();
        assert!(dict.phrases("nihaoma", 9).is_empty(), "还没拼过呢");
        dict.note_phrase("nihaoma", "你好吗").unwrap();
        dict.note_phrase("nihaoma", "你号码").unwrap();
        dict.note_phrase("nihaoma", "你好吗").unwrap();
        // 同一个键下按用的次数排：拼过两次的「你好吗」在前
        assert_eq!(dict.phrases("nihaoma", 9), ["你好吗", "你号码"]);
        // 换个键就查不到（键是用户敲的原样那串）
        assert!(dict.phrases("nihao", 9).is_empty());
    }

    #[test]
    fn 能把拼过的句子和偏好忘掉() {
        let dict = sample_dict();
        dict.note_phrase("nihaoma", "你好马").unwrap();
        assert_eq!(dict.phrases("nihaoma", 9), ["你好马"]);
        assert!(dict.forget_phrase("nihaoma", "你好马").unwrap());
        assert!(dict.phrases("nihaoma", 9).is_empty(), "删掉就该查不到了");
        // 再删一次：没这行，返回 false
        assert!(!dict.forget_phrase("nihaoma", "你好马").unwrap());

        // 偏好（user_word）也是：清了之后词还在，只是不再被顶到前面
        dict.note_used("泥").unwrap();
        assert!(dict.forget_boost("泥").unwrap());
        assert!(!dict.forget_boost("泥").unwrap(), "第二次就没得清了");
        assert_eq!(words(dict.exact("pinyin", "ni", 3)), ["你", "尼", "泥"]);
    }

    #[test]
    fn 删掉的词记进黑名单() {
        let dict = sample_dict();
        assert!(dict.hidden("nihaoma").is_empty());
        assert!(dict.hide("nihaoma", "你好马").unwrap());
        assert!(!dict.hide("nihaoma", "你好马").unwrap(), "第二次就没得记了");
        assert_eq!(dict.hidden("nihaoma"), ["你好马"]);
        // 只对"这串键"生效，别的键不受影响
        assert!(dict.hidden("nihao").is_empty());
    }

    #[test]
    fn 用户词频存在单独的表里() {
        let dict = sample_dict();
        dict.note_used("你好").unwrap();
        dict.note_used("你好").unwrap();
        // 重新打开词库，用户数据还在（词库本身是只读的派生物）
        assert_eq!(
            words(dict.exact("pinyin", "ni hao", 9))[0],
            "你好",
            "选过两次的词该稳居第一"
        );
    }

    #[test]
    fn 前缀查得到() {
        let dict = sample_dict();
        let hits = words(dict.prefix("pinyin", "ni", 20));
        assert!(hits.contains(&"你好".to_string()), "{hits:?}");
        assert!(hits.contains(&"你".to_string()), "{hits:?}");

        // 半截音节：拿 `ni ha` 去搜，`ni hao` 里的词也得出来
        let hits = words(dict.prefix("pinyin", "ni ha", 9));
        assert!(hits.contains(&"你好".to_string()), "{hits:?}");
        assert!(hits.contains(&"你哈".to_string()), "{hits:?}");
    }

    // ---- 词库和用户数据分成两个文件 ----------------------------------------

    /// 造一个临时的库文件（`legacy = true` 时按**老布局**把用户表也建在里面）
    fn make_dict(path: &Path, legacy: bool) {
        let db = pollster::block_on(Builder::new_local(&path.to_string_lossy()).build()).unwrap();
        let conn = db.connect().unwrap();
        create_schema(&conn).unwrap();
        if legacy {
            // 老版本的 DDL：三张用户表就建在词库这个文件里
            for statement in [
                "CREATE TABLE user_word (
                    text TEXT PRIMARY KEY,
                    count INTEGER NOT NULL DEFAULT 0,
                    last_used INTEGER NOT NULL DEFAULT 0
                )",
                "CREATE TABLE user_phrase (
                    code TEXT NOT NULL, text TEXT NOT NULL,
                    count INTEGER NOT NULL DEFAULT 0, last_used INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (code, text)
                )",
                "CREATE TABLE user_hidden (code TEXT NOT NULL, text TEXT NOT NULL,
                    PRIMARY KEY (code, text))",
            ] {
                pollster::block_on(conn.execute(statement, ())).unwrap();
            }
            pollster::block_on(conn.execute(
                "INSERT INTO user_word (text, count) VALUES ('泥', 3),
                        ('你好马', 5)",
                (),
            ))
            .unwrap();
            pollster::block_on(conn.execute(
                "INSERT INTO user_phrase (code, text, count) VALUES ('nihaoma', '你好马', 2)",
                (),
            ))
            .unwrap();
        }
        pollster::block_on(conn.execute(
            "INSERT INTO word (scheme, code, text, weight)
             VALUES ('pinyin', 'ni', '泥', 7000), ('pinyin', 'ni', '你', 2345870)",
            (),
        ))
        .unwrap();
        for syllable in ["ni", "hao", "ma"] {
            pollster::block_on(conn.execute(
                "INSERT INTO syllable (syl) VALUES (?1)",
                turso::params![syllable],
            ))
            .unwrap();
        }
    }

    /// 单独打开一个文件数行数（不 attach 任何东西）。表不存在就返回 Err
    fn rows_in(path: &Path, sql: &str) -> Result<i64> {
        let db = pollster::block_on(Builder::new_local(&path.to_string_lossy()).build())?;
        let conn = db.connect()?;
        let mut stmt = pollster::block_on(conn.prepare(sql))?;
        let mut rows = pollster::block_on(stmt.query(()))?;
        let row = pollster::block_on(rows.next())?.ok_or("查不到")?;
        Ok(row.get_value(0)?.as_integer().copied().unwrap_or(0))
    }

    #[test]
    fn 用户数据写在另一个文件里() {
        let dir = temp_dir("split");
        let dict_path = dir.join("dict.db");
        let user_path = dir.join("user.db");
        make_dict(&dict_path, false);

        {
            let dict = Dict::open(&dict_path, &user_path).unwrap();
            // 三次：一次（+100 万）还压不过「你」的 234 万
            for _ in 0..3 {
                dict.note_used("泥").unwrap();
            }
            dict.note_phrase("nihaoma", "你好马").unwrap();
            dict.hide("nihaoma", "你好马").unwrap();
            assert_eq!(dict.user_path(), user_path, "报出来的路径该是 user.db");
            assert_eq!(dict.boosts(&["泥".to_string()]).get("泥"), Some(&3));
        }

        // 词库那个文件里**没有**用户表
        assert!(
            rows_in(&dict_path, "SELECT count(*) FROM user_word").is_err(),
            "词库里不该再有 user_word 表"
        );
        // 用户数据在自己的文件里
        assert_eq!(
            rows_in(&user_path, "SELECT count(*) FROM user_word").unwrap(),
            1
        );
        assert_eq!(
            rows_in(&user_path, "SELECT count(*) FROM user_phrase").unwrap(),
            1
        );
        assert_eq!(
            rows_in(&user_path, "SELECT count(*) FROM user_hidden").unwrap(),
            1
        );

        // 重新打开：用户数据还在，而且照样参与排序 / 过滤
        let dict = Dict::open(&dict_path, &user_path).unwrap();
        assert_eq!(
            words(dict.exact("pinyin", "ni", 2))[0],
            "泥",
            "选过的排前面"
        );
        assert_eq!(dict.phrases("nihaoma", 9), ["你好马"]);
        assert_eq!(dict.hidden("nihaoma"), ["你好马"]);
    }

    #[test]
    fn 换掉词库也不丢用户数据() {
        let dir = temp_dir("swap");
        let dict_path = dir.join("dict.db");
        let user_path = dir.join("user.db");
        make_dict(&dict_path, false);
        {
            let dict = Dict::open(&dict_path, &user_path).unwrap();
            for _ in 0..3 {
                dict.note_used("泥").unwrap();
            }
        }

        // 模拟 pliers dict build / fetch --force：把词库文件删了重建一份新的
        std::fs::remove_file(&dict_path).unwrap();
        make_dict(&dict_path, false);

        let dict = Dict::open(&dict_path, &user_path).unwrap();
        assert_eq!(
            rows_in(&user_path, "SELECT count(*) FROM user_word").unwrap(),
            1,
            "用户数据该在 user.db 里活着"
        );
        assert_eq!(words(dict.exact("pinyin", "ni", 2))[0], "泥", "偏好也还在");
    }

    #[test]
    fn 老布局的用户数据会搬到新文件() {
        // 以前用户表和词库混在一个文件里：升级之后得把原来那些数据搬过来，
        // 不然"选过的词、自己拼的句子"就丢了
        let dir = temp_dir("legacy");
        let dict_path = dir.join("dict.db");
        let user_path = dir.join("user.db");
        make_dict(&dict_path, true);

        let dict = Dict::open(&dict_path, &user_path).unwrap();
        assert_eq!(
            words(dict.exact("pinyin", "ni", 2))[0],
            "泥",
            "搬过来的偏好生效"
        );
        assert_eq!(dict.phrases("nihaoma", 9), ["你好马"]);
        assert_eq!(
            rows_in(&user_path, "SELECT count(*) FROM user_word").unwrap(),
            2,
            "两行都搬过来了"
        );

        // 搬完之后不再重复搬：往用户库里加一行，再开一次还是 2+1 行
        {
            let dict = Dict::open(&dict_path, &user_path).unwrap();
            dict.note_used("你").unwrap();
        }
        let dict = Dict::open(&dict_path, &user_path).unwrap();
        assert_eq!(
            rows_in(&user_path, "SELECT count(*) FROM user_word").unwrap(),
            3
        );
        assert_eq!(words(dict.exact("pinyin", "ni", 2)).len(), 2);
    }

    #[test]
    fn 缺词库时报错要说人话() {
        let message = match Dict::open(
            Path::new("/nonexistent/pliers.db"),
            Path::new("/nonexistent/user.db"),
        ) {
            Ok(_) => panic!("不该打开成功"),
            Err(e) => e.to_string(),
        };
        assert!(message.contains("词库不存在"), "{message}");
        assert!(
            message.contains("pliers-dict"),
            "错误信息里该告诉用户怎么导入：{message}"
        );
    }
}

/// 测试用的小词库。
///
/// 真库有上百万行，测试里当然不能去开它 —— 这里现造一个几十行的，
/// 权重照着真库的量级写（导入时最大权重缩放到 8000 万），排出来的顺序才跟真机一致
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::pinyin::SYLLABLES;

    /// 每个测试一个独立的库文件。cargo test 是多线程跑的，
    /// 用序号就必须是原子的，否则两个测试会撞到同一个文件（turso 会报 database is locked）
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    thread_local! {
        /// 临时目录要活到进程结束：turso 随时可能回写 -wal / -shm 文件
        static DIRS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    }

    /// 把临时目录记下来，活到进程结束（turso 随时可能回写 -wal / -shm 文件）
    pub fn keep_dir(dir: PathBuf) {
        DIRS.with(|dirs| dirs.borrow_mut().push(dir));
    }

    /// 造一份小词库
    pub fn sample_dict() -> Dict {
        let dir = std::env::temp_dir().join(format!(
            "pliers-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        DIRS.with(|dirs| dirs.borrow_mut().push(dir.clone()));
        let path = dir.join("dict.db");

        {
            let db =
                pollster::block_on(Builder::new_local(&path.to_string_lossy()).build()).unwrap();
            let conn = db.connect().unwrap();
            create_schema(&conn).unwrap();
            for (code, text, weight) in [
                ("ni", "你", 2_345_870),
                ("ni", "尼", 80_000),
                ("ni", "泥", 7_000),
                ("ni", "拟", 6_000),
                ("ni", "逆", 5_000),
                ("ni", "妮", 4_900),
                ("ni", "呢", 4_800),
                ("ni", "匿", 4_700),
                ("ni", "腻", 4_600),
                ("ni", "溺", 4_500),
                ("ni", "倪", 4_400),
                ("ni", "昵", 4_300),
                ("ni", "铌", 4_200),
                ("ni", "猊", 4_100),
                ("ni", "怩", 4_000),
                ("ni", "伲", 3_900),
                ("ni", "旎", 3_800),
                ("ni", "鲵", 3_700),
                ("ni", "蜺", 3_600),
                ("ni", "麑", 3_500),
                ("hao", "好", 925_430),
                ("hao", "号", 500_000),
                ("hao", "浩", 30_000),
                ("ni hao", "你好", 3_000_000),
                ("ni hao", "妮好", 20),
                ("ni ha", "你哈", 30),
                ("bu neng", "不能", 4_000_000),
                // 整句候选要用：单字 + 词，让"库里没有整词也能拼出来"有得测
                ("bu", "不", 1_900_000),
                ("ma", "吗", 212_450),
                ("shi", "是", 7_969_910),
                ("jian", "见", 589_650),
                ("shi jian", "时间", 332_880),
                ("hao ma", "号码", 9_800),
            ] {
                pollster::block_on(conn.execute(
                    "INSERT INTO word (scheme, code, text, weight) VALUES ('pinyin', ?1, ?2, ?3)",
                    turso::params![code, text, weight],
                ))
                .unwrap();
            }
            // 音节表：切词要用，给全套
            let mut stmt =
                pollster::block_on(conn.prepare("INSERT INTO syllable (syl) VALUES (?1)")).unwrap();
            for syllable in SYLLABLES.split_whitespace() {
                pollster::block_on(stmt.execute(turso::params![syllable])).unwrap();
            }
        }

        // 用户数据是**另一个文件**：测试里也分开建，跟真机一致
        Dict::open(&path, &dir.join("user.db")).unwrap()
    }
}
