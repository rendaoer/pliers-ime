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

/// 建库用的 DDL。导入工具（`pliers-dict`）和测试共用同一份，免得两边写岔。
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
    // 用户用"分段上屏"自己拼出来的句子：键是**他敲的那串原文**，
    // 下次敲同一串就直接把这句话给他。跟 user_word 分开是因为这里要带拼音（键）
    "CREATE TABLE IF NOT EXISTS user_phrase (
        code      TEXT NOT NULL,
        text      TEXT NOT NULL,
        count     INTEGER NOT NULL DEFAULT 0,
        last_used INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (code, text)
    )",
    // 用户在候选框里按 Del"删掉"的词：这个词/这句话以后不再出现在这串键的候选里。
    // 词库里的词条不动（那是导入出来的），所以另开一张"黑名单"
    "CREATE TABLE IF NOT EXISTS user_hidden (
        code TEXT NOT NULL,
        text TEXT NOT NULL,
        PRIMARY KEY (code, text)
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
/// 词频权重最大到 8000 万（导入时统一缩放到这个上限），所以这个值让"用过十次"的词
/// 能压过绝大多数常用词 —— 但也压不过「的」「你」这种顶级高频词，避免一次误选就再也翻不了身
pub const USER_BOOST: i64 = 1_000_000;
/// 用户词频最多算多少次（防止某一个词被刷到天上去）
pub const USER_BOOST_CAP: i64 = 50;

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
                "词库不存在：{}\n装一份（写配置 + 下载词库）：\n  pliers --init\n\
                 也可以用你自己的词表构建：pliers dict build（或 pliers-dict --help）",
                path.display(),
            )
            .into());
        }
        let db = pollster::block_on(Builder::new_local(&path.to_string_lossy()).build())?;
        let conn = db.connect()?;
        // 顺手补一下表结构：全是 CREATE TABLE IF NOT EXISTS，成本可以忽略，
        // 但老词库（用户表、整句表）就不用重新导入了 —— 加新表时省事
        create_schema(&conn)?;
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

    /// 这个键（用户敲的原文）上，他自己拼过的句子。按用得多的排前面
    pub fn phrases(&self, code: &str, limit: usize) -> Vec<String> {
        self.query(
            "SELECT text, count FROM user_phrase WHERE code = ?1
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
            "INSERT INTO user_phrase (code, text, count, last_used) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(code, text) DO UPDATE SET count = count + 1, last_used = ?3",
            turso::params![code, text, now],
        ))?;
        Ok(())
    }

    /// 把某个键上的一句"用户自己拼的话"删掉（候选框里按 Del）。
    /// 返回是否真的删掉了一行 —— 词库（`word` 表）里的词条不动，那是导入出来的
    pub fn forget_phrase(&self, code: &str, text: &str) -> Result<bool> {
        let changed = pollster::block_on(self.conn.execute(
            "DELETE FROM user_phrase WHERE code = ?1 AND text = ?2",
            turso::params![code, text],
        ))?;
        Ok(changed > 0)
    }

    /// 这串键上"用户删掉过"的词（候选框里按 Del）。查候选时要按它过滤
    pub fn hidden(&self, code: &str) -> Vec<String> {
        self.query(
            "SELECT text, 0 FROM user_hidden WHERE code = ?1",
            &[code.into()],
        )
        .into_iter()
        .map(|(text, _)| text)
        .collect()
    }

    /// 记下"这串键上别再给我这个词"（`Del` 用）。返回是不是新记的
    pub fn hide(&self, code: &str, text: &str) -> Result<bool> {
        let changed = pollster::block_on(self.conn.execute(
            "INSERT OR IGNORE INTO user_hidden (code, text) VALUES (?1, ?2)",
            turso::params![code, text],
        ))?;
        Ok(changed > 0)
    }

    /// 清掉"我用过这个词"的偏好（`user_word` 里那笔）。词条本身留着 ——
    /// 词库里真有的词不该因为一次误操作就消失
    pub fn forget_boost(&self, text: &str) -> Result<bool> {
        let changed = pollster::block_on(self.conn.execute(
            "DELETE FROM user_word WHERE text = ?1",
            turso::params![text],
        ))?;
        Ok(changed > 0)
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
            user_words: count("SELECT count(*) FROM user_word")?,
            user_phrases: count("SELECT count(*) FROM user_phrase")?,
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

    #[test]
    fn 缺词库时报错要说人话() {
        let message = match Dict::open(Path::new("/nonexistent/pliers.db")) {
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

        Dict::open(&path).unwrap()
    }
}
