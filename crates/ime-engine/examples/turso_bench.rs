//! 基准：turso 能不能扛住"每敲一个键查一次库"。
//!
//! ```text
//! cargo run -p ime-engine --example turso_bench
//! ```
//!
//! 结论（20 万行的合成词库）：精确匹配 64µs，宽前缀 `LIKE 'ni%'` 要 **280ms**。
//! 所以拼音切分必须在内存里做 —— 见 `crates/ime-engine/src/dict.rs` 文件头。
//!
//! 要回答三件事：
//! 1. 建库/批量插入有多快（1.5M 行的导入要多久）
//! 2. 前缀范围查询走不走索引、延迟多少
//! 3. LEFT JOIN + 表达式 ORDER BY + LIMIT 这套 SQL turso 支不支持

use std::time::Instant;

use turso::Builder;

const DB: &str = "target/probe.db";

fn block<F: std::future::Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = std::fs::remove_file(DB);
    let _ = std::fs::remove_file(format!("{DB}-wal"));
    let db = block(Builder::new_local(DB).build())?;
    let conn = db.connect()?;
    println!("打开 {DB} 成功");

    block(conn.execute(
        "CREATE TABLE word (
             id INTEGER PRIMARY KEY,
             scheme TEXT NOT NULL,
             code TEXT NOT NULL,
             text TEXT NOT NULL,
             syllables INTEGER NOT NULL,
             weight INTEGER NOT NULL)",
        (),
    ))?;
    block(conn.execute(
        "CREATE TABLE user_word (text TEXT PRIMARY KEY, count INTEGER NOT NULL)",
        (),
    ))?;

    // ---- 造 20 万行，模拟真实词库的规模（真库是 150 万行）----
    let syllables = [
        "ni", "hao", "shi", "jian", "zhong", "guo", "peng", "you", "de", "yi", "zai", "you",
    ];
    let t0 = Instant::now();
    block(conn.execute("BEGIN", ()))?;
    let mut stmt = block(conn.prepare(
        "INSERT INTO word (scheme, code, text, syllables, weight) VALUES (?1, ?2, ?3, ?4, ?5)",
    ))?;
    let mut rows = 0u32;
    for i in 0..200_000u32 {
        let a = syllables[(i as usize * 7) % syllables.len()];
        let b = syllables[(i as usize * 13 + 3) % syllables.len()];
        let code = match i % 4 {
            0 => format!("{a} {b}"),
            1 => format!("{a} {b} {a}"),
            2 => a.to_string(),
            _ => format!("{a} {b} {b} {a}"),
        };
        let text = format!("词{i}");
        let n = code.split(' ').count() as i64;
        let weight = (i % 100_000) as i64;
        block(stmt.execute(turso::params!["pinyin", code, text, n, weight]))?;
        rows += 1;
    }
    drop(stmt);
    block(conn.execute("COMMIT", ()))?;
    let insert = t0.elapsed();
    println!(
        "插入 {rows} 行用了 {insert:?}（{:.0} 行/秒）",
        rows as f64 / insert.as_secs_f64()
    );

    // ---- 索引 ----
    let t0 = Instant::now();
    block(conn.execute("CREATE INDEX word_code ON word (scheme, code)", ()))?;
    println!("建索引用 {:?}", t0.elapsed());

    // ---- 查询计划：走索引了吗 ----
    let sql = "SELECT w.text, w.weight + COALESCE(u.count, 0) * 1000 AS score
               FROM word w LEFT JOIN user_word u ON u.text = w.text
               WHERE w.scheme = ?1 AND w.code >= ?2 AND w.code < ?3
               ORDER BY (w.syllables = ?4) DESC, score DESC
               LIMIT ?5";
    match block(conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))) {
        Ok(mut stmt) => match block(stmt.query(turso::params![
            "pinyin",
            "ni ha",
            "ni ha\u{10ffff}",
            2i64,
            9i64
        ])) {
            Ok(mut r) => {
                while let Some(row) = block(r.next())? {
                    println!("查询计划: {:?}", row.get_value(3));
                }
            }
            Err(e) => println!("EXPLAIN 查询失败: {e}"),
        },
        Err(e) => println!("EXPLAIN 不支持: {e}"),
    }

    // ---- 延迟：模拟"每敲一个键查一次" ----
    for (label, lo, hi, exact) in [
        ("精确 nihao（4 个候选）", "ni hao", "ni hao\u{10ffff}", 2i64),
        ("前缀 ni ha（半截音节）", "ni ha", "ni ha\u{10ffff}", 2i64),
        ("前缀 ni（刚打一个音节）", "ni", "ni\u{10ffff}", 1i64),
        ("前缀 n（刚打一个字母）", "n", "n\u{10ffff}", 1i64),
    ] {
        let mut stmt = block(conn.prepare(sql))?;
        let mut hits = 0;
        let mut worst = std::time::Duration::ZERO;
        let t0 = Instant::now();
        const N: u32 = 50;
        for _ in 0..N {
            let t1 = Instant::now();
            let mut rows = block(stmt.query(turso::params!["pinyin", lo, hi, exact, 9i64]))?;
            let mut n = 0;
            while block(rows.next())?.is_some() {
                n += 1;
            }
            worst = worst.max(t1.elapsed());
            hits = n;
        }
        let avg = t0.elapsed() / N;
        println!("{label}: {hits} 个候选，平均 {avg:?}，最慢 {worst:?}");
    }

    // ---- 用户词频：UPSERT 能不能用 ----
    match block(conn.execute(
        "INSERT INTO user_word (text, count) VALUES (?1, 1)
         ON CONFLICT(text) DO UPDATE SET count = count + 1",
        turso::params!["你好"],
    )) {
        Ok(_) => {
            block(conn.execute(
                "INSERT INTO user_word (text, count) VALUES (?1, 1)
                 ON CONFLICT(text) DO UPDATE SET count = count + 1",
                turso::params!["你好"],
            ))?;
            let mut stmt = block(conn.prepare("SELECT count FROM user_word WHERE text = ?1"))?;
            let mut rows = block(stmt.query(turso::params!["你好"]))?;
            if let Some(row) = block(rows.next())? {
                println!("UPSERT 生效，count = {:?}", row.get_value(0));
            }
        }
        Err(e) => println!("UPSERT 不支持: {e}"),
    }

    let size = std::fs::metadata(DB).map(|m| m.len()).unwrap_or(0);
    println!("库文件 {} KB（20 万行）", size / 1024);
    Ok(())
}
