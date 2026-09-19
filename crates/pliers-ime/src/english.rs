//! `pliers english ...`：英文候选词表 —— 跟中文词库分开的、可以**单独更新**的那个库。
//!
//! 词表放在自己的 SQLite 文件 `~/.local/share/pliers/english.db` 里（表就一张：
//! `english(word, weight)`，按权重从高到低就是词频顺序）。运行时**启动时一次性读进内存**，
//! 每次按键还是内存里扫（实测 57 µs）—— 用 SQLite 当"存储和发布格式"，
//! 不等于把每次按键的查询交给 SQL。
//!
//! ```text
//! pliers --init            # 装一份（先试 Release；下载不到就写二进制里那份兜底）
//! pliers fetch english     # 从 GitHub Release 更新到最新那份
//! pliers english status    # 现在用的是哪个库、多少个词
//! ```
//!
//! 想自己构建（换一份词表、离线、自己加词）：
//!
//! ```text
//! pliers-dict --english 词表.txt --out ~/.local/share/pliers/english.db
//! ```

use std::path::PathBuf;

use pliers_engine::Config;
use pliers_engine::english::{BUILTIN, parse_list, read_db};

/// 预构建词表跟中文词库挂在**同一个 release** 上（`latest` 永远指向最新那个）。
/// 想换成别的源：`--url` 或 `PLIERS_ENGLISH_URL`
const DEFAULT_URL: &str =
    "https://github.com/rendaoer/pliers-ime/releases/latest/download/english.db.zst";

/// `pliers english <子命令>`
pub fn command(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.first().map(String::as_str) {
        // 下载挪到顶层了：`pliers fetch english`
        Some("fetch") => Err(
            "`pliers english fetch` 现在叫 `pliers fetch english`（想两个都更新：pliers update）"
                .into(),
        ),
        Some("status") | None => status(),
        Some("path") => {
            println!("{}", target_path().display());
            Ok(())
        }
        Some(other) => Err(format!(
            "不认识的子命令 {other:?}（能用的：status / path；下载在顶层：pliers fetch english）"
        )
        .into()),
    }
}

/// `pliers --init` 顺带做的事：装一份 `english.db`。
///
/// 先试着从 Release 下载（跟词库一趟装齐），下载不到就写二进制里那份兜底 ——
/// 所以离线也能装出这个文件（写出来之后它才是个**看得见、能改、能单独更新**的库）
pub fn install_if_missing(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = target_path();
    if path.exists() && !force {
        println!("英文词表：{}（已有，没动它）", path.display());
        println!("  想更新：pliers fetch english");
        return Ok(());
    }
    fetch_command(&[], false)
}

/// 从 Release 下载（`.zst` 自动解压，`file://` 和本地路径也认，跟词库一个规矩）。
/// 下载失败时（离线、资产还没发版）退回"用二进制里那份兜底写一个"
fn fetch_command(args: &[String], quiet: bool) -> Result<(), Box<dyn std::error::Error>> {
    let path = target_path();
    let url = value_of(args, "--url")
        .or_else(|| crate::fetch::url_for("english"))
        .unwrap_or_else(|| DEFAULT_URL.to_string());
    let force = args.iter().any(|arg| arg == "--force");

    if path.exists() && !force {
        println!("词表已经有了：{}", path.display());
        println!("（想覆盖加 --force；想先看看里面是什么用 pliers english status）");
        return Ok(());
    }
    println!("下载英文词表：{url}");
    println!("  目标：{}", path.display());
    match pliers_engine::fetch::install(&url, &path) {
        Ok(()) => {}
        Err(e) => {
            // 离线 / 资产还没发版：退回二进制里那份兜底，装出同样结构的一个库
            println!("  下载不到（{e}）");
            println!("  先把二进制里那份兜底写出来 —— 一样能用，想更新再 pliers fetch english");
            let words = parse_list(BUILTIN);
            pliers_engine::english::write_db(&path, &words)?;
        }
    }
    println!();
    status()?;
    if !quiet {
        println!();
        println!("搞定 —— 起着的实例要 `pliers reload` 才会用上新词表（它是启动时读进内存的）");
    }
    Ok(())
}

/// `pliers english status`：现在用的是哪个库、多大、多少个词
fn status() -> Result<(), Box<dyn std::error::Error>> {
    let path = target_path();
    println!("词表       {}", path.display());
    if path.exists() {
        println!(
            "大小       {} KB",
            std::fs::metadata(&path)?.len().max(1024) / 1024
        );
        match read_db(&path) {
            Ok(words) => println!("词数       {}", words.len()),
            Err(e) => println!("词数       读不了：{e}"),
        }
        println!("来源       外部那个库（pliers fetch english 更新它）");
    } else {
        println!(
            "           还没装 —— 现在用二进制里那份兜底（{} 个词）",
            parse_list(BUILTIN).len()
        );
        println!("           装成文件：pliers --init（离线也行）或 pliers fetch english");
    }
    Ok(())
}

/// 输入法实际会用的那个词表库（`[english] path`，认 `PLIERS_ENGLISH`）。
/// 配置坏了也不至于用不了 —— 那就退回默认路径
pub(crate) fn target_path() -> PathBuf {
    Config::load()
        .map(|config| config.english_path())
        .unwrap_or_else(|_| pliers_engine::config::default_english_path())
}

/// `--url X` / `--url=X` 都能写
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 兜底那份是满的() {
        assert!(parse_list(BUILTIN).len() > 20_000);
    }
}
