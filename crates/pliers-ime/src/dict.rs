//! `pliers --init` 和 `pliers dict ...`：装词库。
//!
//! 词库是几百 MB 的派生物，不进仓库（GitHub 也不让放这么大的文件），
//! 但也不该让你自己去哪儿找一份词表、再守着跑一遍导入：
//!
//! * `pliers --init`      写配置 + 下载预构建的词库（新机器一条命令能用）
//! * `pliers dict fetch`  只装词库（可以换源、重装）
//! * `pliers dict build`  自己下语料、自己构建（Release 上还没有东西时走这条）
//! * `pliers dict status` 现在用的是哪个库、多少词、来源是什么
//!
//! 语料用的是[白霜拼音 rime-frost](https://github.com/gaboolic/rime-frost)（GPL-3.0）：
//! 字频词频是拿 7.4 亿字语料重新统计的，多音字就是**一行一个读音**（各自带权重，
//! 不用猜），格式还正好是 `词<TAB>拼音<TAB>权重` —— 跟 `word.code` 一模一样。

use std::path::{Path, PathBuf};

use pliers_engine::{Config, Dict};

/// 预构建词库的地址：挂在仓库的 Release 上。
/// `releases/latest/download/` 永远指向最新 Release 里的同名资产，所以不用跟着版本号改。
/// 想换源：`PLIERS_DICT_URL=... pliers dict fetch`（公司镜像、自己搭的服务器都行）
pub const DEFAULT_URL: &str =
    "https://github.com/rendaoer/pliers-ime/releases/latest/download/dict.db.zst";

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
    let force = args.iter().any(|arg| arg == "--force");
    let build = args.iter().any(|arg| arg == "--build");
    let url = value_of(args, "--url");

    // ---- 配置 ----
    let config = pliers_engine::config::config_path();
    if config.exists() && !force {
        println!("配置：{}（已有，没动它）", config.display());
    } else {
        crate::init_config(force)?;
    }

    // ---- 词库 ----
    let dest = target_path();
    if dest.exists() && !force {
        println!("词库：{}（已有，没动它）", dest.display());
        describe_path(&dest)?;
        println!();
        println!("想重装：pliers dict fetch --force（或 pliers dict build）");
        return Ok(());
    }
    if build {
        build_dict(&dest, false)
    } else {
        fetch_dict(&dest, url.as_deref(), true)
    }
}

/// `pliers dict fetch|build|status|path`
pub fn command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        Some("fetch") => {
            let force = args.iter().any(|arg| arg == "--force");
            fetch_dict(&target_path(), value_of(args, "--url").as_deref(), force)
        }
        Some("build") => build_dict(&target_path(), args.iter().any(|arg| arg == "--refresh")),
        Some("path") => {
            println!("{}", target_path().display());
            Ok(())
        }
        // 不带参数 = 看现状，跟 `path` 相反：这个给人看，那个给脚本用
        Some("status") | None => status(),
        Some(other) => Err(format!(
            "不认识的：pliers dict {other}\n\
             能用的是：fetch（下载预构建的）/ build（自己下语料构建）/ status / path"
        )
        .into()),
    }
}

/// 下载预构建的词库装到 `dest`
fn fetch_dict(
    dest: &Path,
    url: Option<&str>,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = url
        .map(str::to_string)
        .or_else(|| std::env::var("PLIERS_DICT_URL").ok())
        .unwrap_or_else(|| DEFAULT_URL.to_string());

    if dest.exists() && !force {
        println!("词库已经有了：{}", dest.display());
        println!("（想重装加 --force；想先看看里面是什么用 pliers dict status）");
        return Ok(());
    }
    println!("下载词库：{url}");
    println!("  目标：{}", dest.display());
    pliers_engine::fetch::install(&url, dest).map_err(|e| {
        format!(
            "{e}\n\n\
             下载不到也没关系，自己构建一份就行：pliers dict build\n\
             或者手动下载后放到：{}",
            dest.display()
        )
    })?;
    println!();
    describe_path(dest)?;
    println!();
    println!("搞定 —— 重启输入法就生效（词库是启动时打开的）");
    Ok(())
}

/// 下载语料 + 调 `pliers-dict` 构建一份词库
fn build_dict(dest: &Path, refresh: bool) -> Result<(), Box<dyn std::error::Error>> {
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
         先 `cargo build --release -p pliers-dict`，或者直接用它：\n\
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
        .arg(dest)
        .status()?;
    if !status.success() {
        return Err(format!("构建失败（退出码 {:?}）", status.code()).into());
    }
    println!();
    describe_path(dest)?;
    println!();
    println!("搞定 —— 重启输入法就生效");
    Ok(())
}

/// `pliers dict status`：现在用的是哪个库、有没有、多大、里面是什么
fn status() -> Result<(), Box<dyn std::error::Error>> {
    let dest = target_path();
    println!("词库    {}", dest.display());
    if !dest.exists() {
        println!("        还没有 —— pliers --init 装一份（或 pliers dict build 自己构建）");
        return Ok(());
    }
    println!(
        "大小    {} MB",
        std::fs::metadata(&dest)?.len() / 1024 / 1024
    );
    if let Err(e) = describe_contents(&dest) {
        // 输入法开着的时候词库被它独占（turso 是独占锁），读不进去很正常 ——
        // 这不是词库坏了，所以只说清楚原因，不当成错误
        println!("内容    读不了：{e}");
        println!(
            "        输入法正开着的话词库被它独占着，退掉再看；它自己报的现状用 pliers status"
        );
    }
    Ok(())
}

/// 报一下这个库的体检结果：大小、词条数、来源、用户数据。
/// `Dict::open` 本身就是校验 —— 表缺了、音节表空了都会报错
fn describe_path(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "大小    {} MB",
        std::fs::metadata(path)?.len() / 1024 / 1024
    );
    describe_contents(path)
}

/// 打开来数一数里面有什么（下载完 / 构建完用它验一遍）
fn describe_contents(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let dict = Dict::open(path)?;
    let stats = dict.stats()?;
    println!(
        "词条    {}（单字 {} + 词 {}）",
        stats.words, stats.singles, stats.phrasal
    );
    for (scheme, rows) in &stats.schemes {
        println!("        {scheme}：{rows} 条");
    }
    if let Some(source) = dict.meta("source") {
        println!("来源    {source}");
    }
    if let Some(freq) = dict.meta("freq") {
        println!("权重    {freq}");
    }
    println!(
        "用户    选过 {} 个词，自己拼的句子 {} 条",
        stats.user_words, stats.user_phrases
    );
    Ok(())
}

/// 输入法实际会用的那个词库文件：配置文件里的 `dict.path`（认 `PLIERS_DICT`）。
/// 配置坏了也不至于装不了 —— 那就退回默认路径
fn target_path() -> PathBuf {
    Config::load()
        .map(|config| config.dict_path())
        .unwrap_or_else(|_| pliers_engine::config::default_dict_path())
}

/// 语料缓存放 `~/.cache/pliers/rime-frost`（重装词库不用再下一遍几十 MB）
fn corpus_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into())).join(".cache")
        });
    base.join("pliers").join("rime-frost")
}

/// 找 `pliers-dict`：环境变量 → 跟 pliers 放一起（cargo 编出来就在同一个 target 目录）
/// → PATH
fn importer_binary() -> Option<PathBuf> {
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

/// `--url <值>` / `--url=<值>` 都认
fn value_of(args: &[String], flag: &str) -> Option<String> {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if arg == flag {
            return rest.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(value.to_string());
        }
    }
    None
}
