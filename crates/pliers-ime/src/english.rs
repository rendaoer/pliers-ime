//! 字典的 `english` 那种：英文候选词表，在自己的库 `~/.local/share/pliers/english.db` 里
//!（表就一张：`english(word, weight)`，按权重从高到低就是词频顺序）。
//!
//! ```text
//! pliers status english          # 现在用的是哪份、多少个词
//! pliers fetch english           # 从 Release 更新到最新那份
//! pliers build english [词表]     # 自己构建（不给词表就用二进制里那份兜底）
//! ```
//!
//! 运行时**启动时一次性读进内存**，每次按键还是内存里扫（实测 57 µs）——
//! 用 SQLite 当"存储和发布格式"，不等于把每次按键的查询交给 SQL。

use std::path::PathBuf;

use pliers_engine::Config;
use pliers_engine::english::{BUILTIN, parse_list, read_db};

use crate::{note, row, size};

/// `pliers status english`：这块字典现在用的是哪份、多大、多少个词
pub fn report() -> Result<(), Box<dyn std::error::Error>> {
    let path = path();
    let how_big = size(&path).unwrap_or_else(|| "还没装".to_string());
    println!(
        "{}",
        row("英文词表", format!("{}（{how_big}）", path.display()))
    );
    if !path.exists() {
        println!(
            "{}",
            note(format!(
                "用二进制里那份兜底顶着（{} 个词，英文候选照常能用）",
                parse_list(BUILTIN).len()
            ))
        );
        println!(
            "{}",
            note("装成文件：pliers --init（离线也行）或 pliers fetch english")
        );
        return Ok(());
    }
    match read_db(&path) {
        Ok(words) => println!("{}", row("词数", words.len())),
        Err(e) => println!("{}", row("词数", format!("读不了：{e}"))),
    }
    println!(
        "{}",
        row(
            "来源",
            "外部那个库（pliers fetch english 更新；pliers build english 自己构建）"
        )
    );
    Ok(())
}

/// `pliers build english [词表.txt|目录]`：自己构建一份。
///
/// 给了词表就交给导入工具（`pliers-dict --english`，格式解析跟引擎共用一份实现）；
/// 不给就用二进制里那份兜底重建 —— 那条路不需要 pliers-dict
pub fn build(source: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let dest = path();
    match source {
        Some(source) => {
            let importer = crate::dict::importer_binary().ok_or(
                "找不到 pliers-dict（从自己的词表构建要它）\n\
                 装一个：cargo install pliers-dict\n\
                 在仓库里的话：cargo build --release -p pliers-dict",
            )?;
            println!(
                "构建：{} --english {source} --out {}",
                importer.display(),
                dest.display()
            );
            let status = std::process::Command::new(&importer)
                .arg("--english")
                .arg(source)
                .arg("--out")
                .arg(&dest)
                .status()?;
            if !status.success() {
                return Err(format!("构建失败（退出码 {:?}）", status.code()).into());
            }
        }
        None => {
            let words = parse_list(BUILTIN);
            println!("用二进制里那份兜底重建（{} 个词）", words.len());
            println!("  目标：{}", dest.display());
            println!("  （想要 Release 上那份两万多词的：pliers fetch english）");
            pliers_engine::english::write_db(&dest, &words)?;
        }
    }
    println!();
    report()?;
    println!();
    println!("搞定 —— 起着的实例 `pliers reload` 就会用上新的（词表是启动时读进内存的）");
    Ok(())
}

/// `pliers --init` 顺带做的事：装一份 `english.db`。
///
/// 先试着从 Release 下载（跟词库一趟装齐），下载不到就写二进制里那份兜底 ——
/// 所以离线也能装出这个文件（写出来之后它才是个**看得见、能改、能单独更新**的库）
pub fn install_if_missing(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = path();
    if path.exists() && !force {
        println!(
            "英文词表：{}（已有，没动它；更新用 pliers fetch english）",
            path.display()
        );
        return Ok(());
    }
    if let Err(e) = crate::fetch::install("english", None, true) {
        // 离线 / 资产还没发版：用二进制里那份兜底，装出同样结构的一个库。
        // 只报错的第一行 —— 后面那几行是"自己构建 / 手动放"的长提示，这里不用重复
        let why = e.to_string();
        println!(
            "  从 Release 拿不到：{}",
            why.lines().next().unwrap_or("（没说是为什么）")
        );
        println!("  先把二进制里那份兜底写出来 —— 一样能用，想更新再 pliers fetch english");
        let words = parse_list(BUILTIN);
        pliers_engine::english::write_db(&path, &words)?;
        println!("英文词表：{}（兜底 {} 个词）", path.display(), words.len());
    }
    Ok(())
}

/// 输入法实际会用的那个词表库（`[english] path`，认 `PLIERS_ENGLISH`）。
/// 配置坏了也不至于用不了 —— 那就退回默认路径
pub fn path() -> PathBuf {
    Config::load()
        .map(|config| config.english_path())
        .unwrap_or_else(|_| pliers_engine::config::default_english_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 兜底那份是满的() {
        assert!(parse_list(BUILTIN).len() > 20_000);
    }
}
