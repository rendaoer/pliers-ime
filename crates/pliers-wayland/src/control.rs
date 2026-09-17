//! 命令行遥控：`pliers status` / `pliers reload` / `pliers set ...` 怎么跟**正在跑的那个实例**说上话。
//!
//! 一个 Unix domain socket，一问一答都是纯文本一行（好调试：`socat - UNIX-CONNECT:$PLIERS_SOCKET`
//! 也能直接说话）。输入法那边的主循环用 `poll(2)` 同时等 Wayland 和这个 socket，
//! 所以不用开线程、也不用给它塞定时器 —— 事件驱动，来了就处理。
//!
//! socket 放哪：`$PLIERS_SOCKET` 优先，其次 `$XDG_RUNTIME_DIR/pliers.sock`
//! （每个用户自己的 0700 目录，别的用户连不上），都没有才退到 `/tmp/pliers-<用户名>.sock`。

use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 一条命令最多等多久（客户端连上却不说话时，别把输入法卡住）
const TIMEOUT: Duration = Duration::from_secs(2);

/// 命令行和输入法约定的 socket 路径
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PLIERS_SOCKET") {
        return PathBuf::from(path);
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("pliers.sock");
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "default".into());
    std::env::temp_dir().join(format!("pliers-{user}.sock"))
}

/// 输入法这一侧的监听 socket
pub struct Control {
    listener: UnixListener,
    path: PathBuf,
}

impl Control {
    /// 建 socket 开始听。路径被占着（多半是上次没退干净）就先删掉再 bind
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// 给主循环 poll 用
    pub fn fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 有连接就接下来（没连接返回 None）。一次只接一个：
    /// 命令来得不勤，主循环每轮多调几次就够了
    pub fn accept(&self) -> Option<UnixStream> {
        match self.listener.accept() {
            Ok((stream, _)) => Some(stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(e) => {
                eprintln!("pliers: 命令 socket 接不上：{e}");
                None
            }
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // 退出时把 socket 文件收掉，免得下次启动看到"路径被占着"
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 读一行请求（连接建立后客户端马上就发，超时就当它不说）
pub fn read_request(stream: &UnixStream) -> std::io::Result<String> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line.trim_end().to_string())
}

/// 回一行结果
pub fn reply(stream: &mut UnixStream, message: &str) -> std::io::Result<()> {
    stream.set_write_timeout(Some(TIMEOUT))?;
    writeln!(stream, "{message}")?;
    stream.flush()
}

/// 客户端：把一条命令发给正在跑的实例，拿回它的一行回复。
/// 连不上 = 没有在跑的实例（或者它建 socket 失败了）
pub fn send(path: &Path, request: &str) -> std::io::Result<String> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    writeln!(stream, "{request}")?;
    stream.flush()?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer)?;
    Ok(answer.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 一问一答() {
        let path = std::env::temp_dir().join(format!(
            "pliers-ctrl-test-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        let control = Control::bind(&path).unwrap();

        // 客户端在另一个线程里说话，主线程这边假装是输入法
        let client = std::thread::spawn({
            let path = path.clone();
            move || send(&path, "set scheme.layout flypy").unwrap()
        });

        let mut stream = None;
        for _ in 0..200 {
            if let Some(s) = control.accept() {
                stream = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut stream = stream.expect("客户端没连上来");
        assert_eq!(read_request(&stream).unwrap(), "set scheme.layout flypy");
        reply(&mut stream, "OK 记下了").unwrap();
        assert_eq!(client.join().unwrap(), "OK 记下了");

        drop(stream);
        drop(control);
        assert!(!path.exists(), "退出时该把 socket 文件删掉");
    }
}
