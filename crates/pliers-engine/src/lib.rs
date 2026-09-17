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
mod sentence;

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
/// ←
pub const KEY_LEFT: u32 = 0xff51;
/// ↑
pub const KEY_UP: u32 = 0xff52;
/// →
pub const KEY_RIGHT: u32 = 0xff53;
/// ↓
pub const KEY_DOWN: u32 = 0xff54;
/// PageUp / PageDown
pub const KEY_PAGE_UP: u32 = 0xff55;
pub const KEY_PAGE_DOWN: u32 = 0xff56;
/// `,` `.` 和 `-` `=`：也是翻页键（中文输入法的老习惯，微软拼音用的是 `-` `=`）
pub const KEY_COMMA: u32 = 0x2c;
pub const KEY_PERIOD: u32 = 0x2e;
pub const KEY_MINUS: u32 = 0x2d;
pub const KEY_EQUAL: u32 = 0x3d;

/// Del：把选中的候选"忘掉"（用户自己拼的句子能删，词库里的词只能清偏好）
pub const KEY_DELETE: u32 = 0xffff;

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
    /// 候选框一页显示几个
    pub limit: usize,
    /// 中文模式下把半角标点打成全角（`中文标点`）
    pub chinese_punctuation: bool,
    /// 一次准备多少个候选（翻页能翻多深）
    pub pool: usize,
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
            chinese_punctuation: true,
            pool: 90,
            toggle_keys: ToggleKeys::default(),
            start_mode: Mode::Chinese,
            indicator: true,
        }
    }
}

/// 中文标点：中文模式下这些半角符号打成全角（搜狗/fcitx 的常规映射）。
/// 没列进来的（`-` `=` `@` `#` `%` `&` `*` `/` 之类）照旧原样，
/// 其中 `-` `=` 还是翻页键
const CHINESE_PUNCTUATION: &[(u32, &str)] = &[
    (0x2c, "，"),
    (0x2e, "。"),
    (0x3f, "？"),
    (0x21, "！"),
    (0x3b, "；"),
    (0x3a, "："),
    (0x28, "（"),
    (0x29, "）"),
    (0x5b, "【"),
    (0x5d, "】"),
    (0x7b, "「"),
    (0x7d, "」"),
    (0x3c, "《"),
    (0x3e, "》"),
    (0x22, "“"),
    (0x27, "‘"),
    (0x5c, "、"),
    (0x5f, "——"),
    (0x5e, "……"),
    (0x24, "￥"),
    (0x7e, "～"),
];

/// 这个 keysym 对应的中文标点（没有就是 None）
fn chinese_punctuation(keysym: u32) -> Option<&'static str> {
    CHINESE_PUNCTUATION
        .iter()
        .find(|(code, _)| *code == keysym)
        .map(|(_, text)| *text)
}

/// Shift 的 keysym（左右两个）
fn is_shift(keysym: u32) -> bool {
    keysym == 0xffe1 || keysym == 0xffe2
}

/// 这个 keysym 是不是"能打出一个可见半角字符"的键（字母、数字、符号都算）。
/// 只看 0x20..=0x7e 这一段：符号（/ ; ' [ ] 0）和**大写字母**都在里面，
/// 而 F1、Home、方向键、以及 Shift_L(0xffe1)/Caps_Lock 这些**功能键**
/// 都在外面 —— 它们不该把正在组的词提前上屏
fn is_printable(keysym: u32) -> bool {
    (0x20..=0x7e).contains(&keysym)
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
    /// **当前这一页**的候选词。空 = 没在组词
    pub candidates: Vec<String>,
    /// 这一页里选中的是第几个（从 0 开始）
    pub selected: usize,
    /// 第几页（从 0 开始）和一共几页 —— 候选框靠它画右下角那个 "2/9"
    pub page: usize,
    pub pages: usize,
}

impl Preedit {
    /// 选中的那个候选词（空格上屏的就是它）
    pub fn current(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(String::as_str)
    }
}

/// 一个候选：要上屏的文字 + 它**吃掉输入里的几个字符**。
///
/// `consumed` 是"分段上屏"的关键：`nihaoma` 里「你好」只吃前 5 个字符，
/// 上屏之后 `ma` 还留在预编辑里接着选 —— 不用一次性把整串匹配完
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// 吃掉的字符数（按 `char` 数，不是字节）
    pub consumed: usize,
    /// 这是"用户自己拼过的话"（`user_phrase` 里的），不是词库里的词 —— 按 Del 能删
    pub learned: bool,
    /// 词库里**本来就有**这个词条（`word` 表命中）。
    /// 黑名单（`user_hidden`）只对"不是词库里的"候选生效 ——
    /// 词库里的词再怎么删也删不掉，不然一次误操作就永久少一个正常候选
    pub from_dict: bool,
}

impl Candidate {
    /// 把整串输入都吃掉（整词、整句都是这种）
    pub fn whole(text: impl Into<String>, input: &str) -> Self {
        Self {
            text: text.into(),
            consumed: input.chars().count(),
            learned: false,
            from_dict: false,
        }
    }

    /// 只吃掉前 `consumed` 个字符，剩下的留给下一轮
    pub fn partial(text: impl Into<String>, consumed: usize) -> Self {
        Self {
            text: text.into(),
            consumed,
            learned: false,
            from_dict: false,
        }
    }

    /// 用户自己拼出来的整句（按 Del 能删掉）
    pub fn learned(mut self) -> Self {
        self.learned = true;
        self
    }

    /// 词库里本来就有这个词条（Del 删不掉）
    pub fn from_dict(mut self) -> Self {
        self.from_dict = true;
        self
    }
}

/// 挑一个候选的结果：整串都吃掉了，还是只吃了一段
#[derive(Debug, Clone, PartialEq, Eq)]
enum Picked {
    Whole(String),
    /// 上屏 `text`，剩下的拼音接着组（`rest` 是新的预编辑快照）
    Part {
        text: String,
        rest: Preedit,
    },
}

/// 引擎的决定，协议层照着执行
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 组词状态变了：预编辑串 / 候选列表 / 选中的候选，都在这份快照里
    ///（没在组词时 `candidates` 为空，界面层就该把候选框收起来）
    UpdatePreedit(Preedit),
    /// 把这段文本提交给应用
    Commit(String),
    /// 先把这段文本上屏，再接着显示剩下的预编辑（分段上屏：选了「你好」还剩 `ma`）
    CommitAndContinue(String, Preedit),
    /// 先把这段文本提交给应用，再把这个按键原样转发（敲符号时用）。
    /// 顺序不能反：符号得排在上屏的文字后面，不然屏幕上会变成 ",你好"
    CommitAndForward(String),
    /// 在候选框那儿弹一句话（"删掉了「你好马」"之类），预编辑不动
    Notice(String),
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
    /// 候选框一页显示几个
    limit: usize,
    /// 一次准备多少个候选（翻页用）
    pool_size: usize,
    /// 候选池：一次多取一些，"翻页"就是在这个池子里挪游标，不用重新查库
    pool: Vec<Candidate>,
    /// 现在中文还是英文
    mode: Mode,
    /// 切换键
    toggle: ToggleKeys,
    /// 切换时要不要弹提示（协议层问它）
    indicator: bool,
    /// 中文标点：中文模式下把 `,` `.` `?` 这些打成 `，` `。` `？`
    chinese_punctuation: bool,
    /// Shift 按下之后还没抬起来，且中间没按别的键（"轻按 Shift"用）
    shift_tap: bool,

    /// 攒着的东西：要么是拼音（全小写），要么是英文原文（混进了大写字母）
    buffer: String,
    /// 选中的是**池子里**第几个候选（当前是第几页由它算出来）
    cursor: usize,
    /// 被我们吃掉的按键：它们的抬起事件也得吃掉，
    /// 否则应用会收到"没按下就直接抬起"，修饰键状态可能错乱
    consumed: Vec<u32>,

    /// 这次输入里用户已经挑过的部分（键 + 文字）：整句拼完之后存进词库，
    /// 下次敲同一串键就直接给这个句子
    picked_keys: String,
    picked_text: String,
    /// 挑过几段（挑过 2 段以上才算"他自己拼的句子"，值得记）
    picked_parts: usize,
}

impl Engine {
    pub fn new(dict: Dict, scheme: Box<dyn Scheme>, settings: Settings) -> Self {
        Self {
            dict,
            scheme,
            limit: settings.limit.max(1),
            pool_size: settings.pool.max(settings.limit.max(1)),
            pool: Vec::new(),
            picked_keys: String::new(),
            picked_text: String::new(),
            picked_parts: 0,
            mode: settings.start_mode,
            toggle: settings.toggle_keys,
            indicator: settings.indicator,
            chinese_punctuation: settings.chinese_punctuation,
            shift_tap: false,
            buffer: String::new(),
            cursor: 0,
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

    /// 直接定模式（换配置重建引擎之后，把用户原来在用的模式接上）。
    /// 顺手清掉组词状态 —— 换了方案，旧的拼音串已经没有意义
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.clear_composing();
    }

    /// 把"正在打的这串"塞回来（改配置重建引擎之后用：`pliers set` 不该把打到一半的拼音弄丢）。
    /// 候选会按当前方案重新查一遍
    pub fn set_text(&mut self, text: &str) {
        self.buffer.clear();
        self.buffer.push_str(text);
        self.cursor = 0;
        self.refresh_pool();
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

    /// 现在这一屏该显示什么：当前页的候选 + 这一页里选中的是第几个
    pub fn preedit(&self) -> Preedit {
        let page = self.page();
        let start = page * self.limit;
        let end = (start + self.limit).min(self.pool.len());
        Preedit {
            text: self.buffer.clone(),
            candidates: self.pool[start..end]
                .iter()
                .map(|candidate| candidate.text.clone())
                .collect(),
            selected: self.cursor.saturating_sub(start),
            page,
            // 池子是空的就算 0 页，候选框那边看到 candidates 为空就会收起来
            pages: self.pool.len().div_ceil(self.limit),
        }
    }

    /// 现在在第几页
    fn page(&self) -> usize {
        self.cursor / self.limit
    }

    /// 现在这一页的第一个候选在池子里的下标
    fn page_start(&self) -> usize {
        self.page() * self.limit
    }

    /// 这串是不是"英文原文"：里面混着用户按 Shift / Caps Lock 敲出来的大写字母。
    /// 是的话就不查词了 —— 空格/回车原样上屏，一个候选都不出
    fn literal(&self) -> bool {
        self.buffer.chars().any(|c| c.is_ascii_uppercase())
    }

    /// 问方案要候选。词库查不到、或者这串键根本不是有效输入，就是空的
    /// 查一次库，把候选池灌满（只在输入变了的时候调 ——
    /// 翻页/换选中都只是挪游标，不重新查）
    fn refresh_pool(&mut self) {
        if self.buffer.is_empty() || self.literal() {
            // 空串没什么好查的；带大写的串是英文原文（`nN`、`NIHAO`），
            // 更不能拿去匹配拼音 —— "打 NIHC 也蹦出词"就是这么来的
            self.pool.clear();
            return;
        }
        // 1) 用户自己拼过的整串（分段挑出来的句子）：这是他自己的选择，排最前
        self.pool = self
            .dict
            .phrases(&self.buffer, self.limit)
            .into_iter()
            .map(|text| Candidate::whole(text, &self.buffer).learned())
            .collect();
        // 2) 方案给的：整词、整句、以及"只匹配前面一段"的候选（带 consumed）
        for candidate in self
            .scheme
            .candidates(&self.dict, &self.buffer, self.pool_size)
        {
            if !self.pool.iter().any(|old| old.text == candidate.text) {
                self.pool.push(candidate);
            }
        }
        // 用户按 Del 删过的：这串键上不再出现。
        // 但**词库里本来就有**的候选不受影响 —— 那些删不掉（早先版本误删留下的黑名单
        // 也就自动失效了，不用手工清库）
        let hidden = self.dict.hidden(&self.buffer);
        if !hidden.is_empty() {
            self.pool
                .retain(|candidate| candidate.from_dict || !hidden.contains(&candidate.text));
        }
        self.pool.truncate(self.pool_size);
    }

    /// 输入框失焦：按住没放的键不会再有抬起事件了，账本一起清掉。
    /// 注意**不动 mode** —— 切到英文之后换个窗口，还是英文
    pub fn reset(&mut self) {
        self.abandon();
        self.consumed.clear();
        self.shift_tap = false;
    }

    /// 切中英文：正在组的词不能被丢掉，交出来让调用方上屏（返回 Some(原始拼音)）。
    /// 协议层看到 mode 变了会去清预编辑、弹提示
    fn switch_mode(&mut self) -> Option<String> {
        self.mode = self.mode.toggle();
        self.take_raw()
    }

    /// 放弃这次组词（Esc、切模式、失焦）：连"已经挑了几段"一起忘掉 ——
    /// 用户不要这句话了，没有道理再记进词库
    fn abandon(&mut self) {
        self.clear_composing();
        self.forget_picks();
    }

    /// 组词结束（上屏 / 取消）：只清组词状态，按键账本留着 ——
    /// 刚刚那个键（空格、数字、Esc）的**抬起**还得吃掉，不然应用会收到
    /// 一个"没按下过就抬起"的野按键
    fn clear_composing(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.pool.clear();
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
        // Shift 自己照旧转发给应用（大写得靠它），只是抬起的时候顺手切个模式。
        // 组词当中切模式的话，打了一半的字母要上屏（见下面 tapped）
        let mut tapped: Option<String> = None;
        if self.toggle.shift_tap {
            if is_shift(key.keysym) {
                if key.pressed {
                    self.shift_tap = true;
                } else if std::mem::take(&mut self.shift_tap) {
                    tapped = self.switch_mode();
                }
            } else if key.pressed {
                self.shift_tap = false; // 中间按了别的键 → 是 Shift+A 这种组合，不是轻按
            }
        }

        // Ctrl+空格：切换模式，这个键谁都不给（它是输入法的热键）。
        // 注意必须认准 Ctrl：Alt+空格在很多应用里是窗口菜单
        if key.pressed && self.toggle.ctrl_space && key.ctrl && key.keysym == KEY_SPACE {
            let leftover = self.switch_mode();
            self.consumed.push(key.keycode);
            // 组词当中按了切换键：先把打了一半的字母上屏，别让它们凭空消失
            return match leftover {
                Some(text) => Action::Commit(text),
                None => Action::Swallow,
            };
        }

        // 抬起事件：账本里有的继续吃掉（一次清光，长按会重复记账），
        // 账本里没有的原样转发给应用
        if !key.pressed {
            if self.consumed.contains(&key.keycode) {
                self.consumed.retain(|k| *k != key.keycode);
                return Action::Swallow;
            }
            // 轻按 Shift 切了模式、又有半截拼音：先上屏再放 Shift 抬起过去
            return match tapped {
                Some(text) => Action::CommitAndForward(text),
                None => Action::Forward,
            };
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

        // 能进组词的字符：**小写** a-z（双拼方案可能还收分号）。
        // 大写有自己的去处，见下面那段
        if let Some(ch) = char::from_u32(key.keysym)
            && !ch.is_ascii_uppercase()
            && self.scheme.accepts(ch.to_ascii_lowercase())
        {
            self.buffer.push(ch);
            self.consumed.push(key.keycode);
            return self.changed();
        }

        // 组词当中敲了大写字母（Shift+N、Caps Lock）：**不许上屏**。
        // `n` 之后按 Shift+N 不该变成「你N」—— 那半截拼音跟这个 N 一样，
        // 都是用户想打的英文原文。原样并进 buffer，整串不再匹配中文，
        // 空格/回车时原样上屏（nihao 之后按 Shift+A 就是 nihaoA）
        if composing
            && let Some(ch) = char::from_u32(key.keysym)
            && ch.is_ascii_uppercase()
        {
            self.buffer.push(ch);
            self.consumed.push(key.keycode);
            return self.changed();
        }

        // 已经在打英文原文了（buffer 里混进了大写）：后面敲的字母、数字、符号
        // 也一起并进来 —— 不然它们会被当成"野生字符"插到预编辑前面，
        // 数字更是会被当成选词直接吃掉。空格/回车才收尾
        if self.literal()
            && key.keysym != KEY_SPACE
            && is_printable(key.keysym)
            && let Some(ch) = char::from_u32(key.keysym)
        {
            self.buffer.push(ch);
            self.consumed.push(key.keycode);
            return self.changed();
        }

        // 中文标点：中文模式下 `,` `.` `?` 这些直接打成全角。
        // 组词当中还要先把候选上屏（`nihao,` → 「你好，」），一步到位 ——
        // 走的是 commit_string，不用转发原键，所以应用收到的一定是全角那个
        if self.chinese_punctuation
            && let Some(punctuation) = chinese_punctuation(key.keysym)
        {
            self.consumed.push(key.keycode);
            let word = if composing {
                self.take_whole()
            } else {
                String::new()
            };
            return Action::Commit(format!("{word}{punctuation}"));
        }

        match key.keysym {
            // 空格：把选中的候选交给应用（nihao → 你好）；没在组词就当普通空格
            KEY_SPACE if composing => self.commit_candidate(key.keycode),

            // 回车：把还没转换的内容原样提交，不把回车交给应用
            //（否则表单会被顺手提交掉）
            KEY_RETURN | KEY_KP_ENTER if composing => {
                self.consumed.push(key.keycode);
                let text = std::mem::take(&mut self.buffer);
                self.abandon();
                Action::Commit(text) // 账本不动：回车自己的抬起还得吃掉
            }

            // Esc：取消这次组词，这个键谁也不给（否则会顺手退出全屏、关掉弹窗）
            KEY_ESCAPE if composing => {
                self.consumed.push(key.keycode);
                self.abandon();
                Action::UpdatePreedit(Preedit::default())
            }

            // Del：把选中的候选"忘掉" —— 用户自己拼出来的整句能删掉；
            // 词库里真有的词只能清掉"我用过它"的偏好（词条是导入的，不该被删）
            KEY_DELETE if composing => {
                self.consumed.push(key.keycode);
                self.forget()
            }

            // 退格：删掉一个字符（候选列表跟着重算，选中回到第一个）
            KEY_BACKSPACE if composing => {
                self.consumed.push(key.keycode);
                self.buffer.pop();
                self.changed()
            }

            // ←↓→↑ / Tab：挪选中的候选。组词当中方向键不该去动输入框里的光标。
            // 挪出这一页会自动翻页（页是算出来的）
            KEY_DOWN | KEY_RIGHT | KEY_TAB if composing => {
                self.consumed.push(key.keycode);
                self.step(1)
            }

            // ← / ↑ / Shift+Tab：往回挪
            KEY_LEFT | KEY_UP | KEY_ISO_LEFT_TAB if composing => {
                self.consumed.push(key.keycode);
                self.step(-1)
            }

            // , - / PageUp：上一页；. = / PageDown：下一页。
            // 一页一页翻，页内位置保持（光标 +整页，取模绕圈）
            KEY_COMMA | KEY_MINUS | KEY_PAGE_UP if composing => {
                self.consumed.push(key.keycode);
                self.step(-(self.limit as i32))
            }

            KEY_PERIOD | KEY_EQUAL | KEY_PAGE_DOWN if composing => {
                self.consumed.push(key.keycode);
                self.step(self.limit as i32)
            }

            // 数字 1-9：直接选第几个候选上屏（拼音里的数字没法组词，吃掉不亏）
            KEY_1..=KEY_9 if composing => {
                let index = (key.keysym - KEY_1) as usize;
                self.consumed.push(key.keycode);
                // 数字选的是**这一页**的第几个：超出一页（`limit`）或者这一页没那么多个，
                // 都当没按过 —— 池子里后面的候选得先翻页才轮得到
                if index < self.limit && self.pool.get(self.page_start() + index).is_some() {
                    self.cursor = self.page_start() + index;
                    self.commit_candidate(key.keycode)
                } else {
                    Action::Swallow
                }
            }

            // 组词当中敲了别的字符（/ ; ' [ ] 0 ! ? 之类，以及大写字母 A-Z；
            // `,` `.` `-` `=` 被翻页占了）：先把选中的候选上屏，再把字符交给应用。
            // 顺序反了的话，符号会插在还没上屏的拼音前面 —— nihao, 会变成 ",你好"
            _ if composing && is_printable(key.keysym) => {
                Action::CommitAndForward(self.take_whole())
            }

            // 其他按键（F1、Home……）都交给应用
            _ => Action::Forward,
        }
    }

    /// 缓冲内容变了：候选列表跟着变，选中回到第一个。
    /// 打字过程中新出现的候选才是你要的，停在旧的行上没意义
    fn changed(&mut self) -> Action {
        self.cursor = 0;
        if self.buffer.is_empty() {
            // 删光了：这次输入不算数了
            self.forget_picks();
        }
        self.refresh_pool();
        Action::UpdatePreedit(self.preedit())
    }

    /// 选中项上下走一格（绕圈）
    fn step(&mut self, delta: i32) -> Action {
        if self.pool.is_empty() {
            return Action::Forward;
        }
        // 在池子里挪游标，绕圈。挪出这一页就会自动翻页（页是算出来的，不是存下来的）
        let count = self.pool.len() as i32;
        self.cursor = (self.cursor as i32 + delta).rem_euclid(count) as usize;
        Action::UpdatePreedit(self.preedit())
    }

    /// 上屏当前选中的候选（可能只吃掉前面一段：`nihaoma` 选「你好」还剩 `ma`）
    fn commit_candidate(&mut self, keycode: u32) -> Action {
        self.consumed.push(keycode);
        let picked = self.take_word();
        match picked {
            // 整串都吃掉了：这次组词结束
            Picked::Whole(text) => Action::Commit(text),
            // 只吃了一部分：上屏之后接着显示剩下的拼音和候选
            Picked::Part { text, rest } => Action::CommitAndContinue(text, rest),
        }
    }

    /// 结束这次组词，交出"该上屏的东西"：选中的候选；一个候选都没有
    ///（打的是词库里没有的东西，比如 `aaaa`）就用原始拼音 —— 不能交空串，
    /// 那样用户打的字就凭空消失了。挑中的词顺手记一笔词频
    fn take_word(&mut self) -> Picked {
        let Some(candidate) = self
            .pool
            .get(self.cursor)
            .or_else(|| self.pool.first())
            .cloned()
        else {
            // 一个候选都没有：原始拼音整串上屏
            let raw = std::mem::take(&mut self.buffer);
            self.clear_composing();
            return Picked::Whole(raw);
        };

        let consumed = candidate.consumed.min(self.buffer.chars().count());
        let rest: String = self.buffer.chars().skip(consumed).collect();
        let eaten: String = self.buffer.chars().take(consumed).collect();

        self.record_pick(&eaten, &candidate.text);
        self.note_pick(&candidate.text);

        if rest.is_empty() {
            self.clear_composing();
            self.remember_phrase();
            Picked::Whole(candidate.text)
        } else {
            // 剩下的接着组词：候选按剩下的那截重新查
            self.buffer = rest;
            self.cursor = 0;
            self.refresh_pool();
            Picked::Part {
                text: candidate.text,
                rest: self.preedit(),
            }
        }
    }

    /// Del：删掉选中的候选 —— **只删"自己拼出来的"**（`user_phrase` 里的）。
    ///
    /// 词库里原本就有的词、以及整句候选拼出来的词，都不动（回一句"删不了"）：
    /// 词库是导入出来的，那是别人整理的数据。
    ///
    /// 删的时候做三件事：
    /// 1. `user_phrase` 里那一行真删掉；
    /// 2. 清掉"我用过它"的偏好（`user_word`）；
    /// 3. 记进黑名单（`user_hidden`）—— 不然整句候选会立刻把同一个词拼回来，看着像没删掉。
    ///
    /// 删完把候选列表**重新显示出来**（选中回到第一个）：删没删掉一眼就能看见，
    /// 也不用担心"接着按 Del 又把下一个删了"
    fn forget(&mut self) -> Action {
        let Some(candidate) = self.pool.get(self.cursor).cloned() else {
            // 组词当中 Del 一律吃掉，**绝不转发**：不然它会漏给应用，
            // 在终端里就是一串 `^[[3~`（Del 的转义序列）
            return Action::Notice("没有能删的候选".to_string());
        };
        let text = candidate.text.clone();
        if !candidate.learned {
            return Action::Notice(format!("「{text}」不是自己拼的，删不了"));
        }

        if let Err(e) = self.dict.forget_phrase(&self.buffer, &text) {
            eprintln!("pliers: 删整句失败：{e}");
        }
        if let Err(e) = self.dict.forget_boost(&text) {
            eprintln!("pliers: 清偏好失败：{e}");
        }
        if let Err(e) = self.dict.hide(&self.buffer, &text) {
            eprintln!("pliers: 记黑名单失败：{e}");
        }

        // 列表按新的库重排，选中回到第一个，然后**把候选框贴回来**
        self.refresh_pool();
        self.cursor = 0;
        Action::UpdatePreedit(self.preedit())
    }

    /// 把**整串**收掉再上屏：符号、大写字母走这条路时不能留半截拼音在预编辑里
    ///（那个符号已经先发给应用了）。选中的候选要是只吃了一段，就退而求其次挑
    /// 池子里第一个"整串候选"，一个都没有就原样上屏
    fn take_whole(&mut self) -> String {
        let chars = self.buffer.chars().count();
        let picked = self
            .pool
            .get(self.cursor)
            .filter(|candidate| candidate.consumed >= chars)
            .or_else(|| {
                self.pool
                    .iter()
                    .find(|candidate| candidate.consumed >= chars)
            })
            .cloned();
        match picked {
            Some(candidate) => {
                self.note_pick(&candidate.text);
                self.abandon();
                candidate.text
            }
            None => {
                let raw = std::mem::take(&mut self.buffer);
                self.abandon();
                raw
            }
        }
    }

    /// 记住"用户在这串键上挑过哪些段"：整串拼完（候选都上屏了）就存进词库，
    /// 下次敲同一串键直接把这句话给他
    fn record_pick(&mut self, eaten: &str, text: &str) {
        self.picked_keys.push_str(eaten);
        self.picked_text.push_str(text);
        self.picked_parts += 1;
    }

    /// 这次输入结束了：段数够多就记下来（只挑了一段的话词库里本来就有，记了也是噪音）
    fn remember_phrase(&mut self) {
        if self.picked_parts >= 2
            && !self.picked_keys.is_empty()
            && !self.picked_text.is_empty()
            && let Err(e) = self.dict.note_phrase(&self.picked_keys, &self.picked_text)
        {
            eprintln!("pliers: 记整句失败：{e}");
        }
        self.forget_picks();
    }

    fn forget_picks(&mut self) {
        self.picked_keys.clear();
        self.picked_text.clear();
        self.picked_parts = 0;
    }

    /// 放弃这次组词，但把"打了一半的原始字母"交出来（调用方负责上屏）。
    /// 返回 None 表示刚才没在组词。切模式、失焦时用来救场
    pub fn take_raw(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.buffer);
        self.abandon();
        (!text.is_empty()).then_some(text)
    }

    /// 用户挑了一个词：记一笔，下次它排得更靠前。
    /// 这就是"用户使用频率权重"那半边，存在库里的 `user_word` 表
    fn note_pick(&self, word: &str) {
        if let Err(e) = self.dict.note_used(word) {
            eprintln!("pliers: 记用户词频失败：{e}");
        }
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
        let scheme = FullPinyin::new(dict.syllables(), true);
        Engine::new(dict, Box::new(scheme), settings)
    }

    fn engine() -> Engine {
        engine_with(Settings::default())
    }

    /// 关掉中文标点的引擎（`engine()` 默认是开着的）
    fn no_punct_engine() -> Engine {
        engine_with(Settings {
            chinese_punctuation: false,
            ..Settings::default()
        })
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

    /// 在候选里挑某个词（按下它对应的数字键）
    fn pick(engine: &mut Engine, text: &str) -> Action {
        let index = engine
            .preedit()
            .candidates
            .iter()
            .position(|candidate| candidate == text)
            .unwrap_or_else(|| panic!("候选里没有 {text:?}：{:?}", engine.preedit().candidates));
        engine.on_key(key(0x31 + index as u32))
    }

    #[test]
    fn 分段上屏_先选前面一段再接着选() {
        // nihaoma：词库里没有「你好吗」，但「你好」匹配了前 5 个字符 ——
        // 选它之后 ma 还留在预编辑里，接着选「吗」
        let mut engine = engine();
        type_letters(&mut engine, "nihaoma");
        let candidates = engine.preedit().candidates.clone();
        assert_eq!(candidates[0], "你好吗", "整句候选排前面：{candidates:?}");
        assert!(candidates.contains(&"你好".to_string()), "{candidates:?}");

        // 挑「你好」：上屏 + 剩下的接着组词
        let Action::CommitAndContinue(text, rest) = pick(&mut engine, "你好") else {
            panic!("该是分段上屏");
        };
        assert_eq!(text, "你好");
        assert_eq!(rest.text, "ma", "剩下的该是 ma");
        assert_eq!(engine.text(), "ma");
        assert!(!engine.preedit().candidates.is_empty(), "剩下的也得有候选");

        // 剩下的接着选：吗
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("吗".into()));
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 分段可以一直细到单字() {
        // nihaoma：除了一整段「你好」，也该给单字的「你」——
        // 挑它就上屏「你」，剩下的 haoma 接着组
        let mut engine = engine();
        type_letters(&mut engine, "nihaoma");
        let candidates = engine.preedit().candidates.clone();
        assert!(candidates.contains(&"你好".to_string()), "{candidates:?}");
        assert!(candidates.contains(&"你".to_string()), "{candidates:?}");

        let Action::CommitAndContinue(text, rest) = pick(&mut engine, "你") else {
            panic!("该是分段上屏");
        };
        assert_eq!(text, "你");
        assert_eq!(rest.text, "haoma", "剩下的接着组");
        // 剩下的还能继续一个字一个字挑
        assert!(
            rest.candidates.contains(&"好".to_string()),
            "{:?}",
            rest.candidates
        );
    }

    #[test]
    fn 分段要找得到中间那一段的词() {
        // 回归：`nihaomaneng` 里 `ni hao ma ne`、`ni hao man` 都查不到词，
        // 「你好」在再短一档 —— 以前只看最长的两档，结果只剩单字「你」
        let mut engine = engine();
        type_letters(&mut engine, "nihaomaneng");
        let candidates = engine.preedit().candidates.clone();
        assert!(candidates.contains(&"你好".to_string()), "{candidates:?}");
        assert!(candidates.contains(&"你".to_string()), "{candidates:?}");

        // 挑「你好」：上屏两个字符，剩下的接着组
        let Action::CommitAndContinue(text, rest) = pick(&mut engine, "你好") else {
            panic!("该是分段上屏");
        };
        assert_eq!(text, "你好");
        assert_eq!(rest.text, "maneng");
    }

    #[test]
    fn 黑名单不该盖住词库里的词() {
        // 模拟老版本误删留下的黑名单：词库里的「你好」被记了一笔 ——
        // 现在的规则是"词库里的词删不掉"，所以这一笔不该生效
        let mut engine = engine();
        engine.dict.hide("nihao", "你好").unwrap();
        engine.set_text("nihao");
        assert!(
            engine.preedit().candidates.contains(&"你好".to_string()),
            "词库里的词不该被黑名单挡住：{:?}",
            engine.preedit().candidates
        );
    }

    #[test]
    fn 长串输入也能细到第一个字() {
        // 四个音节：整句/整词候选再长，也总得给"第一个字"，不然只能整词整词地选
        let mut engine = engine();
        type_letters(&mut engine, "nihaobuneng");
        let candidates = engine.preedit().candidates.clone();
        assert!(candidates.contains(&"你".to_string()), "{candidates:?}");

        let Action::CommitAndContinue(text, rest) = pick(&mut engine, "你") else {
            panic!("该是分段上屏");
        };
        assert_eq!(text, "你");
        assert_eq!(rest.text, "haobuneng", "后面的原样留着");
    }

    #[test]
    fn 分段拼出来的句子记进词库() {
        let mut engine = engine();
        type_letters(&mut engine, "nihaoma");
        assert!(matches!(
            pick(&mut engine, "你好"),
            Action::CommitAndContinue(..)
        ));
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("吗".into()));

        // 记下来了：键就是他敲的那串
        assert_eq!(engine.dict.phrases("nihaoma", 9), ["你好吗"]);

        // 再敲同一串键，这句直接排第一
        engine.set_text("nihaoma");
        assert_eq!(engine.preedit().candidates[0], "你好吗");
    }

    #[test]
    fn 按_del_删掉自己拼的句子() {
        let mut engine = engine();
        // 先拼出「你好马」（挑两段），它会记进 user_phrase
        type_letters(&mut engine, "nihaoma");
        assert!(matches!(
            pick(&mut engine, "你好"),
            Action::CommitAndContinue(..)
        ));
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("吗".into()));
        assert_eq!(engine.dict.phrases("nihaoma", 9), ["你好吗"]);

        // 再打一遍：第一条就是它，按 Del 删掉
        engine.set_text("nihaoma");
        assert_eq!(engine.preedit().candidates[0], "你好吗");
        let Action::UpdatePreedit(preedit) = engine.on_key(key(KEY_DELETE)) else {
            panic!("删完该把候选列表贴回来");
        };
        assert_eq!(preedit.text, "nihaoma", "预编辑不受影响");
        assert_eq!(preedit.selected, 0, "选中回到第一个");
        assert!(engine.dict.phrases("nihaoma", 9).is_empty(), "库里该没了");
        // 关键：整句候选会拼出同一个词，所以还得进黑名单，列表里才真的看不到它
        assert!(
            !preedit.candidates.contains(&"你好吗".to_string()),
            "删掉之后不该再出现：{:?}",
            preedit.candidates
        );
        assert_eq!(engine.dict.hidden("nihaoma"), ["你好吗"]);
    }

    #[test]
    fn 组词时_del_绝不转发() {
        // Del 漏给应用的话，终端里会冒出一串 `^[[3~`
        let mut typing = engine();
        type_letters(&mut typing, "qqq"); // 一个候选都没有
        assert!(matches!(typing.on_key(key(KEY_DELETE)), Action::Notice(_)));
        // 没在组词时照旧是应用的键
        let mut idle = engine();
        assert_eq!(idle.on_key(key(KEY_DELETE)), Action::Forward);
    }

    #[test]
    fn 按_del_删不掉词库里的词() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        let Action::Notice(message) = engine.on_key(key(KEY_DELETE)) else {
            panic!("该回一句提示");
        };
        assert!(message.contains("删不了"), "{message}");
        // 候选一个不少、库里也没记黑名单
        assert_eq!(engine.preedit().candidates[0], "你好");
        assert!(engine.dict.hidden("nihao").is_empty());
    }

    #[test]
    fn 删完的句子换一次输入还是不再出现() {
        let mut engine = engine();
        type_letters(&mut engine, "nihaoma");
        assert!(matches!(
            pick(&mut engine, "你好"),
            Action::CommitAndContinue(..)
        ));
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("吗".into()));

        engine.set_text("nihaoma");
        assert_eq!(engine.preedit().candidates[0], "你好吗");
        assert!(matches!(
            engine.on_key(key(KEY_DELETE)),
            Action::UpdatePreedit(_)
        ));
        // 重新打一遍（相当于重新查库）：黑名单还在，整句候选也拼不回来
        engine.set_text("nihaoma");
        assert!(!engine.preedit().candidates.contains(&"你好吗".to_string()));
    }

    #[test]
    fn 没在组词时_del_是应用的键() {
        // 这个时候 Del 归应用（删它自己的字）；组词当中则一律吃掉，见上一个测试
        let mut engine = engine();
        assert_eq!(engine.on_key(key(KEY_DELETE)), Action::Forward);
    }

    #[test]
    fn 只挑一段不记整句() {
        // 只挑了一次（整串命中）没什么好记的：词库里本来就有
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("你好".into()));
        assert!(engine.dict.phrases("nihao", 9).is_empty());
    }

    #[test]
    fn 放弃组词就不记了() {
        // 挑了一段之后按 Esc：这段不该被记成"用户拼的句子"
        let mut typed = engine();
        type_letters(&mut typed, "nihaoma");
        assert!(matches!(
            pick(&mut typed, "你好"),
            Action::CommitAndContinue(..)
        ));
        typed.on_key(key(KEY_ESCAPE));
        assert!(typed.dict.phrases("nihaoma", 9).is_empty());

        // 删光也同理
        let mut erased = engine();
        type_letters(&mut erased, "nihaoma");
        assert!(matches!(
            pick(&mut erased, "你好"),
            Action::CommitAndContinue(..)
        ));
        erased.on_key(key(KEY_BACKSPACE));
        erased.on_key(key(KEY_BACKSPACE));
        assert!(erased.dict.phrases("nihaoma", 9).is_empty());
    }

    #[test]
    fn 整句候选_库里的词拼出长句() {
        // nihaoma：词库里没有「你好吗」这个词，靠「你好」+「吗」拼出来
        let mut engine = engine();
        type_letters(&mut engine, "nihaoma");
        assert!(
            engine.preedit().candidates.contains(&"你好吗".to_string()),
            "{:?}",
            engine.preedit().candidates
        );
    }

    #[test]
    fn 小鹤双拼也能整句() {
        // ni=nihc？不：小鹤里 ni 就是 `ni`，hao 是 `hc`，ma 是 `ma`
        let dict = dict::testing::sample_dict();
        let scheme = DoublePinyin::new(Layout::preset("flypy").unwrap(), dict.syllables(), true);
        let mut engine = Engine::new(dict, Box::new(scheme), Settings::default());
        type_letters(&mut engine, "nihcma");
        assert!(
            engine.preedit().candidates.contains(&"你好吗".to_string()),
            "{:?}",
            engine.preedit().candidates
        );
    }

    #[test]
    fn 整句候选可以关掉() {
        let config = Config::parse("[scheme]\nkind = \"full-pinyin\"\nsentence = false\n").unwrap();
        let dict = dict::testing::sample_dict();
        let scheme = config.build_scheme(&dict).unwrap();
        let texts = |scheme: &Box<dyn Scheme>| -> Vec<String> {
            scheme
                .candidates(&dict, "nihaoma", 9)
                .into_iter()
                .map(|candidate| candidate.text)
                .collect()
        };
        let got = texts(&scheme);
        assert!(!got.contains(&"你好吗".to_string()), "{got:?}");

        // 开着的时候有（对照，免得哪天默默失效）
        let config = Config::parse("[scheme]\nkind = \"full-pinyin\"\n").unwrap();
        let scheme = config.build_scheme(&dict).unwrap();
        let got = texts(&scheme);
        assert!(got.contains(&"你好吗".to_string()), "{got:?}");
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
    fn 候选多了会分页() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        let preedit = engine.preedit();
        assert_eq!(preedit.candidates.len(), 9, "一页 9 个");
        assert_eq!(preedit.page, 0);
        assert_eq!(preedit.pages, 3, "词库里 ni 有 20 个候选 → 3 页");
    }

    #[test]
    fn 挪出这一页会自动翻页() {
        // 注意先建 fresh：局部变量 engine 会把同名的测试 helper 遮蔽掉
        let mut fresh = engine();
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        // 按 9 次 → 正好跨到第 2 页的第一个
        for _ in 0..9 {
            engine.on_key(key(KEY_RIGHT));
        }
        let preedit = engine.preedit();
        assert_eq!(preedit.page, 1);
        assert_eq!(preedit.selected, 0);
        // 从第一页往回挪：绕到最后一页的最后一个
        type_letters(&mut fresh, "ni");
        fresh.on_key(key(KEY_LEFT));
        let preedit = fresh.preedit();
        assert_eq!(preedit.page, 2, "20 个候选、每页 9 个 → 最后一页是第 3 页");
        assert_eq!(preedit.candidates.len(), 2, "最后一页剩 2 个");
        assert_eq!(preedit.selected, 1, "绕回来选的是最后一个");
    }

    #[test]
    fn 逗号句号整页翻() {
        // 中文标点关掉时 `,` `.` 才是翻页键（打开时它们是标点，见另一个测试）
        let mut engine = no_punct_engine();
        type_letters(&mut engine, "ni");
        let first = engine.preedit().candidates[0].clone();
        // `.` 下一页，页内位置保持（都停在第 1 个）
        assert!(matches!(
            engine.on_key(key(KEY_PERIOD)),
            Action::UpdatePreedit(_)
        ));
        let preedit = engine.preedit();
        assert_eq!(preedit.page, 1);
        assert_eq!(preedit.selected, 0);
        assert_ne!(preedit.candidates[0], first);
        // `,` 翻回去
        engine.on_key(key(KEY_COMMA));
        assert_eq!(engine.preedit().page, 0);

        // 这几个键都是翻页：- = （微软拼音的习惯）、, . 、PgUp/PgDn。
        // 从第 0 页开始，一路按下来应该这么走
        for (pressed, want_page) in [
            (KEY_EQUAL, 1),
            (KEY_MINUS, 0),
            (KEY_PAGE_DOWN, 1),
            (KEY_PAGE_UP, 0),
            (KEY_PERIOD, 1),
            (KEY_COMMA, 0),
        ] {
            engine.on_key(key(pressed));
            assert_eq!(
                engine.preedit().page,
                want_page,
                "按 {} 之后该在第 {} 页",
                pressed,
                want_page
            );
        }
    }

    #[test]
    fn 数字选的是这一页的第几个() {
        let mut engine = no_punct_engine();
        type_letters(&mut engine, "ni");
        let first_page_first = engine.preedit().candidates[0].clone();
        engine.on_key(key(KEY_PERIOD)); // 翻到第 2 页
        let second_page_first = engine.preedit().candidates[0].clone();
        assert_ne!(first_page_first, second_page_first);
        // 按 1 上屏的是这一页的第 1 个，不是整池子的第 1 个
        assert_eq!(engine.on_key(key(0x31)), Action::Commit(second_page_first));
    }

    #[test]
    fn 上下左右都能挪选中() {
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        engine.on_key(key(KEY_DOWN));
        assert_eq!(engine.preedit().selected, 1);
        engine.on_key(key(KEY_RIGHT));
        assert_eq!(engine.preedit().selected, 2);
        engine.on_key(key(KEY_UP));
        assert_eq!(engine.preedit().selected, 1);
        engine.on_key(key(KEY_LEFT));
        assert_eq!(engine.preedit().selected, 0);
    }

    #[test]
    fn 候选不够一页就没有页码() {
        // 一页放 20 个：`ni` 的候选（单字 + 分段）也就十来个，够一页
        let settings = Settings {
            limit: 20,
            ..Settings::default()
        };
        let mut engine = engine_with(settings);
        type_letters(&mut engine, "ni");
        let preedit = engine.preedit();
        assert_eq!(preedit.pages, 1);
        assert_eq!(preedit.page, 0);
        assert_eq!(preedit.candidates[0], "你");
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
        // 一页只放 3 个：`ni` 的候选不止 3 个，按 9 该当没按过
        let settings = Settings {
            limit: 3,
            ..Settings::default()
        };
        let mut engine = engine_with(settings);
        type_letters(&mut engine, "ni");
        assert_eq!(engine.preedit().candidates.len(), 3);
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
    fn 大写字母是英文字符不进组词() {
        // 回归：以前大写会被 to_ascii_lowercase() 之后拿去查库，
        // 敲 NIHC 一路蹦出「你」「你好」，Caps Lock 打开跟没开一样
        let mut engine = engine();
        for keysym in [0x4e, 0x49, 0x48, 0x43] {
            // N I H C
            assert_eq!(engine.on_key(key(keysym)), Action::Forward);
            assert_eq!(engine.text(), "", "一个大写字母都不该进预编辑");
            assert!(engine.preedit().candidates.is_empty(), "更不该出候选");
        }
    }

    #[test]
    fn 组词中敲大写不上屏而是并进预编辑() {
        // 回归：以前 `n` 之后按 Shift+N 会先上屏「你」再补个 N，变成"你N"。
        // 那半截拼音跟这个 N 都是英文原文，该原样待在预编辑里
        let mut engine = engine();
        assert_eq!(text_of(engine.on_key(key(0x6e))).unwrap(), "n");
        let action = engine.on_key(key(0x4e)); // Shift 或 Caps Lock 打出来的 N
        assert_eq!(
            text_of(action).unwrap(),
            "nN",
            "大写并进预编辑，不当候选上屏"
        );
        assert!(
            engine.preedit().candidates.is_empty(),
            "掺了大写就是英文原文，一个候选都不许出"
        );
        // 空格把这串原样上屏，不做任何转换
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("nN".into()));
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 英文原文串里后面的字符也并进预编辑() {
        let mut engine = engine();
        for keysym in [0x6e, 0x4e, 0x32, 0x2f] {
            // n N 2 /
            engine.on_key(key(keysym));
        }
        assert_eq!(engine.text(), "nN2/");
        assert!(engine.preedit().candidates.is_empty());
        assert_eq!(
            engine.on_key(key(KEY_SPACE)),
            Action::Commit("nN2/".into()),
            "数字和符号也得留在串里，不能被当成选词吃掉"
        );
    }

    #[test]
    fn 大写之后接着打小写也还是英文原文() {
        let mut engine = engine();
        for ch in "nNihao".chars() {
            engine.on_key(key(ch as u32));
        }
        assert_eq!(engine.text(), "nNihao");
        assert!(engine.preedit().candidates.is_empty(), "不再回头匹配中文");
        assert_eq!(
            engine.on_key(key(KEY_SPACE)),
            Action::Commit("nNihao".into())
        );
    }

    #[test]
    fn 没在组词时大写照旧直接转发() {
        // 没有半截拼音挂着，大写字母直接转给应用最省事（Caps Lock 打英文）
        let mut engine = engine();
        for ch in "NIHAO".chars() {
            assert_eq!(engine.on_key(key(ch as u32)), Action::Forward);
        }
        assert_eq!(engine.text(), "", "不该起一个预编辑");
        assert!(engine.preedit().candidates.is_empty());
    }

    #[test]
    fn 大写字母的抬起不会被吃掉() {
        // 没在组词 → 大写走转发；它的抬起也必须转发，不然应用那边按键状态错乱
        let mut engine = engine();
        assert_eq!(engine.on_key(key(0x4e)), Action::Forward);
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..key(0x4e)
            }),
            Action::Forward
        );
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
    fn 切换的时候正在组的词先上屏() {
        // 回归：以前切中英文直接 clear_composing()，打了一半的 nihao 就凭空消失了
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        assert_eq!(
            engine.on_key(ctrl_space()),
            Action::Commit("nihao".into()),
            "打了一半的字母要先上屏（原始拼音，不做转换）"
        );
        assert_eq!(engine.mode(), Mode::English);
        assert_eq!(engine.text(), "", "上屏之后预编辑要清干净");
        assert!(engine.preedit().candidates.is_empty());
    }

    #[test]
    fn 轻按_shift_切换时半截拼音也上屏且_shift_抬起照旧转发() {
        let settings = Settings {
            toggle_keys: ToggleKeys::parse(&["shift".to_string()]).unwrap(),
            ..Default::default()
        };
        let mut engine = engine_with(settings);
        type_letters(&mut engine, "ni");
        engine.on_key(key(0xffe1)); // Shift 按下
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..key(0xffe1)
            }),
            Action::CommitAndForward("ni".into()),
            "先上屏，再把 Shift 抬起转给应用"
        );
        assert_eq!(engine.mode(), Mode::English);
        assert_eq!(engine.text(), "");
    }

    #[test]
    fn 中文标点_组词中一边上屏一边打全角() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        // `,` 在中文标点打开时是标点（不是翻页键）：候选和标点一起上屏
        assert_eq!(
            engine.on_key(key(0x2c)),
            Action::Commit("你好，".into()),
            "该一步到位，不用转发原键"
        );
        assert_eq!(engine.text(), "");
        // 键的抬起照样吃掉（按下没转给应用，抬起也不能转）
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..key(0x2c)
            }),
            Action::Swallow
        );
    }

    #[test]
    fn 中文标点_没组词时也打全角() {
        let mut engine = engine();
        for (keysym, want) in [
            (0x2c, "，"),
            (0x2e, "。"),
            (0x3f, "？"),
            (0x21, "！"),
            (0x3b, "；"),
            (0x3a, "："),
            (0x28, "（"),
            (0x5f, "——"),
            (0x5e, "……"),
            (0x24, "￥"),
        ] {
            assert_eq!(engine.on_key(key(keysym)), Action::Commit(want.into()));
        }
        // 表里没有的照旧转发
        assert_eq!(engine.on_key(key(0x2d)), Action::Forward); // '-'
        assert_eq!(engine.on_key(key(0x40)), Action::Forward); // '@'
    }

    #[test]
    fn 关掉中文标点就还是半角() {
        let settings = Settings {
            chinese_punctuation: false,
            ..Default::default()
        };
        let mut engine = engine_with(settings);
        // 组词中 `,` 还是翻页键
        type_letters(&mut engine, "nihao");
        assert!(matches!(engine.on_key(key(0x2c)), Action::UpdatePreedit(_)));
        // 没组词时标点原样转发
        let mut idle = no_punct_engine();
        assert_eq!(idle.on_key(key(0x2c)), Action::Forward);
        assert_eq!(idle.on_key(key(0x3f)), Action::Forward);
    }

    #[test]
    fn 英文模式不换标点() {
        let mut engine = engine();
        engine.on_key(ctrl_space()); // 切英文
        assert_eq!(engine.on_key(key(0x2c)), Action::Forward);
    }

    #[test]
    fn 组词中敲符号先上屏再转发() {
        // 回归：以前符号直接转发，屏幕上会变成 ",你好"
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        assert_eq!(
            engine.on_key(key(0x2f)), // '/'
            Action::CommitAndForward("你好".into())
        );
        assert_eq!(engine.text(), "", "符号一来，这次组词就结束了");
        assert!(engine.preedit().candidates.is_empty());

        // 符号自己的抬起没有进账本 —— 按下抬起都要原样给应用，不然插不进字符
        assert_eq!(
            engine.on_key(KeyInput {
                pressed: false,
                ..key(0x2f)
            }),
            Action::Forward
        );
    }

    #[test]
    fn 组词中敲符号上屏的是选中的那个候选() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        engine.on_key(key(KEY_DOWN)); // 换到第二个候选
        let second = engine.preedit().candidates[1].clone();
        assert_eq!(
            engine.on_key(key(0x30)), // '0'：不选词，当符号
            Action::CommitAndForward(second)
        );
    }

    #[test]
    fn 功能键不会把拼音提前上屏() {
        // F1 / Home / Insert 这些键不产生字符，照旧原样转发，组词不受影响
        //（Del 有自己的活儿：删自己拼的词，见下面那个测试）
        let mut engine = engine();
        type_letters(&mut engine, "ni");
        for keysym in [0xffbe, 0xff50, 0xff63] {
            assert_eq!(engine.on_key(key(keysym)), Action::Forward);
            assert_eq!(engine.text(), "ni");
        }
    }

    #[test]
    fn 没在组词时符号照旧直接转发() {
        let mut engine = engine();
        assert_eq!(engine.on_key(key(0x2f)), Action::Forward);
        assert_eq!(engine.on_key(key(0x30)), Action::Forward);
    }

    #[test]
    fn 切模式时没在组词就悄悄吃掉切换键() {
        let mut engine = engine();
        assert_eq!(engine.on_key(ctrl_space()), Action::Swallow);
        assert_eq!(engine.mode(), Mode::English);
    }

    #[test]
    fn take_raw_交出半截拼音并清干净() {
        let mut engine = engine();
        type_letters(&mut engine, "nihao");
        assert_eq!(engine.take_raw().as_deref(), Some("nihao"));
        assert_eq!(engine.text(), "");
        assert_eq!(engine.take_raw(), None, "没在组词时返回 None");
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
    fn 小鹤双拼打不能() {
        // 回归：以前 `ng` 这个码被「嗯」占着，bung 解成 `bu ng`，怎么打都出不来「不能」
        let dict = dict::testing::sample_dict();
        let scheme = DoublePinyin::new(Layout::preset("flypy").unwrap(), dict.syllables(), true);
        let mut engine = Engine::new(dict, Box::new(scheme), Settings::default());
        type_letters(&mut engine, "bung");
        assert!(
            engine.preedit().candidates.contains(&"不能".to_string()),
            "候选是：{:?}",
            engine.preedit().candidates
        );
        assert_eq!(engine.on_key(key(KEY_SPACE)), Action::Commit("不能".into()));
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
