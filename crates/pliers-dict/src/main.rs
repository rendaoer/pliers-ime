//! 把 rime 词库导成 pliers 用的 SQLite 词库。
//!
//! ```text
//! cargo run -p pliers-dict --release -- \
//!     --rime ~/.cache/pliers/rime-frost \
//!     --out  ~/.local/share/pliers/dict.db
//! ```
//!
//! rime 词库（[白霜拼音](https://github.com/gaboolic/rime-frost) /
//! [雾凇拼音](https://github.com/iDvel/rime-ice) 那种 `.dict.yaml`）每行是
//! `词<TAB>拼音<TAB>权重`，拼音用空格分音节 —— 跟 `word.code` 存的格式**一模一样**，
//! 所以除了跳过 YAML 文件头，几乎不用转换。三个白送的好处：
//!
//! * **多音字不用猜**：`长` 就是两行（`chang` 一行、`zhang` 一行），各自带权重
//! * **不用另挂词频表**：权重是语料统计出来的
//! * **单字是现成的**：字表本身就是 `8105.dict.yaml` 这种文件，不用从双字词里反推
//!
//! 权重会等比缩放到 [`RIME_MAX_WEIGHT`]（跟"用户调频 +100 万/次"同一个量纲）。
//!
//! `pliers dict build` 会自动下语料再调这个程序，平时不用手敲上面的命令。

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use pliers_engine::Layout;
use pliers_engine::dict::create_schema;
use turso::{Builder, Connection};

/// 每多少行提交一次（一次性开一个大事务会把内存吃光）
const BATCH: usize = 50_000;

/// 权重缩放后的上限。
///
/// rime 词库的权重是语料统计的原始频次，量纲和"用户调频"（`user_word` 那
/// 选一次 +100 万）不一定对得上。不缩放的话可能两个方向都出问题：太小则选过一次的词
/// 永远第一、连「的」「你」都翻不了身，太大则调频等于没调。
/// 8000 万是照 jieba 那套（词频 ×10，最大的「的」约 8000 万）定的，
/// 于是"选过十次的词能压过绝大多数常用词，但压不过顶级高频词"这条手感保持不变。
const RIME_MAX_WEIGHT: f64 = 80_000_000.0;

/// 一行 rime 词条：`(词, 拼音, 权重)`。`Err` 是读文件本身出的错
type RimeRow = Result<(String, String, i64), String>;

/// 导入结果：词条数、单字数、音节表
struct Imported {
    words: usize,
    singles: usize,
    syllables: HashSet<String>,
    weight_source: String,
    source: String,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("导入失败：{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    let started = Instant::now();

    println!(
        "rime 词库：{}",
        args.rime
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    );
    println!("输出     ：{}", args.out.display());
    println!();

    // 每次重新导入都从零开始：词库是**可以随时重建的派生物**，用户数据在另一个文件
    //（user.db）里，跟这个文件无关 —— 所以旧库直接扔，不留 .bak（以前留，是因为用户数据
    // 混在里面；现在那 90 MB 的备份只是占地方，重建一次也就几秒）
    if args.out.exists() {
        std::fs::remove_file(&args.out)?;
        println!("覆盖旧词库 {}", args.out.display());
    }
    // 旧库的 WAL / SHM 还留着的话，新库会被它污染
    for extra in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(PathBuf::from(format!("{}{extra}", args.out.display())));
    }
    if let Some(dir) = args.out.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let db = pollster::block_on(Builder::new_local(&args.out.to_string_lossy()).build())?;
    let conn = db.connect()?;
    create_schema(&conn)?;

    let imported = import_rime(&conn, &args.rime, started)?;

    // ---- 码表（五笔之类）：`词<TAB>码[<TAB>权重]` ----
    if let (Some(path), Some(scheme)) = (&args.table, &args.table_scheme) {
        let rows = import_table(&conn, path, scheme)?;
        println!("码表 {scheme}：导入 {rows} 条（来自 {}）", path.display());
    }

    // ---- 音节表 + meta ----
    let mut sorted: Vec<&String> = imported.syllables.iter().collect();
    sorted.sort();
    let mut stmt =
        pollster::block_on(conn.prepare("INSERT OR IGNORE INTO syllable (syl) VALUES (?1)"))?;
    pollster::block_on(conn.execute("BEGIN", ()))?;
    for syl in &sorted {
        pollster::block_on(stmt.execute(turso::params![syl.as_str()]))?;
    }
    drop(stmt);
    pollster::block_on(conn.execute("COMMIT", ()))?;

    for (key, value) in [
        ("source", imported.source.clone()),
        ("freq", imported.weight_source.clone()),
        (
            "imported_at",
            format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_secs()
            ),
        ),
        ("words", imported.words.to_string()),
        ("singles", imported.singles.to_string()),
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

    // 把 WAL 收进主库，让 .db 成为**自包含**的一个文件。
    //
    // 这一步不能省：词库是要被拷来拷去的（发布成 Release 资产、mock 测试拷副本、
    // 手动 scp 到别的机器），只拷 .db 而落下 -wal 的话，拿到的是一个不完整的库 ——
    // 最典型的是"音节表是空的"（音节是最后写的，全在 WAL 里），输入法直接起不来。
    // 注意这条 PRAGMA 会回一行结果，得用 query 把它读掉（execute 会报 unexpected row）
    {
        let mut stmt = pollster::block_on(conn.prepare("PRAGMA wal_checkpoint(TRUNCATE)"))?;
        let mut rows = pollster::block_on(stmt.query(()))?;
        while pollster::block_on(rows.next())?.is_some() {}
    }
    let wal = PathBuf::from(format!("{}-wal", args.out.display()));
    match std::fs::metadata(&wal) {
        Ok(meta) if meta.len() > 0 => println!(
            "注意：{} 还有 {} 字节没并回主库（拷词库时记得连它一起拷）",
            wal.display(),
            meta.len()
        ),
        _ => {}
    }

    println!("搞定：{}", args.out.display());
    Ok(())
}

/// 导入一批 rime 词库文件（也可以是装着它们的目录）
fn import_rime(
    conn: &Connection,
    paths: &[PathBuf],
    started: Instant,
) -> Result<Imported, Box<dyn std::error::Error>> {
    let files = rime_files(paths)?;
    for path in &files {
        println!("  读 {}", path.display());
    }

    // 第一遍：量出最大权重（要等比缩放），顺手收音节
    let mut max_weight = 0i64;
    let mut syllables = HashSet::new();
    let mut lines = 0usize;
    let mut exotic = 0usize;
    for path in &files {
        for row in rime_rows(path)? {
            let (_, code, weight) = row?;
            max_weight = max_weight.max(weight);
            for syl in code.split(' ') {
                if usable_syllable(syl) {
                    syllables.insert(syl.to_string());
                } else {
                    exotic += 1;
                }
            }
            lines += 1;
        }
    }
    if lines == 0 || max_weight == 0 {
        return Err("这几个 .dict.yaml 里一行词条都没读到（格式不对？）".into());
    }
    let scale = RIME_MAX_WEIGHT / max_weight as f64;
    println!("  读到 {lines} 行，最大权重 {max_weight}，等比缩放 ×{scale:.2}");
    if exotic > 0 {
        println!("  筛掉 {exotic} 个偏门音节（三套双拼键位都打不出来的，比如 `lvan`）");
    }

    // 第二遍：写库
    let mut stmt = pollster::block_on(conn.prepare(
        "INSERT OR IGNORE INTO word (scheme, code, text, weight) VALUES (?1, ?2, ?3, ?4)",
    ))?;
    pollster::block_on(conn.execute("BEGIN", ()))?;
    let mut words = 0usize;
    let mut singles = 0usize;
    for path in &files {
        for row in rime_rows(path)? {
            let (text, code, weight) = row?;
            let weight = ((weight as f64 * scale).round() as i64).max(1);
            if text.chars().count() == 1 {
                singles += 1;
            }
            pollster::block_on(stmt.execute(turso::params!["pinyin", code, text, weight]))?;
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
    }
    drop(stmt);
    pollster::block_on(conn.execute("COMMIT", ()))?;
    println!(
        "  词条导入完成：{words} 个（其中单字 {singles} 个）（{:.0}s）",
        started.elapsed().as_secs_f32()
    );

    Ok(Imported {
        words,
        singles,
        syllables,
        weight_source: format!("rime 词库自带权重（等比缩放到上限 {RIME_MAX_WEIGHT:.0}）"),
        source: paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("、"),
    })
}

/// 把 `--rime` 给的路径摊平成文件列表：目录就取里面的 `.dict.yaml`
fn rime_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found: Vec<PathBuf> = std::fs::read_dir(path)?
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|file| has_ext(file, ".dict.yaml"))
                // corrections 是"纠错表"（错词 → 正词），不是候选词库，混进来会出怪候选
                .filter(|file| {
                    !file
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("corrections"))
                })
                .collect();
            found.sort();
            files.extend(found);
        } else if path.exists() {
            files.push(path.clone());
        } else {
            return Err(format!("{} 不存在", path.display()).into());
        }
    }
    if files.is_empty() {
        return Err("没找到 .dict.yaml（--rime 给目录或文件都行）".into());
    }
    Ok(files)
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(ext))
}

/// 一行一行吐 rime 词条：`(词, 拼音, 权重)`
///
/// 跳过空行、`#` 注释，以及 `---` 到 `...` 之间的 YAML 元信息。
/// 码里带大写、数字、符号的行直接跳过（英文缩写、表情那些）
fn rime_rows(path: &Path) -> Result<impl Iterator<Item = RimeRow>, Box<dyn std::error::Error>> {
    let file = BufReader::with_capacity(1 << 20, File::open(path)?);
    let path = path.to_path_buf();
    let mut in_header = false;
    let rows = file.lines().filter_map(move |line| {
        let line = match line {
            Ok(line) => line,
            Err(e) => return Some(Err(format!("读 {} 出错：{e}", path.display()))),
        };
        let line = line.trim_end();
        if line == "---" {
            in_header = true;
            return None;
        }
        if line == "..." {
            in_header = false;
            return None;
        }
        if in_header || line.is_empty() || line.starts_with('#') {
            return None;
        }
        let mut parts = line.split('\t');
        let text = parts.next().unwrap_or("").trim();
        let code = parts.next().unwrap_or("").trim();
        let weight: i64 = parts
            .next()
            .and_then(|w| w.trim().parse().ok())
            .unwrap_or(1);
        if text.is_empty() || !is_pinyin_code(code) {
            return None;
        }
        Some(Ok((text.to_string(), code.to_string(), weight)))
    });
    Ok(rows)
}

/// 普通话音节最长 6 个字母（`chuang` / `shuang` / `zhuang`），更长的必然是脏数据
///
/// 这道卡是给 rime-frost 里的一条脏数据准备的：`均订` 的码写成了 `junding`
/// （本该是 `jun ding`）。这种"音节"进了音节表会把双拼的键位校验带沟里 ——
/// 它去掉声母算出 `unding` 这么个不存在的韵母，于是报
/// 「这套双拼键位缺韵母：unding」，输入法直接起不来
const MAX_SYLLABLE_LEN: usize = 6;

/// 是不是"纯拼音码"：小写字母 + 空格分隔的音节，每个音节不超过 [`MAX_SYLLABLE_LEN`]
fn is_pinyin_code(code: &str) -> bool {
    !code.is_empty()
        && !code.starts_with(' ')
        && code.split(' ').all(|syllable| {
            !syllable.is_empty()
                && syllable.len() <= MAX_SYLLABLE_LEN
                && syllable.chars().all(|c| c.is_ascii_lowercase())
        })
}

/// 这个音节进不进音节表：三套双拼预设**都**得打得出来。
///
/// 不筛的话，rime 词库里那些偏门码能卡住输入法启动：`lvan`（孪/娈 那一串的怪读音，
/// 标准读音是 luan）去掉声母是 `van`，而小鹤的键位表里没有这个韵母 ——
/// 双拼的启动自检会报「这套双拼键位缺韵母：van」，可用户什么都没配错。
/// 这些词在词库里另有常规读音（`luan`）能打，筛掉不影响使用。
///
/// 音节表是**给切词用的**（`Segmenter`），所以标准就是"方案能不能打出来"，
/// 而不是"拼音学上存不存在"
fn usable_syllable(syllable: &str) -> bool {
    ["natural", "flypy", "mspy"]
        .iter()
        .all(|name| Layout::preset(name).is_some_and(|layout| layout.encode(syllable).is_some()))
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

/// 命令行参数
struct Args {
    out: PathBuf,
    rime: Vec<PathBuf>,
    table: Option<PathBuf>,
    table_scheme: Option<String>,
}

impl Args {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let mut args = Self {
            out: PathBuf::from(format!("{home}/.local/share/pliers/dict.db")),
            rime: Vec::new(),
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
                "--out" => args.out = PathBuf::from(value()?),
                "--rime" => args.rime.push(PathBuf::from(value()?)),
                "--table" => args.table = Some(PathBuf::from(value()?)),
                "--table-scheme" => args.table_scheme = Some(value()?),
                "--help" | "-h" => {
                    println!(
                        "用法：pliers-dict --rime <词库.yaml|目录> [--rime ...] [--out 词库]\n\
                         \x20     pliers-dict --table <码表> --table-scheme wubi [--out 词库]\n\
                         \n\
                         --rime    rime 词库（.dict.yaml）。语料从哪来、怎么构建，见 docs/dictionary.md\n\
                         --out     输出的 SQLite 词库（默认 ~/.local/share/pliers/dict.db）\n\
                         --table   码表方案（五笔/郑码/仓颉）：每行 `词<TAB>码[<TAB>权重]`"
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("不认识的参数 {other}").into()),
            }
        }
        if args.rime.is_empty() && args.table.is_none() {
            return Err(
                "要给 --rime <词库>（推荐：pliers dict build 会自动下语料）\n\
                        或者 --table <码表> --table-scheme wubi"
                    .into(),
            );
        }
        let _ = std::io::stdout().flush();
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份最小的 rime 词库：YAML 头、注释、多音字、要跳过的行都在里面
    const FIXTURE: &str = "\
# Rime dictionary
# encoding: utf-8
---
name: test
version: \"2026-01-01\"
sort: by_weight
...
##### 常用
长\tchang\t900
长\tzhang\t100
行\thang\t800
行\txing\t200
你好\tni hao\t500
你好吗\tni hao ma\t50
# 注释行不该进库
QQ\tQQ\t10
没有拼音\t\t99
";

    fn 建一份测试词库(name: &str) -> (PathBuf, Connection) {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test.dict.yaml"), FIXTURE).unwrap();
        let out = dir.join("dict.db");
        let db = pollster::block_on(Builder::new_local(&out.to_string_lossy()).build()).unwrap();
        let conn = db.connect().unwrap();
        create_schema(&conn).unwrap();
        let imported = import_rime(&conn, std::slice::from_ref(&dir), Instant::now()).unwrap();
        // 6 行词条：注释、YAML 头、`QQ QQ`（码不是纯小写）、空码那行都不算
        assert_eq!(imported.words, 6, "词条数不对");
        assert_eq!(imported.singles, 4, "单字数不对");
        (dir, conn)
    }

    fn 查(conn: &Connection, code: &str) -> Vec<(String, i64)> {
        let mut stmt = pollster::block_on(conn.prepare(
            "SELECT text, weight FROM word WHERE scheme = 'pinyin' AND code = ?1 ORDER BY weight DESC",
        ))
        .unwrap();
        let mut rows = pollster::block_on(stmt.query(turso::params![code])).unwrap();
        let mut out = Vec::new();
        while let Some(row) = pollster::block_on(rows.next()).unwrap() {
            if let (turso::Value::Text(text), turso::Value::Integer(weight)) =
                (row.get_value(0).unwrap(), row.get_value(1).unwrap())
            {
                out.push((text, weight));
            }
        }
        out
    }

    #[test]
    fn rime_多音字一行一个读音的导进来() {
        let (dir, conn) = 建一份测试词库("pliers-rime-import");
        // 多音字不用猜：两边都收着，而且各自带自己的权重
        assert_eq!(查(&conn, "chang")[0].0, "长");
        assert_eq!(查(&conn, "zhang")[0].0, "长");
        assert_eq!(查(&conn, "hang")[0].0, "行");
        assert_eq!(查(&conn, "xing")[0].0, "行");
        // 词条原样带音节空格
        assert_eq!(查(&conn, "ni hao")[0].0, "你好");
        assert_eq!(查(&conn, "ni hao ma")[0].0, "你好吗");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rime_权重等比缩放到同一个上限() {
        let (dir, conn) = 建一份测试词库("pliers-rime-weight");
        // 最大权重（900）缩到 RIME_MAX_WEIGHT，其余按同样的比例
        let chang = 查(&conn, "chang")[0].1;
        let zhang = 查(&conn, "zhang")[0].1;
        assert_eq!(chang, RIME_MAX_WEIGHT as i64);
        // 900 : 100 = 9 : 1，缩放后还得是这个比例（四舍五入允许差 1）
        let ratio = chang as f64 / zhang as f64;
        assert!((ratio - 9.0).abs() < 0.01, "比例变了：{ratio}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 不是拼音码的行跳过() {
        assert!(is_pinyin_code("ni hao"));
        assert!(is_pinyin_code("chang"));
        assert!(
            is_pinyin_code("zhuang"),
            "6 个字母是合法音节（chuang/shuang 那种）"
        );
        assert!(!is_pinyin_code(""));
        assert!(!is_pinyin_code("QQ"), "大写不是拼音");
        assert!(!is_pinyin_code("ni2 hao"), "带数字不是拼音");
        assert!(!is_pinyin_code(" ni hao"), "开头的空格是脏数据");
        assert!(!is_pinyin_code("ni  hao"), "两个空格中间夹了个空音节");
        assert!(!is_pinyin_code("ni\thao"), "tab 不该出现在码里");
        // rime-frost 里真的有一条：`均订` 的码是 `junding`（本该是 `jun ding`），
        // 放进去会让双拼的键位校验报出"缺韵母 unding"
        assert!(!is_pinyin_code("junding"), "7 个字母的音节不存在");
        assert!(
            !is_pinyin_code("jun junding"),
            "整行里有一个坏音节就整行不要"
        );
    }

    #[test]
    fn 目录里读_yaml_但不要纠错表() {
        let dir = std::env::temp_dir().join("pliers-rime-files");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["base.dict.yaml", "corrections.dict.yaml", "readme.md"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let files = rime_files(std::slice::from_ref(&dir)).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["base.dict.yaml"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
