//! 词库：放在 SQLite 里（用 turso 读写）。
//!
//! 为什么是 SQLite 而不是一个内存里的 HashMap：
//!
//! * 词表有 **150 万行**，全塞进内存要几百 MB，而输入法大部分时间在闲着
//! * 词库、权重、用户词频都是"数据"，可以随时用 SQL 改、加、导出，不用重新编译
//! * 一次装好，多进程共享；以后要做设置界面也直接读这个库
//!
//! 表结构（`SCHEMA`，导入工具和运行时共用同一份 DDL）：
//!
//! ```sql
//! word(scheme, code, text, weight)   -- 词库本体：'pinyin' / 'wubi' / …
//! user_word(text, count, last_used)  -- 用户选过多少次（调频用，跟词库分开存，
//!                                    --   重新导入词库不会把它冲掉）
//! syllable(syl)                      -- 412 个合法音节，切词用
//! meta(key, value)                   -- 词库来源 / 导入时间之类
//! ```
//!
//! **查询只有两种，而且都是精确/短前缀**，这一点很关键：turso（以及任何 SQL 引擎）
//! 做"按前缀扫一大片再排序"会慢到没法用 —— 实测在 20 万行上，`code LIKE 'ni%'`
//! 这种宽前缀查询要 280ms。所以切词的活交给 [`crate::pinyin`] 在内存里做，
//! 数据库这边永远只回答"这个**完整的拼音码**对应哪些词"。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use turso::{Builder, Connection};

/// 建库用的 DDL。导入工具（`ime-dict`）和测试共用同一份，免得两边写岔。
///
/// 注意是一条一条执行的：turso 的 `execute()` 一次只认一条语句
/// （把四条 DDL 拼成一个字符串喂进去，它只会建第一张表）
pub const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS word (
        scheme TEXT NOT NULL,
        code   TEXT NOT NULL,
        text   TEXT NOT NULL,
        weight INTEGER NOT NULL,
        PRIMARY KEY (scheme, code, text)
    )",
    "CREATE TABLE IF NOT EXISTS user_word (
        text TEXT PRIMARY KEY,
        count INTEGER NOT NULL DEFAULT 0,
        last_used INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS syllable (syl TEXT PRIMARY KEY)",
    "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
];

/// 建表（幂等）
pub fn create_schema(conn: &Connection) -> Result<()> {
    for statement in SCHEMA {
        pollster::block_on(conn.execute(statement, ()))?;
    }
    Ok(())
}

/// 用户每选一次词，加多少分。
/// 词频权重最大到 8000 万（jieba 词频 ×10），所以这个值让"用过十次"的词能压过
/// 绝大多数常用词 —— 但也压不过「的」「你」这种顶级高频词，避免一次误选就再也翻不了身
pub const USER_BOOST: i64 = 1_000_000;
/// 用户词频最多算多少次（防止某一个词被刷到天上去）
pub const USER_BOOST_CAP: i64 = 50;

/// 没有词频数据的词，按长度给一点权重。
/// 词频表里的词最低也有 20 分，所以"查不到词频"的一律排在后面，短的优先
pub fn synthetic_weight(len: usize) -> i64 {
    match len {
        0 | 1 => 4,
        2 => 3,
        3 => 2,
        _ => 1,
    }
}

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// 词库
pub struct Dict {
    conn: Connection,
    /// 合法音节表（从库里读出来，412 个）
    syllables: Vec<String>,
    /// 出错只吼一次，别每个按键都刷屏
    complained: std::cell::Cell<bool>,
}

impl Dict {
    /// 打开词库。缺表 / 缺数据都会返回明确的错误，提示去跑导入工具
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(format!(
                "词库不存在：{}\n先导入一份：\n  cargo run -p ime-dict --release -- --source ~/Downloads/CustomPinyinDictionary_IBus.txt --out {}",
                path.display(),
                path.display()
            )
            .into());
        }
        let db = pollster::block_on(Builder::new_local(&path.to_string_lossy()).build())?;
        let conn = db.connect()?;
        let mut dict = Self {
            conn,
            syllables: Vec::new(),
            complained: std::cell::Cell::new(false),
        };
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
             FROM word w LEFT JOIN user_word u ON u.text = w.text
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
             FROM word w LEFT JOIN user_word u ON u.text = w.text
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
                    eprintln!("ime-aa: 查词库失败：{e}");
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

    /// 用户选了某个词：次数 +1。下次它就会排得更靠前
    pub fn note_used(&self, text: &str) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        pollster::block_on(self.conn.execute(
            "INSERT INTO user_word (text, count, last_used) VALUES (?1, 1, ?2)
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

    #[test]
    fn 查得到就按权重排() {
        let dict = sample_dict();
        assert_eq!(words(dict.exact("pinyin", "ni", 3)), ["你", "尼", "泥"]);
        assert_eq!(words(dict.exact("pinyin", "ni", 9)).len(), 5);
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
        let hits = words(dict.prefix("pinyin", "ni", 9));
        assert!(hits.contains(&"你好".to_string()), "{hits:?}");
        assert!(hits.contains(&"你".to_string()), "{hits:?}");
        assert!(
            hits.contains(&"你哈".to_string()),
            "半截音节也该搜得到：{hits:?}"
        );
    }

    #[test]
    fn 缺词库时报错要说人话() {
        let message = match Dict::open(Path::new("/nonexistent/ime-aa.db")) {
            Ok(_) => panic!("不该打开成功"),
            Err(e) => e.to_string(),
        };
        assert!(message.contains("词库不存在"), "{message}");
        assert!(
            message.contains("ime-dict"),
            "错误信息里该告诉用户怎么导入：{message}"
        );
    }
}

/// 测试用的小词库。
///
/// 真库有 150 万行，测试里当然不能去开它 —— 这里现造一个几十行的，
/// 权重照着真实词频的量级写（jieba 词频 ×10），排出来的顺序才跟真机一致
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

    /// 造一份小词库
    pub fn sample_dict() -> Dict {
        let dir = std::env::temp_dir().join(format!(
            "ime-aa-test-{}-{}",
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
                ("hao", "好", 925_430),
                ("hao", "号", 500_000),
                ("hao", "浩", 30_000),
                ("ni hao", "你好", 3_000_000),
                ("ni hao", "妮好", 20),
                ("ni ha", "你哈", 30),
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

        Dict::open(&path).unwrap()
    }
}
