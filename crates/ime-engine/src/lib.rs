//! 输入法引擎：只做"这个按键该干什么"的决定，完全不碰 Wayland。
//!
//! 把这一层单独拆出来的好处：词表、组词规则这些真正属于"输入法"的东西可以脱离合成器
//! 跑测试（`cargo test -p ime-engine`），以后想换界面（候选框从 shm 换成 Slint/egui）
//! 或者换协议层，这一层都不用动。
//!
//! 对外只有两个类型：进去是 [`KeyInput`]（协议层翻译好的一个按键），
//! 出来是 [`Action`]（该干什么），中间的状态是 [`Preedit`]（候选框要显示什么）。

mod dict;

pub use dict::{DICT, candidates, convert};

/// 空格（X11 keysym）
pub const KEY_SPACE: u32 = 0x20;
/// 退格
pub const KEY_BACKSPACE: u32 = 0xff08;
/// 回车
pub const KEY_RETURN: u32 = 0xff0d;
/// 小键盘回车
pub const KEY_KP_ENTER: u32 = 0xff8d;
/// Esc
pub const KEY_ESCAPE: u32 = 0xff1b;
/// Tab（翻下一个候选）
pub const KEY_TAB: u32 = 0xff09;
/// Shift+Tab：keysym 不是 Tab，而是 ISO_Left_Tab
pub const KEY_ISO_LEFT_TAB: u32 = 0xfe20;
/// ↑
pub const KEY_UP: u32 = 0xff52;
/// ↓
pub const KEY_DOWN: u32 = 0xff54;

/// 数字 1..9：直接选第几个候选上屏（keysym 就是 ASCII '1'..'9'）
const KEY_1: u32 = 0x31;
const KEY_9: u32 = 0x39;

/// 协议层翻译好的一个按键事件（keycode + keysym + 修饰键状态）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    /// 物理按键编号（Linux evdev），记着它是为了把按下和抬起配上对
    pub keycode: u32,
    /// 字符编号（X11 keysym）：按键在**当前修饰键状态**下对应的字符，
    /// 所以 Shift/Caps Lock 打出来的大写在这里就是 'A'（0x41）
    pub keysym: u32,
    pub pressed: bool,
    /// Ctrl/Alt/Super 之一按着：这是快捷键，不能拿去组词
    pub shortcut: bool,
    /// 现在有输入框在用吗（没有的话按键只能原样转发）
    pub active: bool,
}

/// 组词状态的一份快照 = 候选框要显示的全部内容。
///
/// 引擎每次改变状态都把这个整体交出去，界面层不用反着问引擎"现在该显示啥"，
/// 也就不会出现"引擎里已经换了、界面上还是旧的"这种不同步。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preedit {
    /// 预编辑串：交给应用显示在输入框里的原文（拼音）
    pub text: String,
    /// 候选词。空 = 没在组词
    pub candidates: Vec<String>,
    /// 选中的是第几个候选（从 0 开始）
    pub selected: usize,
}

impl Preedit {
    /// 选中的那个候选词（空格上屏的就是它）
    pub fn current(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(String::as_str)
    }
}

/// 引擎的决定，协议层照着执行
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 组词状态变了：预编辑串 / 候选列表 / 选中的候选，都在这份快照里
    ///（没在组词时 `candidates` 为空，界面层就该把候选框收起来）
    UpdatePreedit(Preedit),
    /// 把这段文本提交给应用
    Commit(String),
    /// 原样转发给应用（走虚拟键盘）
    Forward,
    /// 吃掉，什么都不做（它的抬起事件也会被吃掉）
    Swallow,
}

/// 组词状态
#[derive(Default)]
pub struct Engine {
    /// 攒着的东西：拼音，或者用户用 Shift/Caps Lock 敲出来的大写英文
    buffer: String,
    /// 选中第几个候选（打字过程中每来一个新字母都回到第一个）
    selected: usize,
    /// 被我们吃掉的按键：它们的抬起事件也得吃掉，
    /// 否则应用会收到"没按下就直接抬起"，修饰键状态可能错乱
    consumed: Vec<u32>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// 预编辑串原文（拼音）
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// 现在这一屏该显示什么
    pub fn preedit(&self) -> Preedit {
        Preedit {
            text: self.buffer.clone(),
            candidates: candidates(&self.buffer),
            selected: self.selected,
        }
    }

    /// 输入框失焦：按住没放的键不会再有抬起事件了，账本一起清掉
    pub fn reset(&mut self) {
        self.clear_composing();
        self.consumed.clear();
    }

    /// 组词结束（上屏 / 取消）：只清组词状态，按键账本留着 ——
    /// 刚刚那个键（空格、数字、Esc）的**抬起**还得吃掉，不然应用会收到
    /// 一个"没按下过就抬起"的野按键
    fn clear_composing(&mut self) {
        self.buffer.clear();
        self.selected = 0;
    }

    /// 处理一个按键，返回该做什么
    pub fn on_key(&mut self, key: KeyInput) -> Action {
        // 没有输入框在用（焦点在 XWayland 应用、或不支持 text-input 的应用上）时，
        // 键盘抓取仍然在我们手里，所以必须原样转发，绝不能组词吞键
        if !key.active {
            return Action::Forward;
        }

        // 抬起事件：账本里有的继续吃掉（一次清光，长按会重复记账），
        // 账本里没有的原样转发给应用
        if !key.pressed {
            if self.consumed.contains(&key.keycode) {
                self.consumed.retain(|k| *k != key.keycode);
                return Action::Swallow;
            }
            return Action::Forward;
        }

        // Ctrl/Alt/Super 按着的都是快捷键（Ctrl+A 全选、Ctrl+C 中断、Alt+F 菜单），
        // 一律原样转发。注意 key_get_one_sym() **不做** Control 变换：按住 Ctrl 时 'a'
        // 解出来还是 0x61（只有 key_get_utf8() 才给 \x01），光看 keysym 分不出来，
        // 只能靠协议层给的 shortcut 标志
        if key.shortcut {
            return Action::Forward;
        }

        let composing = !self.buffer.is_empty();
        match key.keysym {
            // 字母 a-z / A-Z：一律攒进 buffer 当预编辑 —— 包括 Shift、Caps Lock 打出来的
            // 大写。打字中途绝不往应用里塞字符：应用这时正处在预编辑状态，对"野生"字符的
            // 处理不可靠（实测转发 Shift+A 过去，输入框里 A 和 a 都不出现）
            0x41..=0x5a | 0x61..=0x7a => match char::from_u32(key.keysym) {
                Some(ch) => {
                    self.buffer.push(ch);
                    self.consumed.push(key.keycode);
                    self.changed()
                }
                None => Action::Forward,
            },

            // 空格：把选中的候选交给应用（nihao → 你好）；没在组词就当普通空格
            KEY_SPACE if composing => self.commit_candidate(key.keycode),

            // 回车：把还没转换的内容原样提交，不把回车交给应用
            //（否则表单会被顺手提交掉）
            KEY_RETURN | KEY_KP_ENTER if composing => {
                self.consumed.push(key.keycode);
                let text = std::mem::take(&mut self.buffer);
                self.selected = 0;
                Action::Commit(text) // 账本不动：回车自己的抬起还得吃掉
            }

            // Esc：取消这次组词，这个键谁也不给（否则会顺手退出全屏、关掉弹窗）
            KEY_ESCAPE if composing => {
                self.consumed.push(key.keycode);
                self.clear_composing();
                Action::UpdatePreedit(Preedit::default())
            }

            // 退格：删掉一个字符（候选列表跟着重算，选中回到第一个）
            KEY_BACKSPACE if composing => {
                self.consumed.push(key.keycode);
                self.buffer.pop();
                self.changed()
            }

            // ↓ / Tab：下一个候选。组词当中方向键不该去动输入框里的光标
            KEY_DOWN | KEY_TAB if composing => {
                self.consumed.push(key.keycode);
                self.step(1)
            }

            // ↑ / Shift+Tab：上一个候选
            KEY_UP | KEY_ISO_LEFT_TAB if composing => {
                self.consumed.push(key.keycode);
                self.step(-1)
            }

            // 数字 1-9：直接选第几个候选上屏（拼音里的数字没法组词，吃掉不亏）
            KEY_1..=KEY_9 if composing => {
                let index = (key.keysym - KEY_1) as usize;
                self.consumed.push(key.keycode);
                match candidates(&self.buffer).get(index).cloned() {
                    Some(word) => {
                        self.clear_composing();
                        Action::Commit(word)
                    }
                    // 只有 4 个候选却按了 7：当没按过（但也不把数字塞进拼音）
                    None => Action::Swallow,
                }
            }

            // 其他按键（F1、PgUp……）都交给应用
            _ => Action::Forward,
        }
    }

    /// 缓冲内容变了：候选列表跟着变，选中回到第一个。
    /// 打字过程中新出现的候选才是你要的，停在旧的行上没意义
    fn changed(&mut self) -> Action {
        self.selected = 0;
        Action::UpdatePreedit(self.preedit())
    }

    /// 选中项上下走一格（绕圈）
    fn step(&mut self, delta: i32) -> Action {
        let count = candidates(&self.buffer).len();
        if count == 0 {
            return Action::Forward;
        }
        self.selected = (self.selected as i32 + delta).rem_euclid(count as i32) as usize;
        Action::UpdatePreedit(self.preedit())
    }

    /// 上屏当前选中的候选
    fn commit_candidate(&mut self, keycode: u32) -> Action {
        let list = candidates(&self.buffer);
        let word = list
            .get(self.selected)
            .or_else(|| list.first())
            .cloned()
            .unwrap_or_default();
        self.consumed.push(keycode);
        self.clear_composing();
        Action::Commit(word)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个"有输入框、没按修饰键"的按键。
    /// keycode 直接拿 keysym 顶替 —— 测试里只要求"不同的键有不同的 keycode"，
    /// 好把按下和抬起配上对
    fn key(keysym: u32) -> KeyInput {
        KeyInput {
            keycode: keysym,
            keysym,
            pressed: true,
            shortcut: false,
            active: true,
        }
    }

    /// 动作里的预编辑串（不是这个动作就 None）
    fn text_of(action: Action) -> Option<String> {
        match action {
            Action::UpdatePreedit(preedit) => Some(preedit.text),
            _ => None,
        }
    }

    /// 敲一串字母，返回每一步的预编辑串
    fn type_letters(engine: &mut Engine, letters: &str) -> Vec<String> {
        let mut seen = Vec::new();
        for ch in letters.chars() {
            let Some(text) = text_of(engine.on_key(key(ch as u32))) else {
                panic!("打字不该产生别的动作");
            };
            seen.push(text);
        }
        seen
    }

    #[test]
    fn 打_nihao_再空格出你好() {
        let mut engine = Engine::new();
        assert_eq!(type_letters(&mut engine, "nihao").last().unwrap(), "nihao");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("你好".into()));
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 预编辑串逐字母长大() {
        let mut engine = Engine::new();
        assert_eq!(
            type_letters(&mut engine, "nihao"),
            ["n", "ni", "nih", "niha", "nihao"]
        );
    }

    #[test]
    fn 候选框跟着输入长大() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        let preedit = engine.preedit();
        assert_eq!(preedit.text, "nihao");
        assert_eq!(preedit.candidates, ["你好", "尼好", "妮好", "拟好"]);
        assert_eq!(preedit.selected, 0);
        assert_eq!(preedit.current(), Some("你好"));
    }

    #[test]
    fn 没在组词时没有候选() {
        let mut engine = Engine::new();
        assert!(engine.preedit().candidates.is_empty());
        // 打一半再全删掉，候选也得跟着没
        type_letters(&mut engine, "n");
        engine.on_key(key(KEY_BACKSPACE));
        assert!(engine.preedit().candidates.is_empty());
    }

    #[test]
    fn 方向键换候选空格上屏选中的() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        // ↓ 选中第二个
        assert_eq!(
            text_of(engine.on_key(key(KEY_DOWN))).unwrap(),
            "nihao" // 换候选不动预编辑串
        );
        assert_eq!(engine.preedit().selected, 1);
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("尼好".into()));
    }

    #[test]
    fn tab_也能翻候选() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        engine.on_key(key(KEY_TAB));
        engine.on_key(key(KEY_TAB));
        assert_eq!(engine.preedit().selected, 2);
        // Shift+Tab 往回走一格
        engine.on_key(key(KEY_ISO_LEFT_TAB));
        assert_eq!(engine.preedit().selected, 1);
        // ↑ 走到头会绕回最后一个
        engine.on_key(key(KEY_UP));
        engine.on_key(key(KEY_UP));
        assert_eq!(engine.preedit().selected, 3);
    }

    #[test]
    fn 数字直接选词上屏() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        assert_eq!(engine.on_key(key(0x33)), Action::Commit("妮好".into())); // '3'
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 数字超出候选范围就什么都别发生() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao"); // 只有 4 个候选
        assert_eq!(engine.on_key(key(0x39)), Action::Swallow); // '9'
        assert_eq!(engine.text(), "nihao"); // 没被提交，也没混进拼音
    }

    #[test]
    fn 退格之后选中回到第一个() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        engine.on_key(key(KEY_DOWN));
        assert_eq!(engine.preedit().selected, 1);
        engine.on_key(key(KEY_BACKSPACE));
        assert_eq!(engine.preedit().selected, 0);
        // "niha" 查不到，候选只剩原文一条，选中的还是第一个
        assert_eq!(engine.preedit().candidates, ["niha"]);
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("niha".into()));
    }

    #[test]
    fn 退格删掉一个字符() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihaox");
        assert_eq!(text_of(engine.on_key(key(KEY_BACKSPACE))).unwrap(), "nihao");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("你好".into()));
    }

    #[test]
    fn 查不到的词原样提交() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "abc");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("abc".into()));
    }

    #[test]
    fn 大写字母也待在预编辑里() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "aaa");
        // Shift+A：keysym 是 'A'（0x41），协议层翻译时就带上了修饰键的影响
        assert_eq!(text_of(engine.on_key(key(0x41))).unwrap(), "aaaA");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("aaaA".into()));
    }

    #[test]
    fn caps_lock_的大写照样能转拼音() {
        let mut engine = Engine::new();
        // Caps Lock 打开：keysym 全是大写
        assert_eq!(type_letters(&mut engine, "NIHAO").last().unwrap(), "NIHAO");
        // 查表大小写不敏感
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("你好".into()));
    }

    #[test]
    fn 回车提交不把回车给应用() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        assert_eq!(
            engine.on_key(key(KEY_RETURN)),
            Action::Commit("nihao".into())
        );
        // 提交之后 buffer 空了，回车再按就该转发出去
        assert_eq!(engine.on_key(key(KEY_RETURN)), Action::Forward);
    }

    #[test]
    fn esc_取消组词且不转发() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        assert_eq!(
            engine.on_key(key(KEY_ESCAPE)),
            Action::UpdatePreedit(Preedit::default())
        );
        assert_eq!(engine.text(), "");
        // 取消之后再按空格：没内容，当普通空格转发
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Forward);
    }

    #[test]
    fn 没在组词时空格方向键数字都转发() {
        let mut engine = Engine::new();
        for keysym in [KEY_SPACE, KEY_DOWN, KEY_UP, KEY_TAB, 0x31] {
            assert_eq!(engine.on_key(key(keysym)), Action::Forward);
        }
    }

    #[test]
    fn 上屏之后那个键的抬起照样吃掉() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        // 真键盘上空格是 keycode 57
        let mut space = key(KEY_SPACE);
        space.keycode = 57;
        assert_eq!(engine.on_key(space), Action::Commit("你好".into()));
        // 抬起不能漏给应用：应用会收到"没按下过就直接抬起"
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..space
            }),
            Action::Swallow
        );
        // 再抬一次账本已经清了，转发
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..space
            }),
            Action::Forward
        );
    }

    #[test]
    fn 快捷键一律转发() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        let ctrl_a = KeyInput {
            shortcut: true,
            ..key(0x61)
        };
        assert_eq!(engine.on_key(ctrl_a), Action::Forward);
        // 预编辑串没被影响
        assert_eq!(engine.text(), "nihao");
    }

    #[test]
    fn 没有输入框时全部转发() {
        let mut engine = Engine::new();
        let inactive = KeyInput {
            active: false,
            ..key(0x61)
        };
        assert_eq!(engine.on_key(inactive), Action::Forward);
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 吃掉的键连抬起一起吃掉() {
        let mut engine = Engine::new();
        let mut pressed = key(0x61);
        pressed.keycode = 30;
        assert!(matches!(engine.on_key(pressed), Action::UpdatePreedit(_)));

        // 同一个 keycode 的抬起：吃掉
        let released = KeyInput {
            pressed: false,
            ..pressed
        };
        assert_eq!(engine.on_key(released), Action::Swallow);
        // 再抬一次就没有账可销了，转发出去
        assert_eq!(engine.on_key(released), Action::Forward);
    }

    #[test]
    fn 没吃过的键抬起要转发() {
        let mut engine = Engine::new();
        type_letters(&mut engine, "nihao");
        // Shift（keysym 0xffe1）从来没被吃掉，抬起该转发
        let shift_up = KeyInput {
            keysym: 0xffe1,
            pressed: false,
            ..key(0)
        };
        assert_eq!(engine.on_key(shift_up), Action::Forward);
    }

    #[test]
    fn reset_清空组词和账本() {
        let mut engine = Engine::new();
        let mut pressed = key(0x61);
        pressed.keycode = 30;
        engine.on_key(pressed);
        engine.reset();
        assert_eq!(engine.text(), "");
        assert_eq!(engine.preedit().selected, 0);
        // 账本也清了：抬起要转发
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..pressed
            }),
            Action::Forward
        );
    }
}
