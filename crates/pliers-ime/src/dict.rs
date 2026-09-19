//! 字典的 `pinyin` 那种：拼音词库（`pinyin.db`）加它旁边的用户数据 `user.db`。
//! 五笔这类码表是**另一个库**（`wubi.db`），文档里也在这篇 —— 见 `build_wubi` / `wubi_report`
//!
//! 词库是几百 MB 的派生物，不进仓库（GitHub 也不让放这么大的文件），
//! 但也不该让你自己去哪儿找一份词表、再守着跑一遍导入：
//!
//! * `pliers --init`        写配置 + 装一份能用的（下载预构建的，或 `--build` 自己构建）
//! * `pliers fetch pinyin`  只装拼音词库（下载统一在 `fetch` 那边，跟英文词表一套）
//! * `pliers build pinyin`  自己下语料、自己构建（Release 上还没有东西时走这条）
//! * `pliers build wubi`    用你自己的码表建码表库（`wubi.db`）
//! * `pliers status pinyin` 现在用的是哪个库、多少词、来源是什么
//!
//! 语料用的是[白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)（GPL-3.0）：
//! 字频词频是拿 7.4 亿字语料重新统计的，多音字就是**一行一个读音**（各自带权重，
//! 不用猜），格式还正好是 `词<TAB>拼音<TAB>权重` —— 跟 `word.code` 一模一样。

use std::path::{Path, PathBuf};

use pliers_engine::{Config, Dict};

use crate::args::{has, value_of};
use crate::{note, row, size};

/// 语料：rime-frost 的词表。`corrections`（纠错表）不要，它不是候选词库
///
/// 给两个源：`raw.githubusercontent.com` 在国内经常连不上（我自己就先撞了这堵墙），
/// jsDelivr 是同一份文件的 CDN，按分支取。前一个失败就换后一个
const CORPUS_MIRRORS: &[&str] = &[
    "https://raw.githubusercontent.com/gaboolic/rime-frost/master/cn_dicts",
    "https://cdn.jsdelivr.net/gh/gaboolic/rime-frost@master/cn_dicts",
];
const CORPUS_FILES: &[&str] = &["8105", "41448", "base", "ext", "tencent", "others"];

/// 下一个语料文件，源挨个试。下到 `.part` 再改名（`fetch::install` 干的），
/// 所以半截的下载不会被下次当成"已经有了"
fn fetch_corpus(name: &str, dest: &Path) -> Result<(), String> {
    let mut last = String::new();
    for base in CORPUS_MIRRORS {
        match pliers_engine::fetch::install(&format!("{base}/{name}.dict.yaml"), dest) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(format!("下载失败：{last}"))
}

/// `pliers --init [--force] [--build] [--url <地址>]`
pub fn init(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let force = has(args, "--force");
    let build_it = has(args, "--build");
    let url = value_of(args, "--url");

    // ---- 配置 ----
    let config = pliers_engine::config::config_path();
    if config.exists() && !force {
        println!("配置：{}（已有，没动它）", config.display());
    } else {
        crate::init_config(force)?;
    }

    // ---- 英文词表（另一种字典，顺带装一份；离线也能装，写的是二进制里那份兜底）----
    crate::english::install_if_missing(force)?;

    // ---- 拼音词库 ----
    let dest = path();
    if dest.exists() && !force {
        println!("词库：{}（已有，没动它）", dest.display());
        // 跑着的实例会独占词库，这时候读不进去很正常 —— 这不是错，
        // 别让 `pliers --init` 带着一个"失败"的退出码结束
        if let Err(e) = describe_path(&dest) {
            println!("{}", row("大小", size(&dest).unwrap_or_default()));
            println!("{}", row("内容", format!("读不了：{e}")));
            println!("{}", note("输入法正开着的话词库被它独占着，退掉再看"));
        }
        println!();
        println!(
            "想重装：pliers fetch pinyin --force（整套：pliers fetch all；自己构建：pliers build pinyin）"
        );
        return Ok(());
    }
    if build_it {
        return build(false);
    }
    crate::fetch::install("pinyin", url.as_deref(), true)?;
    println!();
    describe_path(&dest)?;
    println!();
    println!("搞定 —— 重启输入法就生效（词库是启动时打开的）");
    Ok(())
}

/// 下载语料 + 调 `pliers-dict` 构建一份词库（`pliers build pinyin [--refresh]`）
pub fn build(refresh: bool) -> Result<(), Box<dyn std::error::Error>> {
    let dest = path();
    let corpus = corpus_dir();
    std::fs::create_dir_all(&corpus)?;
    println!("语料目录：{}", corpus.display());
    for name in CORPUS_FILES {
        let file = corpus.join(format!("{name}.dict.yaml"));
        if file.exists() && !refresh {
            continue;
        }
        print!("  下载 {name}.dict.yaml … ");
        std::io::Write::flush(&mut std::io::stdout())?;
        fetch_corpus(name, &file).map_err(|e| {
            format!(
                "{e}\n\
                 语料下不动就挂个代理再试（curl 认 https_proxy），或者从浏览器下好这六个文件\n\
                 放到 {} 里",
                corpus.display()
            )
        })?;
        println!("{} MB", std::fs::metadata(&file)?.len() / 1024 / 1024);
    }

    let importer = importer_binary().ok_or(
        "找不到 pliers-dict（自己构建要它）\n\
         装一个：cargo install pliers-dict\n\
         在仓库里的话：cargo build --release -p pliers-dict，或者直接用：\n\
         \x20 cargo run -p pliers-dict --release -- --rime <语料目录> --out <词库路径>",
    )?;
    println!();
    println!(
        "构建：{} --rime {} --out {}",
        importer.display(),
        corpus.display(),
        dest.display()
    );
    let status = std::process::Command::new(&importer)
        .arg("--rime")
        .arg(&corpus)
        .arg("--out")
        .arg(&dest)
        .status()?;
    if !status.success() {
        return Err(format!("构建失败（退出码 {:?}）", status.code()).into());
    }
    println!();
    describe_path(&dest)?;
    println!();
    println!("搞定 —— 重启输入法就生效");
    Ok(())
}

/// `pliers build wubi <码表.txt>`：用自己的码表构建**码表库**（`wubi.db`）。
///
/// 码表是几 MB 的纯文本，各家的码还不一样（86 / 98 / 新世纪 / 极点…），所以没有预构建的
/// 资产可下 —— 这一步就是"把你手上那份导进去"。`scheme` 是这批码在库里的名字，
/// 也是配置里 `scheme.name` 要写的那个
pub fn build_wubi(
    table: Option<&str>,
    scheme: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(table) = table else {
        return Err("要给码表文件：pliers build wubi <码表.txt>\n\
                    \x20     码表每行是 `词<TAB>码[<TAB>权重]`，比如：\n\
                    \x20       你\tnin\n\
                    \x20       好\tvbg\t200\n\
                    \x20     （码表从哪来：你那套输入法/码表项目里那份 .txt，rime 的码表格式正好是这个）"
            .into());
    };
    let scheme = scheme.map(str::to_string).unwrap_or_else(wubi_scheme_name);
    let dest = wubi_path();
    let importer = importer_binary().ok_or(
        "找不到 pliers-dict（导入码表要它）\n\
         装一个：cargo install pliers-dict\n\
         在仓库里的话：cargo build --release -p pliers-dict",
    )?;
    println!(
        "构建：{} --table {table} --table-scheme {scheme} --out {}",
        importer.display(),
        dest.display()
    );
    let status = std::process::Command::new(&importer)
        .arg("--table")
        .arg(table)
        .arg("--table-scheme")
        .arg(&scheme)
        .arg("--out")
        .arg(&dest)
        .status()?;
    if !status.success() {
        return Err(format!("构建失败（退出码 {:?}）", status.code()).into());
    }
    println!();
    describe_path(&dest)?;
    println!();
    println!("搞定 —— 重启输入法就生效；方案里要写 kind = \"wubi\"（码表名 {scheme:?}）");
    Ok(())
}

/// `pliers status pinyin`：拼音词库这一块现在是什么样
pub fn report() -> Result<(), Box<dyn std::error::Error>> {
    let dest = path();
    let user = user_path();
    let how_big = size(&dest).unwrap_or_else(|| "还没有".to_string());
    println!(
        "{}",
        row("拼音词库", format!("{}（{how_big}）", dest.display()))
    );
    if !dest.exists() {
        println!(
            "{}",
            note("还没有 —— pliers --init 装一份，或 pliers build pinyin 自己构建")
        );
        return Ok(());
    }
    if let Err(e) = contents(&dest, true) {
        // 输入法开着的时候词库被它独占（turso 是独占锁），读不进去很正常 ——
        // 这不是词库坏了，所以只说清楚原因，不当成错误
        println!("{}", row("内容", format!("读不了：{e}")));
        println!(
            "{}",
            note(
                "输入法正开着的话词库被它独占着，退掉再看；实例自己报的现状在 pliers status 最上面"
            )
        );
        println!("{}", user_row(&user, None));
    }
    // 老名字那份还在？说一句 —— 迁移之后又被写出来一份（比如老版本的实例刚好在下载），
    // 或者当初改名没成功。几十上百 MB 占地方，而且看久了容易看花眼
    if let Some((legacy, how_big)) = leftover_legacy(&dest) {
        println!(
            "{}",
            note(format!(
                "注意：{}（{how_big}）是老名字那份，已经不读了 —— 确认没用了可以删掉",
                legacy.display()
            ))
        );
    }
    Ok(())
}

/// 新名字的库在、旁边还躺着一份老名字的 `dict.db`（已经不读了）
fn leftover_legacy(path: &Path) -> Option<(PathBuf, String)> {
    if !path.exists() || path.file_name()? != "pinyin.db" {
        return None;
    }
    let legacy = path.with_file_name("dict.db");
    Some((legacy.clone(), size(&legacy)?))
}

/// `pliers status wubi`：码表库（五笔/郑码/仓颉）这一块 —— 它跟拼音词库是**两个文件**
pub fn wubi_report() -> Result<(), Box<dyn std::error::Error>> {
    let dest = wubi_path();
    let how_big = size(&dest).unwrap_or_else(|| "还没有".to_string());
    println!(
        "{}",
        row("码表库", format!("{}（{how_big}）", dest.display()))
    );
    if !dest.exists() {
        println!(
            "{}",
            note("还没有 —— 用自己的码表构建一份（每行 `词<TAB>码[<TAB>权重]`）：")
        );
        println!("{}", note("pliers build wubi <码表.txt>"));
        // 老布局：五笔的条目以前跟拼音挤在 dict.db 里（靠 word.scheme 分），现在不读了
        if let Some(rows) = legacy_wubi_rows() {
            println!(
                "{}",
                note(format!(
                    "注意：{} 里还有 {rows} 条 scheme = \"wubi\" 的老数据，现在**不再读它**了 ——",
                    path().display()
                ))
            );
            println!(
                "{}",
                note("拿原来那份码表重新构建一次就有 wubi.db 了（拼音词库不用动）")
            );
        }
        return Ok(());
    }
    // 用户数据不在这儿报：两个库共用同一个 user.db（记的是"你选过哪个词"），
    // 拼音词库那一块已经报过了
    match contents(&dest, false) {
        Ok(schemes) => {
            let want = wubi_scheme_name();
            if !schemes.iter().any(|(name, _)| name == &want) {
                println!(
                    "{}",
                    note(format!(
                        "里面没有 scheme = {want:?} 的行 —— 构建时的 --table-scheme 和配置里的 scheme.name 得对上"
                    ))
                );
            }
        }
        Err(e) => {
            println!("{}", row("内容", format!("读不了：{e}")));
            println!(
                "{}",
                note("输入法正开着的话库被它独占着（用的是哪一份看实例那块的「词库」），退掉再看")
            );
        }
    }
    Ok(())
}

/// 配置里码表那套方案用的名字（`scheme.name`，默认 wubi）。
/// 不是码表方案的话就用默认的 "wubi"
pub fn wubi_scheme_name() -> String {
    match Config::load().map(|config| config.scheme) {
        Ok(pliers_engine::SchemeConfig::Wubi { name }) => name,
        _ => "wubi".to_string(),
    }
}

/// 老布局残留：拼音词库里还有多少 `scheme = "wubi"` 的行。
/// 读不了（没这个库 / 被跑着的实例独占）就是 None
fn legacy_wubi_rows() -> Option<i64> {
    let dict = Dict::open(&path(), &user_path()).ok()?;
    let stats = dict.stats().ok()?;
    stats
        .schemes
        .iter()
        .find(|(name, _)| name == "wubi")
        .map(|(_, rows)| *rows)
        .filter(|rows| *rows > 0)
}

/// 报一下这个库的体检结果：大小、词条数、来源、用户数据。
/// `Dict::open` 本身就是校验 —— 表缺了、音节表空了都会报错
fn describe_path(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", row("大小", size(path).unwrap_or_default()));
    contents(path, true)?;
    Ok(())
}

/// 打开来数一数里面有什么（下载完 / 构建完用它验一遍），返回每种方案多少行。
///
/// `with_user` = 要不要报用户数据那一行：码表库跟拼音库共用同一个 `user.db`，
/// 同一次输出里报两遍没意义
fn contents(
    path: &Path,
    with_user: bool,
) -> Result<Vec<(String, i64)>, Box<dyn std::error::Error>> {
    let user = user_path();
    let dict = Dict::open(path, &user)?;
    let stats = dict.stats()?;
    println!(
        "{}",
        row(
            "词条",
            format!(
                "{}（单字 {} + 词 {}）",
                stats.words, stats.singles, stats.phrasal
            )
        )
    );
    // 一个库里可以放好几套方案（拼音 / 五笔 / …），分开报
    for (scheme, rows) in &stats.schemes {
        println!("{}", note(format!("{scheme}：{rows} 条")));
    }
    if let Some(source) = dict.meta("source") {
        println!("{}", row("来源", source));
    }
    if let Some(freq) = dict.meta("freq") {
        println!("{}", row("权重", freq));
    }
    if with_user {
        println!(
            "{}",
            user_row(&user, Some((stats.user_words, stats.user_phrases)))
        );
    }
    Ok(stats.schemes)
}

/// 用户数据那一行。词库读不进去（被跑着的实例独占）时至少把文件和大小报出来
fn user_row(user: &Path, counts: Option<(i64, i64)>) -> String {
    let how_big = size(user).unwrap_or_else(|| "还没有，用着会自动建".to_string());
    match counts {
        Some((words, phrases)) => row(
            "用户数据",
            format!(
                "选过 {words} 个词，自己拼的句子 {phrases} 条（{}，{how_big}）",
                user.display()
            ),
        ),
        None => row("用户数据", format!("{}（{how_big}）", user.display())),
    }
}

/// 输入法实际会用的那个**用户数据**文件（认 `PLIERS_USER_DB`）
pub fn user_path() -> PathBuf {
    Config::load()
        .map(|config| config.user_path())
        .unwrap_or_else(|_| pliers_engine::config::default_user_path())
}

/// 输入法实际会用的那个词库文件：配置文件里的 `dict.path`（认 `PLIERS_PINYIN`）。
/// 配置坏了也不至于装不了 —— 那就退回默认路径
pub fn path() -> PathBuf {
    Config::load()
        .map(|config| config.dict_path())
        .unwrap_or_else(|_| pliers_engine::config::default_dict_path())
}

/// 码表库（五笔/郑码/仓颉）文件：`dict.wubi_path`（认 `PLIERS_WUBI`）。
/// 它跟拼音词库是**两个文件** —— 重导拼音不会碰到它
pub fn wubi_path() -> PathBuf {
    Config::load()
        .map(|config| config.wubi_path())
        .unwrap_or_else(|_| pliers_engine::config::default_wubi_path())
}

/// 语料缓存放 `~/.cache/pliers/rime-frost`（重装词库不用再下一遍几十 MB）
pub fn corpus_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into())).join(".cache")
        });
    base.join("pliers").join("rime-frost")
}

/// 找 `pliers-dict`：环境变量 → 跟 pliers 放一起（cargo 编出来就在同一个 target 目录）
/// → PATH
pub fn importer_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PLIERS_DICT_BIN") {
        return Some(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("pliers-dict");
        if sibling.exists() {
            return Some(sibling);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("pliers-dict"))
            .find(|path| path.exists())
    })
}
