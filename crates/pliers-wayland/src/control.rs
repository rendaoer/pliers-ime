//! 命令行遥控：`pliers status` / `pliers reload` / `pliers set ...` 怎么跟**正在跑的那个实例**说上话。
//!
//! 一个 Unix domain socket，一问一答都是纯文本一行（好调试：`socat - UNIX-CONNECT:$PLIERS_SOCKET`
//! 也能直接说话）。输入法那边的主循环用 `poll(2)` 同时等 Wayland 和这个 socket，
//! 所以不用开线程、也不用给它塞定时器 —— 事件驱动，来了就处理。
//!
//! socket 放哪：`$PLIERS_SOCKET` 优先，其次 `$XDG_RUNTIME_DIR/pliers.sock`
//! （每个用户自己的 0700 目录，别的用户连不上），都没有才退到 `/tmp/pliers-<用户名>.sock`。

use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 一条命令最多等多久（客户端连上却不说话时，别把输入法卡住）
const TIMEOUT: Duration = Duration::from_secs(2);

/// 控制 socket 上读到的东西
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// 客户端发来的一行（去掉换行；可能是个空行）
    Line(String),
    /// 对端连上就关了，一个字都没发 —— 多半是查"有没有实例在跑"的探活
    Closed,
}

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
    /// 建 socket 开始听。
    ///
    /// 路径被占着时先分清楚是**死文件**还是**真有实例在听**：后者绝不能删 ——
    /// 第二个实例马上会因为 seat 被第一个接管而退出，删掉 socket 却会让第一个实例的
    /// `pliers status` / `reload` / `set` 全部失灵（它的监听 fd 还在，但路径没了）。
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        if path.exists() {
            if UnixStream::connect(path).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "已经有实例在听这个 socket 了",
                ));
            }
            std::fs::remove_file(path)?; // 连不上 = 上次没退干净留下的
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

/// 读一行请求（连接建立后客户端马上就发，超时就当它不说）。
///
/// `Closed` 要跟空行分开：`Control::bind` 判断"路径上有没有活实例"时就是
/// 连上再关掉，拿这种探活当命令回一句 `ERR 空命令` 只会换来一个 Broken pipe。
pub fn read_request(stream: &UnixStream) -> std::io::Result<Request> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    let mut line = String::new();
    if BufReader::new(stream).read_line(&mut line)? == 0 {
        return Ok(Request::Closed);
    }
    Ok(Request::Line(line.trim_end().to_string()))
}

/// 回一行结果。返回值表示**这次回话有没有送出去**：
/// 客户端已经走了（命令被 `timeout` 掐掉、`pliers status | head` 关了管道、
/// 或只是探活）时返回 `Ok(false)` —— 对端不在，写失败是必然的，不算故障。
pub fn reply(stream: &mut UnixStream, message: &str) -> std::io::Result<bool> {
    stream.set_write_timeout(Some(TIMEOUT))?;
    match writeln!(stream, "{message}").and_then(|()| stream.flush()) {
        Ok(()) => Ok(true),
        Err(e) if client_gone(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

/// 这个错误是不是"对端先走了"，而不是我们这边写坏了
fn client_gone(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::BrokenPipe
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
    )
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
        assert_eq!(
            read_request(&stream).unwrap(),
            Request::Line("set scheme.layout flypy".into())
        );
        assert!(reply(&mut stream, "OK 记下了").unwrap());
        assert_eq!(client.join().unwrap(), "OK 记下了");

        drop(stream);
        drop(control);
        assert!(!path.exists(), "退出时该把 socket 文件删掉");
    }

    #[test]
    fn 连上就关是探活不是命令() {
        // `Control::bind` 判断"路径上有没有活实例"就是这么干的：连上、一个字不发、关掉
        let (client, server) = UnixStream::pair().unwrap();
        drop(client);
        assert_eq!(read_request(&server).unwrap(), Request::Closed);
    }

    #[test]
    fn 空行跟探活得分得开() {
        // 真发了个空行的人（比如 nc 里按了个回车）该收到「空命令」提示
        let (mut client, server) = UnixStream::pair().unwrap();
        writeln!(client).unwrap();
        assert_eq!(read_request(&server).unwrap(), Request::Line(String::new()));
    }

    #[test]
    fn 对端走了回话不报错() {
        // 连接被 timeout 掐掉 / 探活连上就关：写不进去是必然的，
        // 不能当成服务器的故障往 stderr 上刷
        let (client, mut server) = UnixStream::pair().unwrap();
        drop(client);
        assert!(!reply(&mut server, "OK\tmode=中").unwrap());
    }

    fn temp_sock(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "pliers-ctrl-{tag}-{}-{:?}.sock",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    #[test]
    fn 不抢活着的实例的_socket() {
        let path = temp_sock("alive");
        let _ = std::fs::remove_file(&path);
        let first = Control::bind(&path).unwrap();

        // 第二个实例（马上会因为 seat 被接管退出）不能把第一个的 socket 删掉
        assert!(Control::bind(&path).is_err());
        assert!(path.exists(), "活着的实例的 socket 文件不能在");
        assert!(UnixStream::connect(&path).is_ok(), "第一个实例还得能被连上");

        drop(first);
        assert!(!path.exists());
    }

    #[test]
    fn 死掉的_socket_文件可以重绑() {
        let path = temp_sock("stale");
        let _ = std::fs::remove_file(&path);
        // 只 bind 不 listen：文件留下、没人听（跟上次崩溃退出一样）
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        assert!(path.exists());

        let control = Control::bind(&path).expect("死文件该被清掉重绑");
        assert!(UnixStream::connect(&path).is_ok());
        drop(control);
        assert!(!path.exists());
    }
}
