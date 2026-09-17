//! 输入法引擎：只做"这个按键该干什么"的决定，完全不碰 Wayland。
//!
//! 引擎自己不管拼音怎么切、候选怎么排 —— 那些在 `scheme`（输入方案）和
//! `dict`（SQLite 词库）里。引擎只负责**按键的状态机**：
//!
//! ```text
//! 按键 ──► KeyInput ──► Engine ──► Action ──► 协议层照做
//!                        │
//!                        ├── scheme：这串键对应哪些码（全拼切音节 / 双拼解码 / 五笔码）
//!                        └── dict  ：这些码对应哪些词、谁排前面（词频 + 用户习惯）
//! ```
//!
//! 拆开的好处：换输入方案（全拼↔双拼↔五笔）改配置文件就行；加词、调权重改数据库就行；
//! 都不需要动这个文件。

pub mod config;
pub mod dict;
mod pinyin;
mod scheme;

pub use config::{Config, EXAMPLE as EXAMPLE_CONFIG, SchemeConfig};
pub use dict::Dict;
pub use scheme::{DoublePinyin, FullPinyin, Layout, Scheme, Table};

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

/// 中英文模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 中文：字母进组词 buffer，空格上屏候选
    #[default]
    Chinese,
    /// 英文：什么都不拦，按键原样转发（等于这台机器上没装输入法）
    English,
}

impl Mode {
    pub fn toggle(self) -> Mode {
        match self {
            Mode::Chinese => Mode::English,
            Mode::English => Mode::Chinese,
        }
    }

    /// 切换提示上显示的那个字
    pub fn label(self) -> &'static str {
        match self {
            Mode::Chinese => "中",
            Mode::English => "英",
        }
    }
}

/// 哪些按键用来切中英文
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToggleKeys {
    /// Ctrl + 空格（中文输入法的老习惯）
    pub ctrl_space: bool,
    /// 轻按一下 Shift（按下到抬起之间没按别的键）
    pub shift_tap: bool,
}

impl Default for ToggleKeys {
    fn default() -> Self {
        // 默认只开 Ctrl+空格：Shift 太容易误触了，想要的人自己开
        Self {
            ctrl_space: true,
            shift_tap: false,
        }
    }
}

impl ToggleKeys {
    /// 解析配置里的写法。写空数组就是不想要切换键
    pub fn parse(keys: &[String]) -> Result<Self, String> {
        let mut parsed = ToggleKeys {
            ctrl_space: false,
            shift_tap: false,
        };
        for key in keys {
            match key.trim().to_ascii_lowercase().as_str() {
                "ctrl+space" => parsed.ctrl_space = true,
                "shift" => parsed.shift_tap = true,
                other => {
                    return Err(format!(
                        "不认识的切换键 {other:?}：可以写 \"ctrl+space\" 或 \"shift\"，                         不想要切换键就写空数组"
                    ));
                }
            }
        }
        Ok(parsed)
    }

    pub fn enabled(&self) -> bool {
        self.ctrl_space || self.shift_tap
    }
}

/// 引擎的行为开关（都来自配置文件）
#[derive(Debug, Clone)]
pub struct Settings {
    /// 候选最多取几个
    pub limit: usize,
    /// 用哪些键切中英文
    pub toggle_keys: ToggleKeys,
    /// 启动时是中文还是英文
    pub start_mode: Mode,
    /// 切换模式时在光标处弹一下"中/英"
    pub indicator: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            limit: 9,
            toggle_keys: ToggleKeys::default(),
            start_mode: Mode::Chinese,
            indicator: true,
        }
    }
}

/// Shift 的 keysym（左右两个）
fn is_shift(keysym: u32) -> bool {
    keysym == 0xffe1 || keysym == 0xffe2
}

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
    /// 具体是 Ctrl 按着（用来认 Ctrl+空格；Alt+空格得留给应用）
    pub ctrl: bool,
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

/// 输入法引擎
pub struct Engine {
    /// 词库（SQLite）
    dict: Dict,
    /// 输入方案：全拼 / 双拼 / 码表
    scheme: Box<dyn Scheme>,
    /// 候选最多取几个
    limit: usize,
    /// 现在中文还是英文
    mode: Mode,
    /// 切换键
    toggle: ToggleKeys,
    /// 切换时要不要弹提示（协议层问它）
    indicator: bool,
    /// Shift 按下之后还没抬起来，且中间没按别的键（"轻按 Shift"用）
    shift_tap: bool,

    /// 攒着的东西：拼音，或者用户用 Shift/Caps Lock 敲出来的大写英文
    buffer: String,
    /// 选中第几个候选（打字过程中每来一个新字母都回到第一个）
    selected: usize,
    /// 被我们吃掉的按键：它们的抬起事件也得吃掉，
    /// 否则应用会收到"没按下就直接抬起"，修饰键状态可能错乱
    consumed: Vec<u32>,
}

impl Engine {
    pub fn new(dict: Dict, scheme: Box<dyn Scheme>, settings: Settings) -> Self {
        Self {
            dict,
            scheme,
            limit: settings.limit.max(1),
            mode: settings.start_mode,
            toggle: settings.toggle_keys,
            indicator: settings.indicator,
            shift_tap: false,
            buffer: String::new(),
            selected: 0,
            consumed: Vec::new(),
        }
    }

    /// 按配置文件建引擎：打开词库、装上方案
    pub fn from_config(config: &Config) -> dict::Result<Self> {
        let (dict, scheme) = config.build_engine_parts()?;
        Ok(Self::new(dict, scheme, config.settings()?))
    }

    /// 现在中文还是英文
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// 切换模式时要不要弹个提示（协议层问它）
    pub fn indicator(&self) -> bool {
        self.indicator
    }

    /// 现在用的是哪套方案（日志用）
    pub fn scheme_name(&self) -> &str {
        self.scheme.name()
    }

    /// 预编辑串原文（拼音 / 码）
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// 现在这一屏该显示什么
    pub fn preedit(&self) -> Preedit {
        Preedit {
            text: self.buffer.clone(),
            candidates: self.candidates(),
            selected: self.selected,
        }
    }

    /// 问方案要候选。词库查不到、或者这串键根本不是有效输入，就是空的
    fn candidates(&self) -> Vec<String> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        // 查库用的小写码；预编辑串保留用户敲的大小写
        self.scheme
            .candidates(&self.dict, &self.buffer.to_ascii_lowercase(), self.limit)
    }

    /// 输入框失焦：按住没放的键不会再有抬起事件了，账本一起清掉。
    /// 注意**不动 mode** —— 切到英文之后换个窗口，还是英文
    pub fn reset(&mut self) {
        self.clear_composing();
        self.consumed.clear();
        self.shift_tap = false;
    }

    /// 切中英文：顺手把正在组的词取消掉（不然预编辑会挂在应用里）,
    /// 协议层看到 mode 变了会去清预编辑、弹提示
    fn switch_mode(&mut self) {
        self.mode = self.mode.toggle();
        self.clear_composing();
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

        // ---- 中英文切换：放在最前面，两种模式下都得能用 ----
        //
        // 轻按 Shift：Shift 的按下和抬起之间没按过别的键，才算"轻按"。
        // Shift 自己照旧转发给应用（大写得靠它），只是抬起的时候顺手切个模式
        if self.toggle.shift_tap {
            if is_shift(key.keysym) {
                if key.pressed {
                    self.shift_tap = true;
                } else if std::mem::take(&mut self.shift_tap) {
                    self.switch_mode();
                }
            } else if key.pressed {
                self.shift_tap = false; // 中间按了别的键 → 是 Shift+A 这种组合，不是轻按
            }
        }

        // Ctrl+空格：切换模式，这个键谁都不给（它是输入法的热键）。
        // 注意必须认准 Ctrl：Alt+空格在很多应用里是窗口菜单
        if key.pressed && self.toggle.ctrl_space && key.ctrl && key.keysym == KEY_SPACE {
            self.switch_mode();
            self.consumed.push(key.keycode);
            return Action::Swallow;
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

        // 英文模式：什么都不拦。输入法这时候等于不存在，按键全部原样转发 ——
        // 打英文、写代码、按快捷键都不希望被组词吃掉
        if self.mode == Mode::English {
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

        // 能进组词的字符：一般就是 a-z，双拼方案可能还收分号。
        // 一律攒进 buffer 当预编辑 —— 包括 Shift、Caps Lock 打出来的大写。
        // 打字中途绝不往应用里塞字符：应用这时正处在预编辑状态，对"野生"字符的
        // 处理不可靠（实测转发 Shift+A 过去，输入框里 A 和 a 都不出现）
        if let Some(ch) = char::from_u32(key.keysym)
            && self.scheme.accepts(ch.to_ascii_lowercase())
        {
            self.buffer.push(ch);
            self.consumed.push(key.keycode);
            return self.changed();
        }

        match key.keysym {
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
                match self.candidates().get(index).cloned() {
                    Some(word) => {
                        self.pick(&word);
                        Action::Commit(word)
                    }
                    // 候选没那么多却按了这个数字：当没按过（但也不把数字塞进拼音）
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
        let count = self.candidates().len();
        if count == 0 {
            return Action::Forward;
        }
        self.selected = (self.selected as i32 + delta).rem_euclid(count as i32) as usize;
        Action::UpdatePreedit(self.preedit())
    }

    /// 上屏当前选中的候选
    fn commit_candidate(&mut self, keycode: u32) -> Action {
        let candidates = self.candidates();
        // 一个候选都没有（打的是词库里没有的东西，比如 `aaaa`）：原样上屏，
        // 不能提交空串 —— 那样用户打的字就凭空消失了
        let word = match candidates.get(self.selected).or_else(|| candidates.first()) {
            Some(word) => word.clone(),
            None => std::mem::take(&mut self.buffer),
        };
        self.consumed.push(keycode);
        if !word.is_empty() {
            self.pick(&word);
        } else {
            self.clear_composing();
        }
        Action::Commit(word)
    }

    /// 用户挑了一个词：记一笔，下次它排得更靠前。
    /// 这就是"用户使用频率权重"那半边，存在库里的 `user_word` 表
    fn pick(&mut self, word: &str) {
        if let Err(e) = self.dict.note_used(word) {
            eprintln!("pliers: 记用户词频失败：{e}");
        }
        self.clear_composing();
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
            ctrl: false,
            active: true,
        }
    }

    /// 小词库 + 全拼方案
    fn engine_with(settings: Settings) -> Engine {
        let dict = dict::testing::sample_dict();
        let scheme = FullPinyin::new(dict.syllables());
        Engine::new(dict, Box::new(scheme), settings)
    }

    fn engine() -> Engine {
        engine_with(Settings::default())
    }

    /// 造一个 Ctrl+空格（中英文切换键）
    fn ctrl_space() -> KeyInput {
        KeyInput {
            keysym: KEY_SPACE,
            ctrl: true,
            shortcut: true,
            ..key(KEY_SPACE)
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
        let mut engine = engine();
        assert_eq!(type_letters(&mut engine, "nihao").last().unwrap(), "nihao");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("你好".into()));
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 预编辑串逐字母长大() {
        let mut engine = engine();
        assert_eq!(
            type_letters(&mut engine, "nihao"),
            ["n", "ni", "nih", "niha", "nihao"]
        );
    }

    #[test]
    fn 打到一半就有候选了() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        // ni 的候选就是 你/尼/泥…
        let preedit = engine.preedit();
        assert_eq!(preedit.candidates[0], "你");
        assert!(preedit.candidates.contains(&"泥".to_string()));
        // 再打两个字母，「你好」才出现
        type_letters(&mut engine, "ha");
        assert!(
            engine.preedit().candidates.contains(&"你好".to_string()),
            "打到 niha 时该已经能看见 你好 了：{:?}",
            engine.preedit().candidates
        );
    }

    #[test]
    fn 候选框跟着输入长大() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        let preedit = engine.preedit();
        assert_eq!(preedit.text, "nihao");
        assert_eq!(preedit.candidates[0], "你好");
        assert_eq!(preedit.selected, 0);
        assert_eq!(preedit.current(), Some("你好"));
    }

    #[test]
    fn 没在组词时没有候选() {
        let mut engine = engine();
        assert!(engine.preedit().candidates.is_empty());
        type_letters(&mut engine, "n");
        engine.on_key(key(KEY_BACKSPACE));
        assert!(engine.preedit().candidates.is_empty());
    }

    #[test]
    fn 方向键换候选空格上屏选中的() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        let second = engine.preedit().candidates[1].clone();
        // ↓ 选中第二个
        assert_eq!(text_of(engine.on_key(key(KEY_DOWN))).unwrap(), "ni");
        assert_eq!(engine.preedit().selected, 1);
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit(second));
    }

    #[test]
    fn tab_也能翻候选() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        engine.on_key(key(KEY_TAB));
        engine.on_key(key(KEY_TAB));
        assert_eq!(engine.preedit().selected, 2);
        engine.on_key(key(KEY_ISO_LEFT_TAB));
        assert_eq!(engine.preedit().selected, 1);
        // ↑ 走到头会绕回最后一个
        engine.on_key(key(KEY_UP));
        engine.on_key(key(KEY_UP));
        assert_eq!(
            engine.preedit().selected,
            engine.preedit().candidates.len() - 1
        );
    }

    #[test]
    fn 数字直接选词上屏() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        let third = engine.preedit().candidates[2].clone();
        assert_eq!(engine.on_key(key(0x33)), Action::Commit(third)); // '3'
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 数字超出候选范围就什么都别发生() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        assert_eq!(engine.on_key(key(0x39)), Action::Swallow); // '9'
        assert_eq!(engine.text(), "ni"); // 没被提交，也没混进拼音
    }

    #[test]
    fn 退格之后选中回到第一个() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        engine.on_key(key(KEY_DOWN));
        assert_eq!(engine.preedit().selected, 1);
        engine.on_key(key(KEY_BACKSPACE));
        assert_eq!(engine.preedit().selected, 0);
    }

    #[test]
    fn 退格删掉一个字符() {
        let mut engine = engine();
        type_letters(&mut engine, "nihaox");
        assert_eq!(text_of(engine.on_key(key(KEY_BACKSPACE))).unwrap(), "nihao");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("你好".into()));
    }

    #[test]
    fn 查不到的词原样提交() {
        let mut engine = engine();
        type_letters(&mut engine, "abc");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("abc".into()));
    }

    #[test]
    fn 大写字母也待在预编辑里() {
        let mut engine = engine();
        type_letters(&mut engine, "aaa");
        // Shift+A：keysym 是 'A'（0x41），协议层翻译时就带上了修饰键的影响
        assert_eq!(text_of(engine.on_key(key(0x41))).unwrap(), "aaaA");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("aaaA".into()));
    }

    #[test]
    fn caps_lock_的大写照样能转拼音() {
        let mut engine = engine();
        // Caps Lock 打开：keysym 全是大写
        assert_eq!(type_letters(&mut engine, "NIHAO").last().unwrap(), "NIHAO");
        // 查表大小写不敏感
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("你好".into()));
    }

    #[test]
    fn 回车提交不把回车给应用() {
        let mut engine = engine();
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
        let mut engine = engine();
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
        let mut engine = engine();
        for keysym in [KEY_SPACE, KEY_DOWN, KEY_UP, KEY_TAB, 0x31] {
            assert_eq!(engine.on_key(key(keysym)), Action::Forward);
        }
    }

    #[test]
    fn 快捷键一律转发() {
        let mut engine = engine();
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
        let mut engine = engine();
        let inactive = KeyInput {
            active: false,
            ..key(0x61)
        };
        assert_eq!(engine.on_key(inactive), Action::Forward);
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 吃掉的键连抬起一起吃掉() {
        let mut engine = engine();
        let mut pressed = key(0x61);
        pressed.keycode = 30;
        assert!(matches!(engine.on_key(pressed), Action::UpdatePreedit(_)));

        let released = KeyInput {
            pressed: false,
            ..pressed
        };
        assert_eq!(engine.on_key(released), Action::Swallow);
        // 再抬一次就没有账可销了，转发出去
        assert_eq!(engine.on_key(released), Action::Forward);
    }

    #[test]
    fn 上屏之后那个键的抬起照样吃掉() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        let mut space = key(KEY_SPACE);
        space.keycode = 57;
        assert_eq!(engine.on_key(space), Action::Commit("你好".into()));
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..space
            }),
            Action::Swallow
        );
    }

    #[test]
    fn 没吃过的键抬起要转发() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        let shift_up = KeyInput {
            keysym: 0xffe1,
            pressed: false,
            ..key(0)
        };
        assert_eq!(engine.on_key(shift_up), Action::Forward);
    }

    #[test]
    fn reset_清空组词和账本() {
        let mut engine = engine();
        let mut pressed = key(0x61);
        pressed.keycode = 30;
        engine.on_key(pressed);
        engine.reset();
        assert_eq!(engine.text(), "");
        assert_eq!(engine.preedit().selected, 0);
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..pressed
            }),
            Action::Forward
        );
    }

    #[test]
    fn ctrl_空格切到英文() {
        let mut engine = engine();
        assert_eq!(engine.mode(), Mode::Chinese);

        // 切到英文：切换键本身不给应用
        assert_eq!(engine.on_key(ctrl_space()), Action::Swallow);
        assert_eq!(engine.mode(), Mode::English);

        // 英文模式下打什么都原样转发，一个字都不进组词
        for ch in "hello".chars() {
            assert_eq!(engine.on_key(key(ch as u32)), Action::Forward);
        }
        assert_eq!(engine.text(), "");
        assert!(engine.preedit().candidates.is_empty());
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Forward);

        // 再按一次切回中文，照常组词
        assert_eq!(engine.on_key(ctrl_space()), Action::Swallow);
        assert_eq!(engine.mode(), Mode::Chinese);
        type_letters(&mut engine, "ni");
        assert_eq!(engine.preedit().candidates[0], "你");
    }

    #[test]
    fn 切换的时候把正在组的词取消掉() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        engine.on_key(ctrl_space());
        assert_eq!(engine.text(), "", "切到英文时预编辑要清干净");
        assert!(engine.preedit().candidates.is_empty());
    }

    #[test]
    fn 切换键的抬起也吃掉() {
        let mut engine = engine();
        let mut pressed = ctrl_space();
        pressed.keycode = 57;
        assert_eq!(engine.on_key(pressed), Action::Swallow);
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..pressed
            }),
            Action::Swallow,
            "不然应用会收到没按下过的空格抬起"
        );
    }

    #[test]
    fn alt_空格不算切换键() {
        let mut engine = engine();
        // Alt+空格在很多应用里是窗口菜单，不能抢
        let alt_space = KeyInput {
            shortcut: true,
            ctrl: false,
            ..key(KEY_SPACE)
        };
        assert_eq!(engine.on_key(alt_space), Action::Forward);
        assert_eq!(engine.mode(), Mode::Chinese);
    }

    #[test]
    fn 轻按_shift_切换但组合键不算() {
        let settings = Settings {
            toggle_keys: ToggleKeys::parse(&["shift".to_string()]).unwrap(),
            ..Default::default()
        };
        let mut engine = engine_with(settings);
        let shift_up = KeyInput {
            pressed: false,
            ..key(0xffe1)
        };

        // 轻按一下：按下和抬起之间没别的东西 → 切换
        engine.on_key(key(0xffe1));
        engine.on_key(shift_up);
        assert_eq!(engine.mode(), Mode::English);

        // Shift+A 这种组合：中间按了 a → 不算轻按
        engine.on_key(key(0xffe1));
        engine.on_key(key(0x61));
        engine.on_key(shift_up);
        assert_eq!(engine.mode(), Mode::English, "组合键不该切成中文");
    }

    #[test]
    fn 切到英文之后换窗口还是英文() {
        let mut engine = engine();
        engine.on_key(ctrl_space());
        engine.reset(); // 焦点换到别的应用
        assert_eq!(engine.mode(), Mode::English, "模式不该被失焦重置");
    }

    #[test]
    fn 用户选过的词下次排前面() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        assert_eq!(engine.preedit().candidates[0], "你");
        engine.on_key(key(KEY_ESCAPE)); // 先取消这次组词，否则下面会接着往后打

        // 连选三次「泥」。注意每次都得重新找它在第几个 ——
        // 选过一次它就会往前挪，这正是要验的东西
        for _ in 0..3 {
            type_letters(&mut engine, "ni");
            let index = engine
                .preedit()
                .candidates
                .iter()
                .position(|word| word == "泥")
                .expect("候选中该有「泥」");
            engine.on_key(key(0x31 + index as u32)); // 数字键选它
        }

        type_letters(&mut engine, "ni");
        assert_eq!(
            engine.preedit().candidates[0],
            "泥",
            "选过三次的词该压过「你」排第一：{:?}",
            engine.preedit().candidates
        );
    }
}
