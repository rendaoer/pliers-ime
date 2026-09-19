//! 下载 + 解压：只给"装词库"用（`pliers --init` / `pliers build pinyin`）。
//!
//! 故意不引 HTTP 库：命令行工具本来就是靠 `curl` 下载的（文档里让人手动下的命令也是它），
//! 解压 `.zst` 靠 `zstd`。少两个依赖，而且现成的好处一堆 ——
//! 进度条、断点续传、代理（`https_proxy` 这些环境变量 curl 自己认）。
//!
//! 下载和安装是分开的两件事：下到 `<目标>.part` 再改名过去，
//! 中途失败/断网不会把已经装好的词库弄坏。

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 把 `url` 下到 `dest`（父目录自动建）。
///
/// `file://` 和普通本地路径也认 —— 离线安装、自己构建完从别处拷过来、跑测试都用得上
pub fn download(url: &str, dest: &Path) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了目录 {}：{e}", dir.display()))?;
    }
    if let Some(local) = local_path(url) {
        std::fs::copy(&local, dest)
            .map_err(|e| format!("拷不了 {} → {}：{e}", local.display(), dest.display()))?;
        return Ok(());
    }
    let mut curl = Command::new("curl");
    // 连接超时是必须的：国内直连 raw.githubusercontent 常常是"包被丢掉"而不是
    // 立刻报错，不给超时就会一直挂着，连换镜像的机会都没有
    curl.args(["-L", "--fail", "--connect-timeout", "15"]);
    // 终端里给个进度条（几十 MB 呢），被脚本调用时安静点、只要错误信息
    if std::io::stderr().is_terminal() {
        curl.arg("-#");
    } else {
        curl.arg("-sS");
    }
    curl.arg("-o").arg(dest).arg(url);
    if try_run(&mut curl, "curl 下载")? {
        return Ok(());
    }
    let mut wget = Command::new("wget");
    wget.args(["--timeout=15", "-O"]).arg(dest).arg(url);
    if try_run(&mut wget, "wget 下载")? {
        return Ok(());
    }
    Err("下载失败：curl 和 wget 都没装（Arch: sudo pacman -S curl）".into())
}

/// 下载 `url` 并装到 `dest`。`.zst` 会自动解压。
///
/// 下来的东西先落在 `<dest>.part`，解压到 `<dest>.unpacked`，最后一步才改名 ——
/// 所以不会出现"装到一半的坏词库"。改名是同一个目录内的操作，是原子的
pub fn install(url: &str, dest: &Path) -> Result<(), String> {
    let part = suffix(dest, ".part");
    download(url, &part)?;
    if url.ends_with(".zst") {
        let unpacked = suffix(dest, ".unpacked");
        decompress_zstd(&part, &unpacked)?;
        let _ = std::fs::remove_file(&part);
        std::fs::rename(&unpacked, dest)
            .map_err(|e| format!("装不了 {} → {}：{e}", unpacked.display(), dest.display()))?;
    } else {
        std::fs::rename(&part, dest)
            .map_err(|e| format!("装不了 {} → {}：{e}", part.display(), dest.display()))?;
    }
    Ok(())
}

/// 解压 `.zst`。`zstd` 不在就报错，并给出两条路（装包 / 手动解压）
pub fn decompress_zstd(src: &Path, dest: &Path) -> Result<(), String> {
    let mut zstd = Command::new("zstd");
    zstd.args(["-d", "-f", "-q", "-o"]).arg(dest).arg(src);
    if try_run(&mut zstd, "zstd 解压")? {
        return Ok(());
    }
    Err(format!(
        "解压失败：没装 zstd（Arch: sudo pacman -S zstd）\n\
         也可以手动解压：zstd -d {} -o {}",
        src.display(),
        dest.display()
    ))
}

/// 跑一条命令。`Ok(true)` 成功，`Ok(false)` 是这个命令不存在（让调用方试下一个工具），
/// `Err` 是命令在、但跑失败了（这时不该再试别的工具）
fn try_run(command: &mut Command, what: &str) -> Result<bool, String> {
    match command.status() {
        Ok(status) if status.success() => Ok(true),
        Ok(status) => Err(format!(
            "{what}失败（退出码 {}）",
            status.code().unwrap_or(-1)
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{what}起不来：{e}")),
    }
}

/// `file://...` 或者干脆一个本地路径（没有 `://`）
fn local_path(url: &str) -> Option<PathBuf> {
    if let Some(rest) = url.strip_prefix("file://") {
        return Some(PathBuf::from(rest));
    }
    (!url.contains("://")).then(|| PathBuf::from(url))
}

fn suffix(path: &Path, extra: &str) -> PathBuf {
    PathBuf::from(format!("{}{extra}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 本地路径两种写法都认() {
        assert_eq!(
            local_path("file:///tmp/a.db"),
            Some(PathBuf::from("/tmp/a.db"))
        );
        assert_eq!(local_path("./dict.db"), Some(PathBuf::from("./dict.db")));
        assert_eq!(local_path("https://example.com/a.db"), None);
    }

    #[test]
    fn 装本地文件是原子的() {
        let dir = std::env::temp_dir().join("pliers-fetch-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("来源.db");
        std::fs::write(&src, b"hello").unwrap();
        let dest = dir.join("装好.db");

        install(&format!("file://{}", src.display()), &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        // 中间文件不留下
        assert!(!suffix(&dest, ".part").exists());
        assert!(!suffix(&dest, ".unpacked").exists());

        // 再装一次（覆盖）也不炸
        std::fs::write(&src, b"world").unwrap();
        install(&format!("file://{}", src.display()), &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"world");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 下不到就报错且不碰目标文件() {
        let dir = std::env::temp_dir().join("pliers-fetch-test-missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("装好.db");
        std::fs::write(&dest, b"old").unwrap();

        let err = install("file:///nonexistent/根本没有这个文件.db", &dest).unwrap_err();
        assert!(err.contains("拷不了"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"old", "旧文件不能被弄坏");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
