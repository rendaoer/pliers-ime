//! `pliers fetch ...` / `pliers update`：从 GitHub Release 把**预构建的字典**拿下来。
//!
//! **字典（dict）是个总概念**，按方案分成几块，一个库一个文件、各有各的资产：
//!
//! | 种类 | 资产 | 落到哪 | 是什么 |
//! | --- | --- | --- | --- |
//! | `pinyin` | `pinyin.db.zst`（27 MB） | `~/.local/share/pliers/pinyin.db` | 拼音词库 + 音节表 |
//! | `english` | `english.db.zst`（约 500 KB） | `~/.local/share/pliers/english.db` | 英文候选词表 |
//!
//! ```text
//! pliers fetch pinyin      # 只更新拼音词库
//! pliers fetch english     # 只更新英文词表
//! pliers fetch all         # 两个预构建的都要
//! pliers update            # 哪个不是最新的就更新哪个（先比 Release 上的 sha256，一样就跳过）
//! ```
//!
//! 码表（五笔/郑码/仓颉）**没有资产**：各家的码不一样、许可也各不相同，得用自己那份
//! `pliers build wubi <码表.txt>` 建到它自己的 `wubi.db` 里（见 docs/dictionary.md）。
//!
//! "是不是最新的"靠 Release 上那个 `<资产>.sha256`（构建时算的压缩包指纹）+ 本地一份小抄
//!（`<文件>.asset-sha256`，装的时候写下来）。比不了（比如 sha256sum 没装、marker 是老版本
//! 装的）就老老实实下载 —— 那是安全的方向。

use std::path::{Path, PathBuf};

use pliers_engine::Config;

use crate::args::{has, kind, value_of};

/// 一个可以从 Release 拿的资产
#[derive(Debug)]
struct Asset {
    /// 命令里怎么写：`pliers fetch pinyin`
    name: &'static str,
    /// 人看的名字
    label: &'static str,
    /// 下载地址（`latest/download/` 永远指向最新那个 Release）
    url: &'static str,
    /// 想换源就用这个环境变量
    url_env: &'static str,
    /// 换源的环境变量的老名字（改过名的还认，见 `env_url`）
    url_env_legacy: Option<&'static str>,
    /// 安装到哪
    dest: fn(&Config) -> PathBuf,
}

/// 两个资产。加第三个（比如码表）就往这儿加一行
const ASSETS: &[Asset] = &[
    Asset {
        name: "pinyin",
        label: "拼音词库（pinyin）",
        url: "https://github.com/rendaoer/pliers-ime/releases/latest/download/pinyin.db.zst",
        url_env: "PLIERS_PINYIN_URL",
        url_env_legacy: Some("PLIERS_DICT_URL"),
        dest: |config| config.dict_path(),
    },
    Asset {
        name: "english",
        label: "英文词表（english）",
        url: "https://github.com/rendaoer/pliers-ime/releases/latest/download/english.db.zst",
        url_env: "PLIERS_ENGLISH_URL",
        url_env_legacy: None,
        dest: |config| config.english_path(),
    },
];

/// `pliers fetch <pinyin|english|all> [--url 地址] [--force]`
///
/// 想拿的那几种（什么都不写跟 `all` 一样）。不认识的名字报错里列出能用的
fn targets(what: &str) -> Result<Vec<&'static Asset>, String> {
    Ok(match what {
        "pinyin" => vec![&ASSETS[0]],
        "english" => vec![&ASSETS[1]],
        // 「整套字典」
        "" | "all" => ASSETS.iter().collect(),
        // `dict` 是**总概念**（一套字典按方案分成几块），不是种类 ——
        // 别再让它变成"整套"的第二种写法
        "dict" => {
            return Err(
                "`dict` 是总概念（一套字典里按方案分 pinyin / english），不是种类 ——\n\
                        \x20     整套下载写 all：pliers fetch all"
                    .to_string(),
            );
        }
        "wubi" => {
            return Err(
                "五笔这类码表**没有**预构建的资产 —— 各家的码不一样（86 / 98 / 新世纪 / 极点…），\n\
                 \x20     许可也各不相同。它有自己的库（wubi.db），用自己的码表建一份就行：\n\
                 \x20     pliers build wubi <码表.txt>        # 每行 `词<TAB>码[<TAB>权重]`\n\
                 \x20     （拼音词库不受影响：那是另一个文件 pinyin.db）"
                    .to_string(),
            );
        }
        other => {
            return Err(format!(
                "不认识的种类 {other:?}（fetch 收这些）：\n\
                 \x20     pinyin   拼音词库（pinyin.db，27 MB）\n\
                 \x20     english  英文词表（english.db，约 500 KB）\n\
                 \x20     all      两个预构建的都要（也可以不写）\n\
                 \x20 想「哪个不是最新就更新哪个」用：pliers update\n\
                 \x20 五笔码表没有资产可下，用 pliers build wubi <码表.txt> 自己建"
            ));
        }
    })
}

/// `pliers fetch ...`
pub fn command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let url = value_of(args, "--url");
    let force = has(args, "--force");
    let assets = targets(kind(args))?;
    if url.is_some() && assets.len() > 1 {
        return Err(
            "`--url` 一次只能配一个种类（两个资产的地址本来就不一样）：\n\
                    \x20     pliers fetch pinyin --url 地址\n\
                    \x20     pliers fetch english --url 地址"
                .into(),
        );
    }
    // `--url` 只对单个资产有意义（两个资产地址不一样）
    let config = Config::load()?;
    let mut failed = false;
    for asset in assets {
        let url = url.clone().or_else(|| env_url(asset));
        if let Err(e) = fetch_one(asset, url.as_deref(), &config, force) {
            eprintln!("pliers: {}没装上：{e}", asset.label);
            failed = true;
        }
    }
    if failed {
        return Err("有资产没装上（上面写了原因）".into());
    }
    println!();
    println!("搞定 —— 起着的实例 `pliers reload` 就会用上新的（数据是启动时读的）");
    Ok(())
}

/// 装某一个种类（`pliers --init` 这类调用方用；`url` 不给就按环境变量 / 内置默认）
pub fn install(
    name: &str,
    url: Option<&str>,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let asset = ASSETS
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| format!("没有这个种类：{name}"))?;
    let url = url.map(str::to_string).or_else(|| env_url(asset));
    let config = Config::load()?;
    fetch_one(asset, url.as_deref(), &config, force)
}

/// `pliers update [--force]`：两个都更新到最新，已经一样的不重复下载
pub fn update(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let force = has(args, "--force");
    let config = Config::load()?;
    println!("看看 Release 上是什么版本…");
    let mut changed = 0;
    for asset in ASSETS {
        let dest = (asset.dest)(&config);
        let remote = match remote_sha(asset) {
            Some(sha) => sha,
            None => {
                println!(
                    "  {}：拿不到 Release 上的指纹（离线？），直接下载试试",
                    asset.label
                );
                fetch_one(asset, env_url(asset).as_deref(), &config, true)?;
                changed += 1;
                continue;
            }
        };
        if !force && is_current(&dest, &remote) {
            println!("  {}：已经是最新的（{}）", asset.label, short(&remote));
            continue;
        }
        fetch_one(asset, env_url(asset).as_deref(), &config, true)?;
        changed += 1;
    }
    println!();
    if changed == 0 {
        println!("都是最新的，没动它们");
    } else {
        println!("更新了 {changed} 个 —— 起着的实例 `pliers reload` 就会用上");
    }
    Ok(())
}

/// 这个资产的下载地址：环境变量优先（`PLIERS_PINYIN_URL` / `PLIERS_ENGLISH_URL`），
/// 否则内置默认（`latest/download/` 永远指向最新 Release 里的同名资产）
fn env_url(asset: &Asset) -> Option<String> {
    let from_env = std::env::var(asset.url_env).ok().or_else(|| {
        asset
            .url_env_legacy
            .and_then(|name| std::env::var(name).ok())
    });
    from_env.or_else(|| Some(asset.url.to_string()))
}

/// 下载一个资产。`url` 不给就用内置那个；`force = false` 时本地已经有就不动
fn fetch_one(
    asset: &Asset,
    url: Option<&str>,
    config: &Config,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = url.unwrap_or(asset.url);
    let dest = (asset.dest)(config);
    if dest.exists() && !force {
        println!("{}：{}（已有，没动它）", asset.label, dest.display());
        println!("  想覆盖加 --force，或者用 pliers update");
        return Ok(());
    }
    println!("{}：下载 {url}", asset.label);
    println!("  目标：{}", dest.display());
    let sha = remote_sha_of(url);
    pliers_engine::fetch::install(url, &dest).map_err(|e| {
        format!(
            "{e}\n\n\
             下载不到也没关系：\n\
             \x20 拼音词库可以自己构建：pliers build pinyin（要 pliers-dict）\n\
             \x20 英文词表没装的话输入法会用二进制里那份兜底\n\
             或者手动下载后放到：{}",
            dest.display()
        )
    })?;
    if let Some(sha) = &sha {
        write_marker(&dest, sha);
    }
    println!("  {} 装好了（{}）", asset.label, describe(&dest));
    Ok(())
}

/// 本地这份是不是就是 Release 上那个版本（比 sha256 小抄）
fn is_current(dest: &Path, remote: &str) -> bool {
    matches!(read_marker(dest), Some(local) if local.trim() == remote.trim())
}

/// 下载地址旁边的 `.sha256`（构建时算的压缩包指纹）。
/// 拿不到就算了 —— 调用方会当成"不知道，直接下载"
fn remote_sha(asset: &Asset) -> Option<String> {
    remote_sha_of(&env_url(asset)?)
}

fn remote_sha_of(url: &str) -> Option<String> {
    let tmp = std::env::temp_dir().join(format!("pliers-sha-{}", std::process::id()));
    pliers_engine::fetch::download(&format!("{url}.sha256"), &tmp).ok()?;
    let text = std::fs::read_to_string(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    parse_sha(&text)
}

/// `sha256sum` 输出的第一列（`<hash>  <文件名>`；只有哈希也认）
fn parse_sha(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    let sha = line.split_whitespace().next()?.trim();
    (!sha.is_empty()).then(|| sha.to_string())
}

/// 装完之后记一笔"我这份是从哪个资产来的"（下次 `pliers update` 靠它跳过白下载）
fn marker_path(dest: &Path) -> PathBuf {
    PathBuf::from(format!("{}.asset-sha256", dest.display()))
}

fn write_marker(dest: &Path, sha: &str) {
    let _ = std::fs::write(marker_path(dest), format!("{sha}\n"));
}

fn read_marker(dest: &Path) -> Option<String> {
    std::fs::read_to_string(marker_path(dest)).ok()
}

/// 文件多大（人看的）
fn describe(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() >= 1024 * 1024 => format!("{} MB", meta.len() / 1024 / 1024),
        Ok(meta) => format!("{} KB", meta.len().max(1024) / 1024),
        Err(_) => "？".to_string(),
    }
}

/// 指纹只显示前 12 位（够认人了）
fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 认得出_sha256_文件的两种写法() {
        // sha256sum 的输出：`<hash>  <文件名>`
        assert_eq!(
            parse_sha(
                "0bd9c79a8a617a69986076979ca33407898486a8aeca077953bf70fa056da1cf  pinyin.db.zst\n"
            ),
            Some("0bd9c79a8a617a69986076979ca33407898486a8aeca077953bf70fa056da1cf".to_string())
        );
        // 只有哈希也认
        assert_eq!(parse_sha("abc123\n"), Some("abc123".to_string()));
        assert_eq!(parse_sha(""), None);
    }

    #[test]
    fn 小抄一样就算是最新的() {
        let dest = std::env::temp_dir().join(format!("pliers-marker-{}", std::process::id()));
        let _ = std::fs::remove_file(marker_path(&dest));
        assert!(!is_current(&dest, "abc"), "没小抄 = 不知道，得当旧的处理");
        write_marker(&dest, "abc");
        assert!(is_current(&dest, "abc"));
        assert!(!is_current(&dest, "def"), "指纹不一样就是要更新");
        let _ = std::fs::remove_file(marker_path(&dest));
    }

    #[test]
    fn 字典的几种各对应哪个资产() {
        assert_eq!(ASSETS.len(), 2);
        assert_eq!(ASSETS[0].name, "pinyin");
        assert_eq!(ASSETS[1].name, "english");
        // 单个种类
        assert_eq!(targets("pinyin").unwrap().len(), 1);
        assert_eq!(targets("english").unwrap().len(), 1);
        // 整套字典：all / 不写都一样
        for what in ["", "all"] {
            assert_eq!(targets(what).unwrap().len(), 2, "{what:?}");
        }
        // `dict` 是总概念，不是"整套"的另一种写法 —— 要把它指回 all
        let message = targets("dict").unwrap_err();
        assert!(message.contains("pliers fetch all"), "{message}");
        // 码表：没有资产可下，要说清楚该怎么办（自己的码表 → 自己的库）
        let message = targets("wubi").unwrap_err();
        assert!(message.contains("pliers build wubi"), "{message}");
        assert!(message.contains("wubi.db"), "{message}");
        // 不认识的名字要把能用的列出来
        let message = targets("pinying").unwrap_err();
        assert!(message.contains("pinyin"), "{message}");
        assert!(message.contains("english"), "{message}");
        for asset in ASSETS {
            assert!(asset.url.starts_with("https://"), "{}", asset.url);
            assert!(asset.url.ends_with(".zst"), "{}", asset.url);
        }
    }
}
