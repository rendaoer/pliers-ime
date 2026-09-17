//! 交互式设置用的那点终端功夫：原始模式、读一个按键、上下选的列表、一行文本输入。
//!
//! 为什么不引 `crossterm`/`ratatui`：这里只要"上下 + 回车 + 一句话输入"，
//! 用 libc 的 `tcgetattr`/`tcsetattr` 加几个 ANSI 转义就够，几十行的事 ——
//! 跟这个项目其余部分一样，看得见摸得着。
//!
//! 注意：**不进备用屏幕**（不像 vim 那样整屏接管），选完就把自己画的那几行擦掉，
//! 结果留在滚动历史里，跟普通命令行的输出混在一起很自然。

use std::io::{self, IsTerminal, Read, Write};
use std::os::fd::AsRawFd;

/// 终端原始模式守卫：Drop 时恢复原样。
///
/// `ISIG` 也关掉了，所以 Ctrl+C 不会变成信号（不会在没恢复终端的情况下把进程打死），
/// 而是当成一个普通按键交给我们处理
pub struct RawMode {
    saved: libc::termios,
}

impl RawMode {
    pub fn enable() -> io::Result<Self> {
        let fd = io::stdin().as_raw_fd();
        // SAFETY: termios 是 POD，fd 是打开的 stdin
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IXON);
        // VMIN=0 + VTIME=1：read 最多等 100ms 就返回 0，够把 Esc 和方向键分开
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { saved })
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let fd = io::stdin().as_raw_fd();
        // SAFETY: saved 是上面 tcgetattr 填的
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &self.saved) };
    }
}

// ---- 排版小工具（为这点需求引 unicode-width 不划算）-------------------------

/// 一个字符占几列：CJK、全角标点算两列，别的算一列
fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x20000..=0x3fffd)
}

/// 显示宽度（终端里占几列）
pub fn width(text: &str) -> usize {
    text.chars().map(|ch| if is_wide(ch) { 2 } else { 1 }).sum()
}

/// 补空格到指定显示宽度（已经够长就原样返回）
pub fn pad(text: &str, width: usize) -> String {
    let mut out = text.to_string();
    let mut current = self::width(text);
    while current < width {
        out.push(' ');
        current += 1;
    }
    out
}

/// 要不要上色：不是终端、或者设了 `NO_COLOR`，就老老实实输出纯文本
fn colors() -> bool {
    std::env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal()
}

/// ANSI 上色（`code` 比如 `1` 加粗、`2` 变暗、`1;36` 亮青）
fn style(text: &str, code: &str) -> String {
    if colors() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

// ---- 按键 -------------------------------------------------------------------

/// 一个按键
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Enter,
    Escape,
    Backspace,
    Char(char),
    /// Ctrl + 字母（Ctrl+C 也走这儿）
    Ctrl(char),
}

/// 读一个字节。超时（100ms 没动静）返回 None —— 用来判断 Esc 后面还有没有东西
fn read_byte_timeout() -> io::Result<Option<u8>> {
    let mut byte = [0u8; 1];
    loop {
        match io::stdin().read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => return Ok(Some(byte[0])),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// 读一个字节，一直等到有为止
fn read_byte() -> io::Result<u8> {
    loop {
        if let Some(byte) = read_byte_timeout()? {
            return Ok(byte);
        }
    }
}

/// 读一个按键（UTF-8 的多字节字符也拼回来）
pub fn read_key() -> io::Result<Key> {
    let first = read_byte()?;
    Ok(match first {
        // Esc、方向键、Home/End 之类都是 ESC 开头
        0x1b => match read_byte_timeout()? {
            Some(b'[') | Some(b'O') => match read_byte_timeout()? {
                Some(b'A') => Key::Up,
                Some(b'B') => Key::Down,
                _ => Key::Char(' '), // 别的不关心
            },
            _ => Key::Escape,
        },
        b'\r' | b'\n' => Key::Enter,
        0x7f | 0x08 => Key::Backspace,
        b if b < 0x20 => Key::Ctrl((b'a' + b - 1) as char),
        b if b < 0x80 => Key::Char(b as char),
        // UTF-8：首字节告诉你后面还有几个
        b => {
            let extra = match b {
                0xf0..=0xf7 => 3,
                0xe0..=0xef => 2,
                _ => 1,
            };
            let mut bytes = vec![b];
            for _ in 0..extra {
                bytes.push(read_byte()?);
            }
            match std::str::from_utf8(&bytes) {
                Ok(text) => text.chars().next().map_or(Key::Escape, Key::Char),
                Err(_) => Key::Escape,
            }
        }
    })
}

// ---- 上下选的列表 -----------------------------------------------------------

/// 列表里的一项：`label      现在 now          options`
pub struct Item {
    pub label: String,
    pub now: String,
    pub options: String,
    /// 当前生效的那个（值选择器里用绿点标出来）
    pub mark: bool,
}

impl Item {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            now: String::new(),
            options: String::new(),
            mark: false,
        }
    }

    /// 右边两列：现在的值 + 可选项（都没有就只显示标题）
    pub fn detail(mut self, now: impl Into<String>, options: impl Into<String>) -> Self {
        self.now = now.into();
        self.options = options.into();
        self
    }

    pub fn marked(mut self, mark: bool) -> Self {
        self.mark = mark;
        self
    }
}

/// 上下选的列表：画在光标当前位置，退出时把自己擦掉
pub struct Picker {
    title: String,
    items: Vec<Item>,
    cursor: usize,
    /// 已经画了几行（擦的时候要往上退这么多行）
    drawn: usize,
}

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<Item>) -> Self {
        Self {
            title: title.into(),
            items,
            cursor: 0,
            drawn: 0,
        }
    }

    /// 上下挪（绕圈）
    pub fn step(&mut self, delta: isize) {
        let count = self.items.len() as isize;
        if count == 0 {
            return;
        }
        self.cursor = (self.cursor as isize + delta).rem_euclid(count) as usize;
    }

    /// 直接把光标放到某一项（再打开时停在刚才那项上）
    pub fn focus(&mut self, index: usize) {
        if index < self.items.len() {
            self.cursor = index;
        }
    }

    /// 某一项渲染出来长什么样（抽出来是为了能单测，不用真开终端）
    fn render(&self, index: usize) -> String {
        let item = &self.items[index];
        let selected = index == self.cursor;
        let arrow = if selected {
            style("❯", "1;36")
        } else {
            " ".to_string()
        };
        let mark = if item.mark {
            style("●", "32")
        } else {
            " ".to_string()
        };
        let label = pad(&item.label, 12);
        let label = if selected { style(&label, "1") } else { label };

        let mut line = format!("{arrow} {mark} {label}");
        if !item.now.is_empty() {
            line.push(' ');
            line.push_str(&style(&pad(&item.now, 18), "2"));
        }
        if !item.options.is_empty() {
            line.push(' ');
            line.push_str(&style(&item.options, "2"));
        }
        line
    }

    /// 每行长什么样（第一行标题，最后一行按键提示）
    pub fn lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "{} {}",
            style("?", "1;36"),
            style(&self.title, "1")
        )];
        lines.extend((0..self.items.len()).map(|index| self.render(index)));
        lines.push(style("  ↑↓ 选择 · Enter 确认 · 数字键直达 · q 退出", "2"));
        lines
    }

    fn draw(&mut self) -> io::Result<()> {
        let mut out = io::stdout();
        // 藏光标：不藏的话重画时它会满屏乱跳
        write!(out, "\x1b[?25l")?;
        if self.drawn > 0 {
            write!(out, "\x1b[{}A", self.drawn)?;
        }
        let lines = self.lines();
        for line in &lines {
            write!(out, "\x1b[2K{line}\r\n")?;
        }
        write!(out, "\x1b[J")?; // 擦掉下面多余的
        out.flush()?;
        self.drawn = lines.len();
        Ok(())
    }

    /// 把自己擦掉，光标回到第一行行首
    pub fn erase(&mut self) -> io::Result<()> {
        if self.drawn > 0 {
            let mut out = io::stdout();
            write!(out, "\x1b[{}A\x1b[J\x1b[?25h", self.drawn)?;
            out.flush()?;
            self.drawn = 0;
        }
        Ok(())
    }

    /// 跑起来：返回选中的下标，取消（q / Esc / Ctrl+C）返回 None
    pub fn run(&mut self) -> io::Result<Option<usize>> {
        if self.items.is_empty() {
            return Ok(None);
        }
        self.draw()?;
        loop {
            match read_key()? {
                Key::Up | Key::Char('k') => {
                    self.step(-1);
                    self.draw()?;
                }
                Key::Down | Key::Char('j') => {
                    self.step(1);
                    self.draw()?;
                }
                Key::Char(digit) if digit.is_ascii_digit() && digit != '0' => {
                    let index = digit as usize - '1' as usize;
                    if index < self.items.len() {
                        self.cursor = index;
                        self.draw()?;
                    }
                }
                Key::Enter => {
                    let chosen = self.cursor;
                    self.erase()?;
                    return Ok(Some(chosen));
                }
                Key::Escape | Key::Char('q') | Key::Ctrl('c') => {
                    self.erase()?;
                    return Ok(None);
                }
                _ => {}
            }
        }
    }
}

// ---- 一行输入 ---------------------------------------------------------------

/// 一行文本输入，预填一个默认值（回车确认、Esc 取消、退格删）
pub fn input_line(title: &str, default: &str) -> io::Result<Option<String>> {
    let mut text = default.to_string();
    let mut out = io::stdout();
    writeln!(
        out,
        "{} {}  {}",
        style("?", "1;36"),
        style(title, "1"),
        style("（Enter 确认 · Esc 取消 · 退格删）", "2")
    )?;
    write!(out, "\x1b[?25h")?;
    loop {
        write!(out, "\x1b[2K{} {text}", style("❯", "1;36"))?;
        out.flush()?;
        match read_key()? {
            Key::Enter => {
                write!(out, "\r\n")?;
                out.flush()?;
                return Ok(Some(text));
            }
            Key::Escape | Key::Ctrl('c') => {
                write!(out, "\r\n")?;
                out.flush()?;
                return Ok(None);
            }
            Key::Backspace => {
                text.pop();
            }
            Key::Char(ch) => text.push(ch),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker() -> Picker {
        Picker::new(
            "改哪个？",
            vec![
                Item::new("甲").detail("1", "1 / 2"),
                Item::new("乙").detail("2", "1 / 2"),
                Item::new("丙"),
            ],
        )
    }

    /// 现在选中的是第几项（看谁带箭头）
    fn cursor_of(list: &Picker) -> usize {
        list.lines()
            .iter()
            .position(|line| line.contains('❯'))
            .expect("总得有一项是选中的")
            - 1 // 第一行是标题
    }

    #[test]
    fn 选中项带箭头其他不带() {
        let list = picker();
        let lines = list.lines();
        assert_eq!(lines.len(), 5, "标题 + 三项 + 提示：{lines:?}");
        assert!(lines[0].contains("改哪个？"), "{:?}", lines[0]);
        assert!(lines[1].contains('❯'), "{:?}", lines[1]);
        assert!(!lines[2].contains('❯'), "{:?}", lines[2]);
        assert!(lines[4].contains("Enter"), "{:?}", lines[4]);
    }

    #[test]
    fn 上下挪是绕圈的() {
        let mut list = picker();
        list.step(-1);
        assert_eq!(cursor_of(&list), 2, "从第一个往上 = 最后一个");
        list.step(1);
        assert_eq!(cursor_of(&list), 0);
        list.step(1);
        assert_eq!(cursor_of(&list), 1);
    }

    #[test]
    fn 可以指定停在哪一项() {
        let mut list = picker();
        list.focus(2);
        assert_eq!(cursor_of(&list), 2);
        list.focus(99); // 越界不动
        assert_eq!(cursor_of(&list), 2);
    }

    #[test]
    fn 空列表不会崩() {
        let mut list = Picker::new("空的", Vec::new());
        list.step(1);
        assert_eq!(list.lines().len(), 2, "标题 + 提示");
    }

    #[test]
    fn 中文按两列对齐() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("双拼"), 4);
        assert_eq!(pad("双拼", 6), "双拼  ");
        assert_eq!(width(&pad("输入方案", 12)), 12);
        assert_eq!(width(&pad("一页候选数", 12)), 12);
        // 已经够宽就不补
        assert_eq!(pad("abcdef", 3), "abcdef");
    }

    #[test]
    fn 当前值带绿点() {
        let list = Picker::new(
            "值",
            vec![
                Item::new("natural"),
                Item::new("flypy").marked(true),
                Item::new("none"),
            ],
        );
        let lines = list.lines();
        assert!(lines[2].contains('●'), "自己那项该有标记：{:?}", lines[2]);
        assert!(!lines[1].contains('●'), "{:?}", lines[1]);
    }
}
