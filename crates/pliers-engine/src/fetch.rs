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
    fetch(url, dest, false)
}

/// 跟 [`download`] 一样，但**不画进度条**。
///
/// 给"顺手拿一下"的小文件用（比如资产旁边那个 `.sha256`，一百来个字节）：
/// 那种文件也画一条进度条的话，终端上看起来像"下个词库却出现两条进度"，
/// 甚至以为下了两遍 —— 其实第一条是指纹、第二条才是词库本体
pub fn download_quiet(url: &str, dest: &Path) -> Result<(), String> {
    fetch(url, dest, true)
}

fn fetch(url: &str, dest: &Path, quiet: bool) -> Result<(), String> {
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建不了目录 {}：{e}", dir.display()))?;
    }
    if let Some(local) = local_path(url) {
        std::fs::copy(&local, dest)
            .map_err(|e| format!("拷不了 {} → {}：{e}", local.display(), dest.display()))?;
        return Ok(());
    }
    // 上次断在半截的 `.part` 还能接着下（GitHub 的 release 支持 Range）。
    // 先试续传；服务器不认 Range 的话，把半截删掉从头来一次
    let partial = dest.metadata().map(|meta| meta.len() > 0).unwrap_or(false);
    let mut last_error = String::new();
    let attempts: &[bool] = if partial { &[true, false] } else { &[false] };
    for &resume in attempts {
        if !resume && partial && dest.exists() {
            let _ = std::fs::remove_file(dest);
        }
        match try_curl(url, dest, quiet, resume) {
            Ok(true) => return Ok(()),
            // curl 不在：别在这儿报错，下面还有 wget
            Ok(false) => break,
            Err(e) => last_error = e,
        }
    }

    let mut wget = Command::new("wget");
    if quiet {
        wget.arg("-nv"); // 安静：只留错误
    }
    // wget 的续传是 `-c`（配合 `-O` 用），半截文件在就接着下
    if dest.metadata().map(|meta| meta.len() > 0).unwrap_or(false) {
        wget.arg("-c");
    }
    wget.args(["--timeout=15", "-O"]).arg(dest).arg(url);
    match try_run(&mut wget, "wget 下载")? {
        true => Ok(()),
        false if last_error.is_empty() => {
            Err("下载失败：curl 和 wget 都没装（Arch: sudo pacman -S curl）".into())
        }
        false => Err(last_error),
    }
}

/// 用 curl 下一遍。`resume` 时带 `-C -`（断点续传）。
/// `Ok(false)` = curl 没装，交给 wget
fn try_curl(url: &str, dest: &Path, quiet: bool, resume: bool) -> Result<bool, String> {
    let mut curl = Command::new("curl");
    // 连接超时是必须的：国内直连 raw.githubusercontent 常常是"包被丢掉"而不是
    // 立刻报错，不给超时就会一直挂着，连换镜像的机会都没有。
    // --retry 管的是"连上了但中途断了"，几十 MB 的文件最怕下到 90% 断掉。
    // --retry-max-time 别让一个坏镜像把人拖在这儿（有的会回 502 + 很长的 Retry-After）；
    // 速度低于 1 KB/s 持续 60 秒也当它不行，换下一个源
    curl.args([
        "-L",
        "--fail",
        "--connect-timeout",
        "15",
        "--retry",
        "3",
        "--retry-delay",
        "2",
        "--retry-max-time",
        "60",
        "--retry-connrefused",
        "--speed-time",
        "60",
        "--speed-limit",
        "1024",
    ]);
    // 终端里给个进度条（几十 MB 呢），被脚本调用或者 quiet 时安静点、只要错误信息
    if std::io::stderr().is_terminal() && !quiet {
        curl.arg("-#");
    } else {
        curl.arg("-sS");
    }
    if resume {
        curl.args(["-C", "-"]);
    }
    curl.arg("-o").arg(dest).arg(url);
    try_run(&mut curl, "curl 下载")
}

/// 下载 `url` 并装到 `dest`。`.zst` 会自动解压。
///
/// 下来的东西先落在 `<dest>.part`，解压到 `<dest>.unpacked`，最后一步才改名 ——
/// 所以不会出现"装到一半的坏词库"。改名是同一个目录内的操作，是原子的
pub fn install(url: &str, dest: &Path) -> Result<(), String> {
    install_checked(url, dest, None)
}

/// 跟 [`install`] 一样，但下完先拿 `sha256` 校验压缩包。
///
/// 校验尤其重要：走镜像时文件是第三方转发的，Release 上那个 `.sha256` 是唯一的
/// 防篡改/防下坏的凭据。没装 `sha256sum`/`shasum` 就跳过（说一声），不因为
/// 少了校验工具就装不了
pub fn install_checked(url: &str, dest: &Path, sha256: Option<&str>) -> Result<(), String> {
    let part = suffix(dest, ".part");
    download(url, &part)?;
    if let Some(expected) = sha256.map(str::trim).filter(|sha| !sha.is_empty()) {
        match sha256_of(&part) {
            Some(actual) if actual.eq_ignore_ascii_case(expected) => {}
            Some(actual) => {
                let _ = std::fs::remove_file(&part);
                let short = |sha: &str| sha.chars().take(16).collect::<String>();
                return Err(format!(
                    "校验不过：下载到的指纹是 {}，Release 上是 {}\n\
                     可能是下坏了，也可能是镜像换了包 —— 再试一次，或者用 --url 换个源",
                    short(&actual),
                    short(expected)
                ));
            }
            None => eprintln!("pliers: 没装 sha256sum / shasum，跳过下载校验"),
        }
    }
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

/// 算文件的 sha256（用系统的 `sha256sum` / `shasum`；都没有就返回 None）
fn sha256_of(path: &Path) -> Option<String> {
    for (program, args) in [
        ("sha256sum", Vec::<&str>::new()),
        ("shasum", vec!["-a", "256"]),
    ] {
        let Ok(output) = Command::new(program).args(&args).arg(path).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(sha) = text.split_whitespace().next() {
            return Some(sha.to_ascii_lowercase());
        }
    }
    None
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
        assert_eq!(
            local_path("./pinyin.db"),
            Some(PathBuf::from("./pinyin.db"))
        );
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

    #[test]
    fn 指纹不对就不装() {
        let dir = std::env::temp_dir().join("pliers-fetch-test-sha");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("来源.db");
        std::fs::write(&src, b"hello").unwrap();
        let dest = dir.join("装好.db");
        std::fs::write(&dest, b"old").unwrap();

        // 没装 sha256sum 的机器跳过（拿不到真指纹就没法验证）
        let Some(real) = sha256_of(&src) else {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };

        // 错的指纹：报错、目标文件不变、半截文件不留下
        let err = install_checked(
            &format!("file://{}", src.display()),
            &dest,
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
        )
        .unwrap_err();
        assert!(err.contains("校验不过"), "{err}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"old");
        assert!(!suffix(&dest, ".part").exists());

        // 对的指纹（大小写不敏感）：装成功
        install_checked(
            &format!("file://{}", src.display()),
            &dest,
            Some(&real.to_uppercase()),
        )
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
