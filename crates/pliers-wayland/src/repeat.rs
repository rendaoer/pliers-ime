//! 键盘自动重复：按住不放要能"连续按"（按住退格连删、按住方向键连挪）。
//!
//! **合成器不会替我们重复**。抓取键盘的接口
//! (`zwp_input_method_keyboard_grab_v2`) 只发一次按下、一次抬起，另外发一个
//! `repeat_info`（速率 + 延迟）—— 跟 `wl_keyboard` 一个规矩：**重复是客户端自己的事**
//! （GTK / Qt 的文本输入也是各自实现）。smithay 那边核对过：建好抓取对象之后它只发一句
//! `instance.repeat_info(rate, delay)`，之后按住不放不会再发 `key`。
//!
//! 所以这一小段就是"到点了把那个键当成又按了一次"。关键是**同一个键走同一条路**：
//! 引擎吃掉的还是吃掉（退格删一个字）、该转给应用的还是转发（终端里的退格）——
//! 这里不需要知道哪个键是干吗的。

use std::time::{Duration, Instant};

/// 按着的那个键。只有**最后**按下的那个会重复，跟真键盘一样
struct Held {
    keycode: u32,
    keysym: u32,
}

/// 自动重复：合成器给的速率/延迟 + 现在按着谁 + 下次什么时候该重复
#[derive(Default)]
pub struct Repeat {
    /// 每秒重复几次；0 = 不重复（协议明确允许，合成器也能随时改）
    rate: u32,
    /// 按下之后隔多久才开始重复
    delay: Duration,
    held: Option<Held>,
    /// 下一次该重复的时刻（没在重复时是 None）
    next: Option<Instant>,
    /// 合成器**自己**在替我们重复（收到过"同一个键的第二次按下"）。
    /// 协议上它不该这么干，但真有这种合成器：那我们就不抢着重复，免得更快
    external: bool,
}

impl Repeat {
    /// 合成器报的速率和延迟（`repeat_info`，抓取对象一建好就会发，之后还能改）。
    /// 协议说负数非法，速率 0 = 关掉重复
    pub fn set_info(&mut self, rate: i32, delay: i32) {
        self.rate = rate.max(0) as u32;
        self.delay = Duration::from_millis(delay.max(0) as u64);
        if self.rate == 0 {
            self.next = None;
        }
    }

    /// 一个按键（按下 / 抬起）。
    ///
    /// `repeatable = false` 的键永远不重复：修饰键（Shift/Ctrl 按住不放不该重复）、
    /// 以及输入法自己的切换键（Ctrl+空格按住不放会来回切中英文）
    pub fn key(
        &mut self,
        keycode: u32,
        keysym: u32,
        pressed: bool,
        repeatable: bool,
        now: Instant,
    ) {
        if !pressed {
            if self
                .held
                .as_ref()
                .is_some_and(|held| held.keycode == keycode)
            {
                self.clear();
            }
            return;
        }
        // 同一个键"又按下了一次"、中间没有抬起：合成器在自己重复（协议说它不该，
        // 但真遇到就顺着它，别两边一起重复）
        if self
            .held
            .as_ref()
            .is_some_and(|held| held.keycode == keycode)
        {
            self.external = true;
            self.next = None;
            return;
        }
        // 新的按下就换成它重复：按住 A 再按 B，重复的是 B（抬起 B 之后 A 也不会自己续上，
        // 跟真键盘一致 —— 真键盘也是这个行为）
        self.external = false;
        self.held = repeatable.then_some(Held { keycode, keysym });
        self.next = (repeatable && self.rate > 0).then(|| now + self.delay);
    }

    /// 到点了就把该重复的键交出来（调用方拿它"再按一次"）
    pub fn due(&mut self, now: Instant) -> Option<(u32, u32)> {
        let held = self.held.as_ref()?;
        if now < self.next? {
            return None;
        }
        // 落后太多（系统卡了一下）**不补一串**：从"现在"重新计时，
        // 不然卡 200ms 之后会一口气删掉几十个字符
        self.next = Some(now + self.interval());
        Some((held.keycode, held.keysym))
    }

    /// 这一轮 `poll(2)` 最晚该在什么时候醒（没在重复就是 `None`）
    pub fn timeout(&self, now: Instant) -> Option<Duration> {
        self.next.map(|next| next.saturating_duration_since(now))
    }

    /// 别再重复了（松开、失焦、抓取没了都走这儿）
    pub fn clear(&mut self) {
        self.held = None;
        self.next = None;
        self.external = false;
    }

    /// 两次重复之间隔多久。速率大得离谱（或者为 0）时也兜在 1ms 上，别把主循环转晕
    fn interval(&self) -> Duration {
        Duration::from_millis((1000 / self.rate.max(1) as u64).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 退格的 keycode/keysym（evdev 14 / X11 0xff08）
    const BACKSPACE: (u32, u32) = (14, 0xff08);

    #[test]
    fn 默认不重复() {
        // 合成器还没报 repeat_info 之前：一个键都不重复（速率默认 0）
        let mut repeat = Repeat::default();
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        assert_eq!(repeat.due(start + Duration::from_secs(10)), None);
        assert_eq!(repeat.timeout(start), None);
    }

    #[test]
    fn 先等延迟再按速率重复() {
        let mut repeat = Repeat::default();
        repeat.set_info(25, 600); // 25 次/秒 = 每 40ms 一次，按下后 600ms 开始
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);

        assert_eq!(
            repeat.timeout(start),
            Some(Duration::from_millis(600)),
            "先等延迟"
        );
        assert_eq!(repeat.due(start + Duration::from_millis(599)), None);
        assert_eq!(
            repeat.due(start + Duration::from_millis(600)),
            Some(BACKSPACE)
        );
        // 之后按速率来
        assert_eq!(repeat.due(start + Duration::from_millis(630)), None);
        assert_eq!(
            repeat.due(start + Duration::from_millis(640)),
            Some(BACKSPACE)
        );
        // 速率变了（协议说 repeat_info 之后还能再发）：已经定好的那一拍不变，
        // 再往后的按新速率算
        repeat.set_info(100, 0); // 100 次/秒 = 每 10ms
        assert_eq!(repeat.due(start + Duration::from_millis(660)), None);
        assert_eq!(
            repeat.due(start + Duration::from_millis(680)),
            Some(BACKSPACE)
        );
        assert_eq!(
            repeat.due(start + Duration::from_millis(690)),
            Some(BACKSPACE)
        );
    }

    #[test]
    fn 松开就不重复了() {
        let mut repeat = Repeat::default();
        repeat.set_info(25, 0);
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        assert_eq!(repeat.due(start), Some(BACKSPACE));
        repeat.key(BACKSPACE.0, BACKSPACE.1, false, true, start);
        assert_eq!(repeat.due(start + Duration::from_millis(100)), None);
        assert_eq!(repeat.timeout(start), None, "没按着东西就别定闹钟");
        // 松开的是别的键：这个键接着重复（同时按住两个键的场合）
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        repeat.key(30, 0x61, false, true, start);
        assert!(repeat.due(start).is_some());
    }

    #[test]
    fn 速率零就是关掉() {
        let mut repeat = Repeat::default();
        repeat.set_info(25, 0);
        repeat.set_info(0, 600); // 关了
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        assert_eq!(repeat.due(start + Duration::from_secs(5)), None);
    }

    #[test]
    fn 只有最后按下的那个键重复() {
        let mut repeat = Repeat::default();
        repeat.set_info(25, 0);
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        repeat.key(30, 0x61, true, true, start); // 又按了 a
        assert_eq!(repeat.due(start), Some((30, 0x61)));
    }

    #[test]
    fn 修饰键和切换键不重复() {
        // 修饰键：按住 Shift 不放不该重复（不然松手时会被当成"轻按 Shift"切模式，
        // 而且应用会收到一串假的 Shift）
        let mut repeat = Repeat::default();
        repeat.set_info(25, 0);
        let start = Instant::now();
        repeat.key(42, 0xffe1, true, false, start);
        assert_eq!(repeat.due(start + Duration::from_secs(1)), None);
        // 输入法自己的切换键（Ctrl+空格）同理：按住不放会来回切中英文
        repeat.key(57, 0x20, true, false, start);
        assert_eq!(repeat.due(start + Duration::from_secs(1)), None);
    }

    #[test]
    fn 卡了一下也不补一串() {
        let mut repeat = Repeat::default();
        repeat.set_info(25, 0); // 每 40ms 一次
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        // 系统卡了 2 秒才回到主循环：只补一次，而且下一拍从"现在"重新算
        let late = start + Duration::from_secs(2);
        assert_eq!(repeat.due(late), Some(BACKSPACE));
        assert_eq!(repeat.due(late), None);
        assert_eq!(
            repeat.timeout(late),
            Some(Duration::from_millis(40)),
            "下一拍从现在算起"
        );
    }

    #[test]
    fn 合成器自己重复时不抢着重复() {
        // 有合成器既发 repeat_info 又自己重发按键（协议上不该，但真遇到过）：
        // 收到"同一个键又按下一次"就该让位，不然一次长按会删两倍
        let mut repeat = Repeat::default();
        repeat.set_info(25, 0);
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        assert!(repeat.due(start).is_some(), "先按我们的节拍走");
        // 合成器重复了一次
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        assert_eq!(repeat.due(start + Duration::from_millis(500)), None);
        assert_eq!(repeat.timeout(start), None);
        // 松开再按：又归我们管
        repeat.key(BACKSPACE.0, BACKSPACE.1, false, true, start);
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        assert_eq!(repeat.due(start), Some(BACKSPACE));
    }

    #[test]
    fn 失焦之类的场合清干净() {
        let mut repeat = Repeat::default();
        repeat.set_info(25, 0);
        let start = Instant::now();
        repeat.key(BACKSPACE.0, BACKSPACE.1, true, true, start);
        repeat.clear();
        assert_eq!(repeat.due(start + Duration::from_secs(1)), None);
        assert_eq!(repeat.timeout(start), None);
    }
}
