//! 一个极小的 `poll(2)` 封装。
//!
//! 只为了主循环能同时等两个 fd（Wayland 的 socket + 命令 socket）。
//! 标准库没有 poll/select，`wayland-client` 也只管自己那个 fd，
//! 所以这里直接用 libc 调一次 —— 比引一个大运行时划算。

use std::io;
use std::os::fd::RawFd;

/// 等这几个 fd 里任意一个**可读**，返回和入参等长的布尔表。
///
/// `timeout` 是"最多睡多久"（`None` = 一直睡到有人叫醒）：自动重读配置文件要靠它 ——
/// 文件变了又没人说话，光等 fd 是等不到的
pub fn readable(fds: &[RawFd], timeout: Option<std::time::Duration>) -> io::Result<Vec<bool>> {
    let mut pollfds: Vec<libc::pollfd> = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();

    let millis = match timeout {
        Some(duration) => duration.as_millis().min(i32::MAX as u128) as libc::c_int,
        None => -1,
    };
    loop {
        // SAFETY: pollfds 是一块长度正确的连续内存，poll 只会往里写 revents
        let ready =
            unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, millis) };
        if ready >= 0 {
            return Ok(pollfds
                .iter()
                .map(|p| p.revents & libc::POLLIN != 0)
                .collect());
        }
        let error = io::Error::last_os_error();
        // 被信号打断（SIGWINCH 之类）不算错，接着等
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
