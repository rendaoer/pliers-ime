//! 把拼音词表导成 pliers 用的 SQLite 词库。
//!
//! ```text
//! cargo run -p pliers-dict --release -- \
//!     --source ~/Downloads/CustomPinyinDictionary_IBus.txt \
//!     --freq   ~/Downloads/jieba-dict.txt \
//!     --out    ~/.local/share/pliers/dict.db
//! ```
//!
//! 手上有三份数据，各管一件事：
//!
//! | 来源 | 提供 | 说明 |
//! | --- | --- | --- |
//! | IBus 词表 | **词 + 拼音**（150 万条） | `词 拼音`，音节用 `'` 分隔，没有词频 |
//! | jieba 词频表 | **权重** | `词 频次 词性`，MIT 许可；没有它的词只能拿一个很小的兜底权重 |
//! | IBus 词表自己 | **单字** | 表里全是 2 字以上的词，单字靠"字↔音节"对齐推出来 |
//!
//! 为什么要费劲推单字：用户打 `ni` 想要的第一个候选是「你」，而词表里根本没有单字条目。
//! 好在词表里每一行的**字数都等于音节数**（我验过 150 万行，零例外），
//! 所以从 `你好 ni'hao` 就能得到 你=ni、好=hao；同一个字在不同词里出现几百次，
//! 取出现最多的那个读音，就是它的主读音（的→de 602 次，di 只有 31 次 ✓）。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use pliers_engine::dict::{create_schema, synthetic_weight};
use turso::{Builder, Connection};

/// 每多少行提交一次（一次性开一个大事务会把内存吃光）
const BATCH: usize = 50_000;

/// 词频表里的次数乘个系数，让它跟"没词频的兜底权重"（个位数）拉开档次
const FREQ_SCALE: i64 = 10;

fn main() {
    if let Err(e) = run() {
        eprintln!("导入失败：{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    let started = Instant::now();

    println!("词表  : {}", args.source.display());
    println!(
        "词频表: {}",
        args.freq
            .as_ref()
            .map_or("（无，全部用兜底权重）".into(), |p| p
                .display()
                .to_string())
    );
    println!("输出  : {}", args.out.display());
    println!();

    // 每次重新导入都从零开始：词库是可以随时重建的派生物，用户数据在 user_word 表里，
    // 但那也一起重建更省事 —— 所以先备份掉（如果存在）
    if args.out.exists() {
        let backup = args.out.with_extension("db.bak");
        std::fs::rename(&args.out, &backup)?;
        println!("旧词库已备份到 {}", backup.display());
    }
    if let Some(dir) = args.out.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let db = pollster::block_on(Builder::new_local(&args.out.to_string_lossy()).build())?;
    let conn = db.connect()?;
    create_schema(&conn)?;

    let freq = match &args.freq {
        Some(path) => load_freq(path)?,
        None => HashMap::new(),
    };
    println!(
        "词频表载入 {} 条（{:.1}s）",
        freq.len(),
        started.elapsed().as_secs_f32()
    );

    // 单字读音：字 → (音节 → 出现次数)
    let mut readings: HashMap<char, HashMap<String, u32>> = HashMap::new();
    let mut syllables: HashMap<String, ()> = HashMap::new();
    let mut words = 0usize;
    let mut with_freq = 0usize;

    let mut stmt = pollster::block_on(conn.prepare(
        "INSERT OR IGNORE INTO word (scheme, code, text, weight) VALUES (?1, ?2, ?3, ?4)",
    ))?;
    pollster::block_on(conn.execute("BEGIN", ()))?;

    let file = BufReader::with_capacity(1 << 20, File::open(&args.source)?);
    for line in file.lines() {
        let line = line?;
        // 每行是「词 拼音」，中间一个空格
        let Some((word, pinyin)) = line.split_once(' ') else {
            continue;
        };
        let word = word.trim();
        let code = pinyin.trim().replace('\'', " ");
        if word.is_empty() || code.is_empty() {
            continue;
        }

        for syl in code.split(' ') {
            syllables.insert(syl.to_string(), ());
        }
        let weight = match freq.get(word) {
            Some(&count) => {
                with_freq += 1;
                count * FREQ_SCALE
            }
            None => synthetic_weight(word.chars().count()),
        };
        // 顺手记下"字 → 读音"：只用双字词，对齐最干净
        let chars: Vec<char> = word.chars().collect();
        let syls: Vec<&str> = code.split(' ').collect();
        if chars.len() == 2 && syls.len() == 2 {
            for (ch, syl) in chars.into_iter().zip(syls) {
                *readings
                    .entry(ch)
                    .or_default()
                    .entry(syl.to_string())
                    .or_default() += 1;
            }
        }

        // code 到这儿才 move（上面还要借它）
        pollster::block_on(stmt.execute(turso::params!["pinyin", code, word, weight]))?;
        words += 1;

        if words.is_multiple_of(BATCH) {
            pollster::block_on(conn.execute("COMMIT", ()))?;
            pollster::block_on(conn.execute("BEGIN", ()))?;
            println!(
                "  {words} 词（{:.0}s，{:.0} 词/秒）",
                started.elapsed().as_secs_f32(),
                words as f32 / started.elapsed().as_secs_f32()
            );
        }
    }
    drop(stmt);
    pollster::block_on(conn.execute("COMMIT", ()))?;
    println!(
        "词表导入完成：{words} 词，其中 {with_freq} 个有真实词频（{:.0}s）",
        started.elapsed().as_secs_f32()
    );

    // ---- 单字 ----
    let mut stmt = pollster::block_on(conn.prepare(
        "INSERT OR IGNORE INTO word (scheme, code, text, weight) VALUES (?1, ?2, ?3, ?4)",
    ))?;
    pollster::block_on(conn.execute("BEGIN", ()))?;
    let mut singles = 0usize;
    for (ch, counts) in &readings {
        // 出现最多的读音就是主读音；打平取字典序小的，保证结果稳定
        let Some((syl, _)) = counts.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) else {
            continue;
        };
        let text = ch.to_string();
        let weight = match freq.get(&text) {
            Some(&count) => count * FREQ_SCALE,
            None => synthetic_weight(1),
        };
        pollster::block_on(stmt.execute(turso::params!["pinyin", syl.as_str(), text, weight]))?;
        singles += 1;
    }
    drop(stmt);
    pollster::block_on(conn.execute("COMMIT", ()))?;
    println!("单字补充：{singles} 个（从双字词的读音推出来的）");

    // ---- 码表（五笔之类）：`词<TAB>码[<TAB>权重]` ----
    if let (Some(path), Some(scheme)) = (&args.table, &args.table_scheme) {
        let rows = import_table(&conn, path, scheme)?;
        println!("码表 {scheme}：导入 {rows} 条（来自 {}）", path.display());
    }

    // ---- 音节表 + meta ----
    let mut stmt =
        pollster::block_on(conn.prepare("INSERT OR IGNORE INTO syllable (syl) VALUES (?1)"))?;
    pollster::block_on(conn.execute("BEGIN", ()))?;
    let mut sorted: Vec<&String> = syllables.keys().collect();
    sorted.sort();
    for syl in &sorted {
        pollster::block_on(stmt.execute(turso::params![syl.as_str()]))?;
    }
    drop(stmt);
    pollster::block_on(conn.execute("COMMIT", ()))?;

    for (key, value) in [
        ("source", args.source.display().to_string()),
        (
            "freq",
            args.freq
                .as_ref()
                .map_or("无".into(), |p| p.display().to_string()),
        ),
        (
            "imported_at",
            format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs()
            ),
        ),
        ("words", words.to_string()),
        ("singles", singles.to_string()),
    ] {
        pollster::block_on(conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            turso::params![key, value],
        ))?;
    }

    let size = std::fs::metadata(&args.out)?.len();
    println!();
    println!(
        "音节 {} 个，词库 {} MB，总共 {:.0}s",
        sorted.len(),
        size / 1024 / 1024,
        started.elapsed().as_secs_f32()
    );
    println!("搞定：{}", args.out.display());
    Ok(())
}

/// 导入码表：每行 `词<TAB>码[<TAB>权重]`（rime 那种 .txt 码表就是这格式）
fn import_table(
    conn: &Connection,
    path: &Path,
    scheme: &str,
) -> Result<usize, Box<dyn std::error::Error>> {
    let mut stmt = pollster::block_on(conn.prepare(
        "INSERT OR IGNORE INTO word (scheme, code, text, weight) VALUES (?1, ?2, ?3, ?4)",
    ))?;
    pollster::block_on(conn.execute("BEGIN", ()))?;
    let mut rows = 0usize;
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split('\t');
        let (Some(text), Some(code)) = (parts.next(), parts.next()) else {
            continue;
        };
        let weight: i64 = parts
            .next()
            .and_then(|w| w.trim().parse().ok())
            .unwrap_or(1);
        pollster::block_on(stmt.execute(turso::params![scheme, code.trim(), text.trim(), weight]))?;
        rows += 1;
    }
    drop(stmt);
    pollster::block_on(conn.execute("COMMIT", ()))?;
    Ok(rows)
}

/// 载入 jieba 词频表：`词 频次 词性`
fn load_freq(path: &Path) -> Result<HashMap<String, i64>, Box<dyn std::error::Error>> {
    let mut map = HashMap::with_capacity(400_000);
    for line in BufReader::with_capacity(1 << 20, File::open(path)?).lines() {
        let line = line?;
        let mut parts = line.split(' ');
        if let (Some(word), Some(count)) = (parts.next(), parts.next())
            && let Ok(count) = count.parse::<i64>()
        {
            map.insert(word.to_string(), count);
        }
    }
    Ok(map)
}

/// 命令行参数
struct Args {
    source: PathBuf,
    freq: Option<PathBuf>,
    out: PathBuf,
    table: Option<PathBuf>,
    table_scheme: Option<String>,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let mut args = Self {
            source: PathBuf::from(format!("{home}/Downloads/CustomPinyinDictionary_IBus.txt")),
            freq: None,
            out: PathBuf::from(format!("{home}/.local/share/pliers/dict.db")),
            table: None,
            table_scheme: None,
        };
        let mut rest = std::env::args().skip(1);
        while let Some(flag) = rest.next() {
            let mut value = || {
                rest.next()
                    .ok_or_else(|| format!("{flag} 后面要跟一个路径"))
            };
            match flag.as_str() {
                "--source" => args.source = PathBuf::from(value()?),
                "--freq" => args.freq = Some(PathBuf::from(value()?)),
                "--out" => args.out = PathBuf::from(value()?),
                "--table" => args.table = Some(PathBuf::from(value()?)),
                "--table-scheme" => args.table_scheme = Some(value()?),
                "--help" | "-h" => {
                    println!(
                        "用法：import-dict [--source 词表] [--freq 词频表] [--out 词库] \
                         [--table 码表 --table-scheme wubi]"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("不认识的参数 {other}").into()),
            }
        }
        let _ = std::io::stdout().flush();
        Ok(args)
    }
}
