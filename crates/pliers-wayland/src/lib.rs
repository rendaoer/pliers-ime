//! Wayland 协议层：以"输入法"的身份连上合成器，抓键盘，把 `pliers-engine` 的决定变成
//! 协议请求。
//!
//! 一共用四个协议：
//!
//! * `zwp_input_method_manager_v2` / `zwp_input_method_v2`
//!   注册成输入法；输入框获得焦点时收到 `activate`
//! * `zwp_input_method_keyboard_grab_v2`
//!   抓键盘：抓到手之后所有按键先送到我们这里，合成器不再自己处理
//! * `zwp_virtual_keyboard_v1`
//!   虚拟键盘：不归我们管的按键，用它原样发回给应用
//! * `zwp_input_popup_surface_v2` + `wl_shm`
//!   候选框：自己往共享内存画像素，位置交给合成器；画什么在 `pliers-popup` 里
//! * `wl_output`
//!   只为了问一句"屏幕缩放多少"：1.5x/2x 的屏幕得按倍数多画几倍像素，不然字是糊的

pub mod control;
mod keyboard;
mod poll;
mod popup;
mod repeat;

use std::error::Error;
use std::os::fd::{AsFd, AsRawFd};

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2 as grab, zwp_input_method_manager_v2 as im_manager,
    zwp_input_method_v2 as im,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1 as vk_manager, zwp_virtual_keyboard_v1 as vk,
};
use xkbcommon::xkb;

use control::Control;
use keyboard::Keyboard;
use pliers_engine::{Action, Config, Engine, KeyInput, Mode, Preedit};
use pliers_popup::Painter;
use popup::PopupSurface;
use repeat::Repeat;

/// keysym 常量留给引擎用，这里重导出一份方便主程序看
pub use pliers_engine;

/// `zwp_virtual_keyboard_v1.keymap` 的 format：1 = XKB_KEYMAP_FORMAT_TEXT_V1
const KEYMAP_FORMAT_XKB_V1: u32 = 1;

/// 启动参数
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// 把每个按键的判定过程打到 stderr（也可以直接设环境变量 PLIERS_DEBUG=1）
    pub debug: bool,
}

/// 起一个输入法，一直跑到合成器把协议收回去为止
///
/// `config` 也留一份在手里：`pliers set ...` / `pliers reload` 改的就是它，
/// 改完按它重建引擎（见 [`State::apply_config`]）
pub fn run(config: Config, engine: Engine, options: Options) -> Result<(), Box<dyn Error>> {
    let debug = options.debug || std::env::var_os("PLIERS_DEBUG").is_some();

    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut state = State {
        config_stamp: config_stamp(),
        engine: Some(engine),
        config,
        control: bind_control(),
        debug,
        // 找字体要扫系统字体目录（几十毫秒），放启动时做，别卡在第一次敲键上
        painter: Some(Painter::new()),
        // 一般不用管缩放（问合成器就知道了），调试的时候可以用它强制指定
        scale_override: std::env::var("PLIERS_SCALE")
            .ok()
            .and_then(|value| value.parse().ok()),
        ..State::default()
    };

    // 第一次往返：拿回全局对象列表（wl_registry 的 global 事件）
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut state)?;

    let im_manager = state
        .im_manager
        .take()
        .ok_or("合成器不支持 zwp_input_method_manager_v2")?;
    let vk_manager = state
        .vk_manager
        .take()
        .ok_or("合成器不支持 zwp_virtual_keyboard_manager_v1")?;
    let seat = state.seat.take().ok_or("合成器没有 wl_seat")?;

    state.im = Some(im_manager.get_input_method(&seat, &qh, ()));
    state.vk = Some(vk_manager.create_virtual_keyboard(&seat, &qh, ()));

    // 第二次往返：等这两个对象在合成器那边建好，之后它才会给我们 activate
    queue.roundtrip(&mut state)?;

    run_loop(&mut state, &mut queue)
}

/// 建命令 socket。建不上（比如 /tmp 满了、$XDG_RUNTIME_DIR 有问题）不该影响打字，
/// 所以只吼一声，输入法照常跑
fn bind_control() -> Option<Control> {
    let path = control::socket_path();
    match Control::bind(&path) {
        Ok(control) => {
            if std::env::var_os("PLIERS_DEBUG").is_some() {
                eprintln!("pliers: 命令 socket 在 {}", path.display());
            }
            Some(control)
        }
        Err(e) => {
            eprintln!(
                "pliers: 命令 socket {} 建不起来（{e}）：pliers status/reload/set 这次用不了",
                path.display()
            );
            None
        }
    }
}

/// 主循环。
///
/// 要同时等两个东西：合成器的事件（Wayland socket）和 `pliers ...` 发过来的命令
/// （命令 socket）。所以不能再用 `blocking_dispatch()`（它只等 Wayland），
/// 改成标准的"两源事件循环"：
///
/// 1. 先把攒着的请求 flush 出去，再派发已经收到的事件；
/// 2. 真没活干了，`poll(2)` 两个 fd 一起等（不设超时，睡到有人叫醒为止）；
/// 3. 谁可读处理谁。
///
/// `prepare_read()` 拿到的守卫如果没读就要 drop 掉 —— 那表示"这次不读 socket 了"，
/// 连接会退回可写状态（不然下次发请求会被挡住）
///
/// poll 带超时（`WATCH_INTERVAL`）是为了**自动重读配置文件**：文件被改了不会有人来叫醒我们，
/// 只能隔一会儿自己看一眼 mtime
fn run_loop(
    state: &mut State,
    queue: &mut wayland_client::EventQueue<State>,
) -> Result<(), Box<dyn Error>> {
    while !state.quit {
        queue.flush()?;
        if queue.dispatch_pending(state)? == 0 {
            let Some(guard) = queue.prepare_read() else {
                continue; // 还有没派发的，回到循环开头
            };
            let wayland = guard.connection_fd().as_raw_fd();
            let fds: Vec<std::os::fd::RawFd> = match state.control.as_ref() {
                Some(control) => vec![wayland, control.fd()],
                None => vec![wayland],
            };
            let ready = poll::readable(&fds, Some(state.poll_timeout()))?;
            if ready[0] {
                guard.read()?;
            } else {
                drop(guard); // 是命令来了（或者只是超时）：取消这次读，下一轮再派发
            }
        }
        // 提示到点了就自己收掉，别等下一个按键（切完中英文不打字的话，
        // 等按键就等于一直挂在那儿）
        state.expire_notice();
        state.repeat_due();
        state.serve_control();
        state.reload_if_config_changed();
    }
    Ok(())
}

/// 多久看一眼配置文件有没有被改。人保存文件到生效最多等这么久；
/// 1 秒的 stat 一次，代价可以忽略
const WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(700);

/// 模式提示（「中」/「英」、删词那句）在屏幕上停留多久。
///
/// 以前是"等下一个按键才收"：切完中英文不打字，那个小方块就一直挂在光标那儿。
/// 现在到点自己收 —— 600ms 够看清一个字，又不至于挡着看
const NOTICE_DURATION: std::time::Duration = std::time::Duration::from_millis(600);

#[derive(Default)]
struct State {
    // 合成器那边的对象
    im: Option<im::ZwpInputMethodV2>,
    grab: Option<grab::ZwpInputMethodKeyboardGrabV2>,
    vk: Option<vk::ZwpVirtualKeyboardV1>,
    /// 键盘布局 + 当前修饰键状态（把 keycode 翻成 keysym）
    keyboard: Option<Keyboard>,
    /// 键盘布局已经转给虚拟键盘了吗
    vk_ready: bool,

    // 注册表里捡到的全局对象，启动时用一次就被 take 走
    seat: Option<wl_seat::WlSeat>,
    im_manager: Option<im_manager::ZwpInputMethodManagerV2>,
    vk_manager: Option<vk_manager::ZwpVirtualKeyboardManagerV1>,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    /// 所有输出（一个显示器一个），只为读它们的缩放
    outputs: Vec<wl_output::WlOutput>,
    /// 每个输出的缩放，下标跟 `outputs` 对齐
    output_scales: Vec<i32>,
    /// PLIERS_SCALE 强制指定的缩放（没有就用输出的）
    scale_override: Option<i32>,

    // 协议状态
    /// 现在有输入框在用吗（没有的话按键只能原样转发）
    active: bool,
    /// 收到过几次 `done`；`commit` 的 serial 必须等于这个数
    serial: u32,
    /// 最近一次按键的合成器时间戳，转发按键时要带上
    time: u32,
    /// Ctrl/Alt/Super 按着吗：引擎靠它区分"打字"和"快捷键"
    shortcut_mods: bool,
    /// 具体是 Ctrl 按着吗（认 Ctrl+空格切中英文用）
    ctrl_held: bool,
    /// 屏幕上现在挂的是不是那个模式提示（下一个键一来就收掉，或者到点自己收掉）
    notice: bool,
    /// 模式提示该在什么时候自己消失（`None` = 现在没在提示）
    notice_until: Option<std::time::Instant>,
    /// Shift / Caps Lock 按着吗（只用于调试日志，大小写已经在 keysym 里了）
    shift_held: bool,
    caps_lock: bool,
    /// 上一次同步给虚拟键盘的修饰键掩码（depressed, latched, locked, group）
    last_mods: (u32, u32, u32, u32),
    /// 键盘自动重复：合成器只给速率和延迟（`repeat_info`），重复得我们自己做 ——
    /// 不做的表现就是"按住退格只删一个字母"。见 [`repeat`]
    repeat: Repeat,

    // 候选框
    popup: Option<PopupSurface>,
    /// 画候选框的东西（字体在里面）。Option 只是为了 State::default() 不用去找字体
    painter: Option<Painter>,
    /// 合成器告知的光标矩形（相对候选框），只是提示
    caret: (i32, i32, i32, i32),

    /// 引擎（按键状态机 + 输入方案 + 词库）。
    /// Option 只是为了 State::default() 能编译 —— 引擎要开词库、找字体，没法 Default；
    /// run() 一定会把它塞进来
    engine: Option<Engine>,
    /// 现在生效的配置。`pliers set` 改它、`pliers reload` 从磁盘重新读它，
    /// 然后按它重建引擎
    config: Config,
    /// 命令行遥控用的监听 socket（建不起来就是 None）
    control: Option<Control>,
    /// 配置文件上次被改是什么时候（(mtime, 大小)）：变了就自动重读
    config_stamp: Option<(std::time::SystemTime, u64)>,
    debug: bool,
    quit: bool,
}

impl State {
    /// 引擎（run() 之前不该有人调用）
    fn engine(&mut self) -> &mut Engine {
        self.engine.as_mut().expect("引擎还没放进来")
    }

    // ---- 命令行遥控（`pliers status` / `reload` / `set ...`）----------------

    /// 命令 socket 上来的请求：收一条办一条。
    /// 一轮尽量把排队的都办完（accept 到 WouldBlock 为止）
    fn serve_control(&mut self) {
        while let Some(mut stream) = self.control.as_ref().and_then(Control::accept) {
            let answer = match control::read_request(&stream) {
                Ok(request) if request.is_empty() => {
                    "ERR 空命令（试试 status / reload）".to_string()
                }
                Ok(request) => self.run_command(&request),
                Err(e) => format!("ERR 读请求失败：{e}"),
            };
            if self.debug {
                eprintln!("pliers: 命令回话：{answer}");
            }
            if let Err(e) = control::reply(&mut stream, &answer) {
                eprintln!("pliers: 回命令失败：{e}");
            }
        }
    }

    /// 执行一条命令，返回给命令行看的一行（`OK ...` / `ERR ...`）
    fn run_command(&mut self, request: &str) -> String {
        let mut parts = request.splitn(3, ' ');
        let (command, rest) = (parts.next().unwrap_or(""), parts.next());
        match command {
            // 状态是一堆 tab 分隔的 `名=值`：怎么排版（换行、对齐）是命令行的事，
            // 这样 `socat` 之类自己接上来也能一眼看懂
            "status" => format!("OK\t{}", self.status_fields()),

            // 重新读配置文件：读坏了、写得不对，都保持原样继续跑（正在打字的人不该被打断）
            "reload" => match Config::load() {
                Ok(config) => match self.apply_config(config) {
                    Ok(()) => format!("OK\t{}", self.status_fields()),
                    Err(e) => format!("ERR 配置有问题，保持原样：{e}"),
                },
                Err(e) => format!("ERR 读配置失败，保持原样：{e}"),
            },

            // 改一项：先改内存里的配置，重建失败就整个回滚
            "set" => {
                let (Some(key), Some(value)) = (rest, parts.next()) else {
                    return "ERR 用法：pliers set <项> <值>（pliers --help 看能改哪些）"
                        .to_string();
                };
                let before = self.config.clone();
                if let Err(e) = self.config.set(key, value) {
                    return format!("ERR {e}");
                }
                match self.apply_config(self.config.clone()) {
                    Ok(()) => format!(
                        "OK\tchanged={key} = {value}{}\t{}",
                        self.status_note(key),
                        self.status_fields()
                    ),
                    Err(e) => {
                        self.config = before;
                        format!("ERR {e}（没改成）")
                    }
                }
            }

            other => {
                format!("ERR 不认识的命令 {other:?}（能用的：status / reload / set <项> <值>）")
            }
        }
    }

    /// 配置文件被改了就自动重读（`pliers config set` 和手动编辑都走这条路）。
    ///
    /// 改坏了不慌：新配置建不出引擎就保持原样，只在 stderr 上吼一声
    fn reload_if_config_changed(&mut self) {
        let stamp = config_stamp();
        if stamp.is_none() || stamp == self.config_stamp {
            return;
        }
        self.config_stamp = stamp;
        match Config::load() {
            Ok(config) => match self.apply_config(config) {
                Ok(()) => {
                    if self.debug {
                        eprintln!("pliers: 配置文件变了，已经重读");
                    }
                }
                Err(e) => eprintln!("pliers: 配置文件有问题，继续用旧的：{e}"),
            },
            Err(e) => eprintln!("pliers: 重读配置失败，继续用旧的：{e}"),
        }
    }

    /// 按配置重建引擎。失败就把错误原样返回，**不动**现有的引擎 ——
    /// 配置写错了不该把正在跑的输入法弄挂
    fn apply_config(&mut self, config: Config) -> Result<(), Box<dyn Error>> {
        // 正在打的这串和当前模式都得先记下来：`pliers set dict.max_candidates 5`
        // 不该把打到一半的拼音吃掉
        let mode = self.engine().mode();
        let composing = self.engine().text().to_string();

        let engine = Engine::from_config(&config)?;
        self.engine = Some(engine);
        // 新引擎从 start_mode 开始，这里把用户正在用的模式接上（切到英文之后 reload 不该跳回中文）
        self.engine().set_mode(mode);
        self.config = config;

        if composing.is_empty() {
            self.set_preedit(&Preedit::default());
        } else {
            // 塞回去并按新方案重查候选（换了方案的话，同一串键解出来的词会变）
            self.engine().set_text(&composing);
            let preedit = self.engine().preedit();
            self.set_preedit(&preedit);
        }
        Ok(())
    }

    /// 现状：tab 分隔的 `名=值`，排版（换行、对齐）交给命令行
    fn status_fields(&mut self) -> String {
        // 先把要用的几样读出来，再借引擎（不然同时可变 + 不可变借用 self）
        let mode = self.engine().mode().label();
        let config = &self.config;
        // 给两种用法：`dict` 是给人看的（带大小），`dict_path` 是原文（编辑框预填要用）
        let dict_path = config.dict_path();
        let dict = match std::fs::metadata(&dict_path) {
            Ok(meta) => format!("{}（{} MB）", dict_path.display(), meta.len() / 1024 / 1024),
            Err(_) => format!("{}（打不开？）", dict_path.display()),
        };
        let (kind, layout, name, sentence) = match &config.scheme {
            pliers_engine::SchemeConfig::FullPinyin { sentence } => {
                ("full-pinyin", String::new(), String::new(), *sentence)
            }
            pliers_engine::SchemeConfig::DoublePinyin {
                layout, sentence, ..
            } => ("double-pinyin", layout.clone(), String::new(), *sentence),
            pliers_engine::SchemeConfig::Table { name } => {
                ("table", String::new(), name.clone(), false)
            }
        };
        [
            format!("kind={kind}"),
            format!("layout={layout}"),
            format!("name={name}"),
            // 字段给 true/false（命令行要拿它跟选项比对），给人看的"开/关"由命令行渲染
            format!("sentence={sentence}"),
            format!("dict={dict}"),
            format!("dict_path={}", dict_path.display()),
            format!("candidates={}", config.dict.max_candidates),
            format!("pool={}", config.dict.pool_size),
            format!("toggle={}", config.engine.toggle_keys.join(",")),
            format!(
                "start={}",
                match config.engine.start_mode {
                    Mode::Chinese => "chinese",
                    Mode::English => "english",
                }
            ),
            format!("indicator={}", config.engine.indicator),
            format!("chinese_punctuation={}", config.engine.chinese_punctuation),
            format!("english={}", config.english.enabled),
            format!("english_limit={}", config.english.limit),
            format!("english_path={}", config.english.path),
            format!(
                "english_words={}",
                self.engine().english_words().unwrap_or(0)
            ),
            format!("mode={mode}"),
            format!("config={}", pliers_engine::config::config_path().display()),
            format!(
                "socket={}",
                match &self.control {
                    Some(control) => control.path().display().to_string(),
                    None => "（没建起来）".to_string(),
                }
            ),
        ]
        .join("\t")
    }

    /// 改完某一项之后，补一句"这项什么时候生效 / 有什么前提"
    fn status_note(&self, key: &str) -> String {
        match key {
            "engine.start_mode" => "；start_mode 下次启动才生效".to_string(),
            // PLIERS_DICT 的优先级比配置文件高，指了它的话改 dict.path 是白改
            "dict.path" if std::env::var_os("PLIERS_DICT").is_some() => {
                "；但 PLIERS_DICT 环境变量优先级更高，实际用的还是它".to_string()
            }
            "dict.path" => "；词库已经重新打开了，用户词频还在库里".to_string(),
            // 词表是起引擎的时候读进内存的，改完 path 得重新读一遍 —— 这里重建引擎了
            "english.path" | "english.enabled" => "；英文词表已经重新读了".to_string(),
            "english.limit" if self.config.english.enabled => {
                "；只影响一次给几个英文候选".to_string()
            }
            _ => String::new(),
        }
    }

    // ---- 把引擎的决定翻译成协议请求 ----------------------------------------

    /// 处理一个已经翻译好的按键：问引擎该干什么，然后照做
    fn handle_key(&mut self, keycode: u32, keysym: u32, pressed: bool) {
        // 先把修饰键状态读出来，再借引擎（不然同时可变+不可变借用同一个 self）
        let input = KeyInput {
            keycode,
            keysym,
            pressed,
            shortcut: self.shortcut_mods,
            ctrl: self.ctrl_held,
            active: self.active,
        };
        let mode_before = self.engine().mode();
        let action = self.engine().on_key(input);
        if self.debug {
            match &action {
                Action::UpdatePreedit(preedit) if preedit.candidates.is_empty() => {
                    eprintln!("pliers:   → 预编辑清空，候选框收起");
                }
                Action::UpdatePreedit(preedit) => eprintln!(
                    "pliers:   → 预编辑 {:?}，候选 {} 个（选中第 {} 个）",
                    preedit.text,
                    preedit.candidates.len(),
                    preedit.selected + 1
                ),
                Action::Commit(text) => eprintln!("pliers:   → 提交文本 {text:?}"),
                Action::CommitAndContinue(text, rest) => {
                    eprintln!("pliers:   → 分段上屏 {text:?}，剩下 {:?} 接着选", rest.text)
                }
                Action::CommitAndForward(text) => {
                    eprintln!("pliers:   → 提交文本 {text:?}，再把这个键原样交给应用")
                }
                Action::Notice(text) => eprintln!("pliers:   → 提示 {text:?}"),
                Action::Forward => {}
                Action::Swallow => eprintln!("pliers:   → 吃掉这个抬起"),
            }
        }
        // 要弹的话留到最后：模式提示优先，其次是这条
        let mut notice: Option<String> = None;
        match action {
            Action::UpdatePreedit(preedit) => self.set_preedit(&preedit),
            Action::Commit(text) => self.commit_text(&text),
            // 弹一句话，预编辑不动（引擎那边已经把候选列表改好了）
            Action::Notice(text) => notice = Some(text),
            // 分段上屏：「提交这段 + 剩下的预编辑」放进**同一批**发出去
            Action::CommitAndContinue(text, rest) => self.commit_and_continue(&text, &rest),
            // 顺序要紧：先把文字提交给应用，再把这个键转过去，
            // 应用才会把符号插在文字后面（不然就是 ",你好"）
            Action::CommitAndForward(text) => {
                self.commit_text(&text);
                self.forward(keycode, pressed);
            }
            Action::Forward => self.forward(keycode, pressed),
            // 被吃掉的键什么都不用做（连它的抬起也吃掉）
            Action::Swallow => {}
        }

        // 中英文切换：把应用里挂着的预编辑清掉，再弹一下「中」/「英」；
        // 别的按键一来就把提示收掉（候选框得跟引擎的当前状态一致）
        let mode_now = self.engine().mode();
        if mode_now != mode_before {
            self.set_preedit(&Preedit::default());
            self.show_notice(mode_now.label());
        } else if let Some(text) = notice {
            self.show_notice(&text);
        } else if pressed && self.notice {
            // 按键一来就把提示收掉 —— 但不能看"下一个事件"：
            // 触发提示那个键自己的抬起紧跟其后，会把刚弹出来的提示直接收掉
            //（表现就是"按了 Del 之后候选框没了"，切中英文的「中」/「英」也一闪而过）。
            // 没人接着按键的场合交给 expire_notice() 到点收（NOTICE_DURATION）
            self.hide_notice();
        }
    }

    /// 在光标处弹一个小方块说句话（切模式的「中」/「英」、删词的「删掉了…」都走它）
    fn show_notice(&mut self, text: &str) {
        self.notice = false;
        self.notice_until = None;
        if !self.engine().indicator() {
            return;
        }
        let scale = self.scale();
        let State {
            popup: Some(popup),
            painter: Some(painter),
            ..
        } = self
        else {
            return;
        };
        popup.show_notice(painter, text, scale);
        self.notice = true;
        self.notice_until = Some(std::time::Instant::now() + NOTICE_DURATION);
    }

    fn hide_notice(&mut self) {
        if self.debug && self.notice {
            eprintln!("pliers:   → 提示按下一次键就收掉");
        }
        self.notice = false;
        self.notice_until = None;
        if let Some(popup) = &mut self.popup {
            popup.hide();
        }
    }

    /// 提示到点了没（到点就该收掉，不用等下一个按键）
    fn notice_expired(&self) -> bool {
        self.notice
            && self
                .notice_until
                .is_some_and(|until| std::time::Instant::now() >= until)
    }

    /// 提示到点就收掉，并把候选框贴回**引擎的当前状态**。
    ///
    /// 不是简单收掉：删词提示消失之后，候选列表该接着显示
    ///（引擎里那些候选还在，预编辑也还在），跟着一起没就说不通了
    fn expire_notice(&mut self) {
        if !self.notice_expired() {
            return;
        }
        if self.debug {
            eprintln!("pliers:   → 提示到点，自己收掉");
        }
        let preedit = self.engine().preedit();
        self.sync_popup(&preedit);
    }

    /// 这一轮 `poll(2)` 最多睡多久：既要定期看一眼配置文件，
    /// 也要赶在提示到点之前醒过来（不然它得等到下一次按键或下一轮超时才消失），
    /// 还要赶在"该重复下一个键"之前醒过来（按住退格得一下一下地删）
    fn poll_timeout(&self) -> std::time::Duration {
        let now = std::time::Instant::now();
        let mut timeout = WATCH_INTERVAL;
        if let Some(until) = self.notice_until {
            timeout = timeout.min(until.saturating_duration_since(now));
        }
        if let Some(next) = self.repeat.timeout(now) {
            timeout = timeout.min(next);
        }
        timeout
    }

    /// 到点了：按住的键当成"又按了一次"，走同一条路（引擎吃掉的还是吃掉、
    /// 该转发给应用的还是转发）—— 所以按住退格能连删、终端里按住退格也能连删
    fn repeat_due(&mut self) {
        let Some((keycode, keysym)) = self.repeat.due(std::time::Instant::now()) else {
            return;
        };
        if self.debug {
            eprintln!("pliers:   → 长按重复 keycode={keycode} keysym=0x{keysym:04x}");
        }
        self.handle_key(keycode, keysym, true);
    }

    /// 把预编辑串发给应用（应用把它显示在输入框里），并同步候选框
    fn set_preedit(&mut self, preedit: &Preedit) {
        if let Some(im) = &self.im {
            // 光标停在预编辑串末尾，应用据此把光标画在正确的位置
            let cursor = preedit.text.len() as i32;
            im.set_preedit_string(preedit.text.clone(), cursor, cursor);
            // 协议是双缓冲的：set_preedit_string / commit_string 只是改"待生效"状态，
            // commit(serial) 才让合成器把它变成当前状态
            im.commit(self.serial);
        }
        self.sync_popup(preedit);
    }

    /// 结束组词：清掉预编辑，把最终文本提交给应用
    fn commit_text(&mut self, text: &str) {
        if let Some(im) = &self.im {
            im.set_preedit_string(String::new(), 0, 0);
            im.commit_string(text.to_string());
            im.commit(self.serial);
        }
        self.sync_popup(&Preedit::default());
    }

    /// 分段上屏：一批里同时给"上屏的文字"和"剩下的预编辑"。
    ///
    /// 为什么要一批：应用是按 `done` 一次应用一批状态的（GTK 的顺序是
    /// 先 commit 再 preedit），一次给全它就"落字 + 接着显示剩下的"。
    /// 分两批发的话，有的应用会把第二批的预编辑吃掉 —— 表现就是
    /// 「前面的 nihc 变成你好，后面的 an 输入框里没提示」
    fn commit_and_continue(&mut self, text: &str, rest: &Preedit) {
        if let Some(im) = &self.im {
            let cursor = rest.text.len() as i32;
            im.set_preedit_string(rest.text.clone(), cursor, cursor);
            im.commit_string(text.to_string());
            im.commit(self.serial);
        }
        self.sync_popup(rest);
    }

    /// 候选框跟着组词状态走：有候选就贴一帧，没有就收起来
    fn sync_popup(&mut self, preedit: &Preedit) {
        // 候选框贴的是真状态，那个「中」/「英」提示就不算数了 ——
        // 不然下一个按键会把正在显示的候选一起收掉
        self.notice = false;
        self.notice_until = None;
        let scale = self.scale();
        // 分开借 State 里的两个字段：一个要改，一个只读
        let State {
            popup: Some(popup),
            painter: Some(painter),
            ..
        } = self
        else {
            return;
        };
        if preedit.candidates.is_empty() {
            popup.hide();
        } else {
            popup.show(painter, preedit, scale);
        }
    }

    /// 候选框按几倍像素密度画。
    ///
    /// 取所有输出里最大的缩放：1.5x 的屏幕合成器会报 2（它只能说整数）。
    /// 用最大的那个，混着 1x / 2x 显示器时在 1x 上顶多浪费点内存（多画的像素会被缩回去），
    /// 但绝不会糊
    fn scale(&self) -> i32 {
        self.scale_override
            .unwrap_or_else(|| self.output_scales.iter().copied().max().unwrap_or(1))
            .clamp(1, 4)
    }

    /// 建候选框：一个 input_popup role 的 surface + 一堆 shm 缓冲区
    fn create_popup(
        &self,
        im_obj: &im::ZwpInputMethodV2,
        qh: &QueueHandle<Self>,
    ) -> Result<PopupSurface, Box<dyn Error>> {
        let compositor = self.compositor.as_ref().ok_or("合成器没有 wl_compositor")?;
        let shm = self.shm.as_ref().ok_or("合成器没有 wl_shm")?;
        PopupSurface::new(compositor, shm, im_obj, qh)
    }

    /// 把按键原样交给应用（通过虚拟键盘）
    fn forward(&self, keycode: u32, pressed: bool) {
        if self.debug {
            eprintln!(
                "pliers:   → 转发 keycode={keycode} {}（shift={} caps={} ctrl/alt/super={}）",
                if pressed { "按下" } else { "抬起" },
                self.shift_held,
                self.caps_lock,
                self.shortcut_mods
            );
        }
        if !self.vk_ready {
            return; // 还没拿到键盘布局，转发出去的应用会收到错的字符
        }
        if let Some(vk) = &self.vk {
            vk.key(self.time, keycode, u32::from(pressed));
        }
    }

    // ---- 修饰键状态 --------------------------------------------------------

    /// 把修饰键掩码同步给虚拟键盘（应用靠它决定 Ctrl+C 之类算不算快捷键）。
    /// 没变化就不发，免得每按一个键都灌一遍
    fn sync_mods_to_vk(&mut self, mods: (u32, u32, u32, u32)) {
        if mods == self.last_mods {
            return;
        }
        self.last_mods = mods;
        if self.debug {
            eprintln!(
                "pliers:   → 虚拟键盘 modifiers(depressed={:#x}, latched={:#x}, locked={:#x}, group={})",
                mods.0, mods.1, mods.2, mods.3
            );
        }
        if self.vk_ready
            && let Some(vk) = &self.vk
        {
            vk.modifiers(mods.0, mods.1, mods.2, mods.3);
        }
    }

    /// 按当前 xkb 状态刷新"修饰键标志"，并同步给虚拟键盘
    fn refresh_mods(&mut self) {
        let Some(keyboard) = &self.keyboard else {
            return;
        };
        let shortcut = keyboard.shortcut_mods();
        let ctrl = keyboard.ctrl_held();
        let (shift, caps) = keyboard.shift_and_caps();
        let masks = keyboard.masks();
        // 上面借用了 self.keyboard，到这里结束，后面才能改 self
        self.shortcut_mods = shortcut;
        self.ctrl_held = ctrl;
        self.shift_held = shift;
        self.caps_lock = caps;
        self.sync_mods_to_vk(masks);
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        // 只 bind 我们真正要用的全局对象，别的一概不管
        match interface.as_str() {
            "wl_seat" => state.seat = Some(registry.bind(name, version.min(8), qh, ())),
            "zwp_input_method_manager_v2" => {
                state.im_manager = Some(registry.bind(name, 1, qh, ()))
            }
            "zwp_virtual_keyboard_manager_v1" => {
                state.vk_manager = Some(registry.bind(name, 1, qh, ()))
            }
            "wl_compositor" => state.compositor = Some(registry.bind(name, version.min(5), qh, ())),
            "wl_shm" => state.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_output" => {
                // 缩放要 v2 才有（v4 还多了显示器名字，顺手 bind 上）
                state
                    .outputs
                    .push(registry.bind(name, version.min(4), qh, ()));
                state.output_scales.push(1); // 等它的 scale 事件到了再改
            }
            _ => {}
        }
    }
}

impl Dispatch<im::ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        im_obj: &im::ZwpInputMethodV2,
        event: im::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            // 输入框获得焦点：抓键盘，之后按键先到我们这儿
            im::Event::Activate => {
                state.active = true;
                if state.grab.is_none() {
                    // 抓取对象一直留着：deactivate 之后抓取并不会自己消失，
                    // 每次 activate 都重新抓一遍会攒出好几个抓取，按键可能被重复处理
                    state.grab = Some(im_obj.grab_keyboard(qh, ()));
                }
                // 候选框也等有输入框时再建：建 surface 是"一次性"的（role 只能定一次）
                if state.popup.is_none() {
                    match state.create_popup(im_obj, qh) {
                        Ok(p) => state.popup = Some(p),
                        Err(e) => eprintln!("pliers: 创建候选框失败：{e}"),
                    }
                }
            }

            // 焦点离开输入框：按住没放的键不会再有抬起事件，账本一起清掉
            im::Event::Deactivate => {
                state.active = false;
                // 按住没放的键不会再有抬起事件了，重复也跟着停
                state.repeat.clear();
                // 这里**上屏不了**，只能丢掉正在打的拼音。原因是合成器的时序：
                // 它先发 deactivate 给我们，紧接着就把这次 text-input 会话收掉了
                //（niri/smithay: keyboard.rs 里 deactivate_input_method() 之后
                // 立刻 text_input_leave()，活动 text-input 被清空）。
                // 等我们反应过来再 commit_string，合成器那边已经没有收件人了 ——
                // 要么丢掉，要么落到**下一个**输入框里，后者更糟，所以不试。
                // 想要打了一半的拼音不白打，就在换窗口前按空格/回车把它收掉
                if let Some(text) = state.engine().take_raw()
                    && state.debug
                {
                    eprintln!(
                        "pliers: 焦点离开输入框，正在组的 {text:?} 只能丢掉（协议来不及上屏）"
                    );
                }
                state.engine().reset();
                state.sync_popup(&Preedit::default());
            }

            // done：合成器那边状态更新完了（比如 activate 后紧跟一个 done），
            // 这里的 serial 就是 commit 要用的编号
            im::Event::Done => state.serial += 1,

            // 合成器把输入法协议收回去了（比如被别的输入法顶掉），退出就行
            im::Event::Unavailable => {
                // 注意：同一个 seat 上只要出现第二个 zwp_input_method_v2，合成器就会给
                // 旧的那个发 unavailable（smithay: InputMethodHandle::add_instance）
                eprintln!("pliers: 有另一个输入法接管了这个 seat，协议被收回，退出");
                state.quit = true;
            }

            _ => {}
        }
    }
}

/// 这个键值不值得重复。修饰键（Shift/Ctrl/Alt/Super/Caps）不重复，
/// 输入法自己的切换键（Ctrl+空格）也不重复 —— 它是"切一下"，不是"按着不放"
fn repeatable(state: &State, keysym: u32) -> bool {
    if is_modifier(keysym) {
        return false;
    }
    if keysym == pliers_engine::KEY_SPACE
        && state.ctrl_held
        && state
            .config
            .engine
            .toggle_keys
            .iter()
            .any(|key| key == "ctrl+space")
    {
        return false;
    }
    true
}

/// X11 里的修饰键 keysym：Shift_L(0xffe1)…Hyper_R(0xffee)，外加 ISO_Level3_Shift(0xfe03)。
/// 真键盘按住 Shift 也不会重复
fn is_modifier(keysym: u32) -> bool {
    (0xffe1..=0xffee).contains(&keysym) || keysym == 0xfe03
}

impl Dispatch<grab::ZwpInputMethodKeyboardGrabV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &grab::ZwpInputMethodKeyboardGrabV2,
        event: grab::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // 合成器把当前键盘布局发过来：一是给我们翻译 keysym 用，
            // 二是原样转给虚拟键盘 —— 应用收到的按键要跟真实键盘布局一致
            grab::Event::Keymap { format, fd, size } => {
                if !matches!(format, WEnum::Value(wl_keyboard::KeymapFormat::XkbV1)) {
                    return;
                }
                // new_from_fd 会吃掉 fd，所以先复制一份留给虚拟键盘
                let copy = fd.as_fd().try_clone_to_owned().ok();
                let ctx = xkb::Context::new(0);
                match Keyboard::new(&ctx, fd, size as usize) {
                    Some(keyboard) => state.keyboard = Some(keyboard),
                    None => eprintln!("pliers: 解析 XKB 键盘布局失败"),
                }
                if let (Some(vk_obj), Some(copy)) = (&state.vk, copy) {
                    vk_obj.keymap(KEYMAP_FORMAT_XKB_V1, copy.as_fd(), size);
                    state.vk_ready = true;
                }
            }

            // 按键：翻译成 keysym，刷新修饰键状态，然后交给引擎
            grab::Event::Key {
                time,
                key,
                state: key_state,
                ..
            } => {
                state.time = time;
                let pressed = matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                let keysym = match &mut state.keyboard {
                    Some(keyboard) => keyboard.update_key(key, pressed),
                    None => return,
                };
                state.refresh_mods();
                // 自动重复：记下"现在按着谁"。修饰键和输入法自己的切换键不重复 ——
                // 按住 Shift 不放会在松手时被当成"轻按 Shift"切模式，
                // 按住 Ctrl+空格不放会来回切中英文
                let repeatable = repeatable(state, keysym);
                state
                    .repeat
                    .key(key, keysym, pressed, repeatable, std::time::Instant::now());
                if state.debug {
                    let preedit = state.engine().text().to_string();
                    eprintln!(
                        "pliers: 收到 keycode={key} keysym=0x{keysym:04x} {}（shift={} caps={} ctrl/alt/super={} 预编辑={preedit:?}）",
                        if pressed { "按下" } else { "抬起" },
                        state.shift_held,
                        state.caps_lock,
                        state.shortcut_mods,
                    );
                }
                state.handle_key(key, keysym, pressed);
            }

            // 合成器报的修饰键状态。我们自己的判断不靠它（见 xkb 模块），
            // 但既然送来了就顺手同步给虚拟键盘
            grab::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if state.debug {
                    eprintln!(
                        "pliers: 收到 modifiers 事件 depressed={mods_depressed:#x} latched={mods_latched:#x} locked={mods_locked:#x} group={group}"
                    );
                }
                state.sync_mods_to_vk((mods_depressed, mods_latched, mods_locked, group));
            }

            // 合成器报"按住多久开始重复、每秒重复几次"。它自己**不会**替我们重复
            //（协议跟 wl_keyboard 一个规矩），所以这个值是我们唯一的依据
            grab::Event::RepeatInfo { rate, delay } => {
                if state.debug {
                    eprintln!("pliers: 合成器报的重复速率 rate={rate}/s delay={delay}ms");
                }
                state.repeat.set_info(rate, delay);
            }

            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // 只关心缩放：候选框按它决定画几倍像素
        if let wl_output::Event::Scale { factor } = event
            && let Some(index) = state.outputs.iter().position(|it| it.id() == output.id())
        {
            state.output_scales[index] = factor;
            if state.debug {
                eprintln!("pliers: 输出缩放 {factor}x（候选框按这个倍数画）");
            }
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(
        state: &mut Self,
        buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // 合成器把这块缓冲区还回来了：现在改写它的内容才安全
        if let wl_buffer::Event::Release = event
            && let Some(popup) = &mut state.popup
        {
            popup.release(buffer);
        }
    }
}

// 下面这些对象只有请求、没有我们在意的事件，实现一个空的 Dispatch 就能 bind / 创建
macro_rules! ignore {
    ($($iface:ty),* $(,)?) => {$(
        impl Dispatch<$iface, ()> for State {
            fn event(_: &mut Self, _: &$iface, _: <$iface as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}

ignore!(
    wl_seat::WlSeat,
    im_manager::ZwpInputMethodManagerV2,
    vk_manager::ZwpVirtualKeyboardManagerV1,
    vk::ZwpVirtualKeyboardV1,
    wl_compositor::WlCompositor,
    wl_surface::WlSurface,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
);

/// 配置文件的"版本号"：修改时间 + 大小。文件不在就是 None
fn config_stamp() -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(pliers_engine::config::config_path()).ok()?;
    let mtime = meta.modified().ok()?;
    Some((mtime, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 提示到点才算过期() {
        // 没在提示：什么时候都不算到点（不然会把候选框一起收掉）
        let mut state = State {
            notice_until: Some(std::time::Instant::now() - NOTICE_DURATION),
            ..State::default()
        };
        assert!(!state.notice_expired());

        // 在提示、但时间还没到
        state.notice = true;
        state.notice_until = Some(std::time::Instant::now() + NOTICE_DURATION);
        assert!(!state.notice_expired());

        // 到点了
        state.notice_until = Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
        assert!(state.notice_expired());
    }

    #[test]
    fn 提示没到点时_poll_提前醒() {
        // 平时只为"自动重读配置文件"醒一次
        let state = State::default();
        assert_eq!(state.poll_timeout(), WATCH_INTERVAL);

        // 有提示在倒计时：得在它到点之前醒过来（不然提示要等到下一次按键才消失）
        let left = std::time::Duration::from_millis(80);
        let state = State {
            notice: true,
            notice_until: Some(std::time::Instant::now() + left),
            ..State::default()
        };
        let timeout = state.poll_timeout();
        assert!(timeout <= left, "poll 睡太久了：{timeout:?} > {left:?}");
        assert!(timeout > std::time::Duration::ZERO);
    }

    #[test]
    fn 按住键时_poll_按重复的节拍醒() {
        // 什么都没按：平时那样（只为自动重读配置文件醒）
        let mut state = State::default();
        assert_eq!(state.poll_timeout(), WATCH_INTERVAL);

        // 按住退格：poll 得在"该重复下一个"之前醒过来，不然按住不放就是不删
        state.repeat.set_info(25, 10);
        state.repeat.key(
            14,
            pliers_engine::KEY_BACKSPACE,
            true,
            true,
            std::time::Instant::now(),
        );
        let timeout = state.poll_timeout();
        assert!(
            timeout <= std::time::Duration::from_millis(10),
            "按住退格时 poll 睡太久了：{timeout:?}"
        );
    }

    #[test]
    fn 修饰键和切换键不重复() {
        let state = State::default();
        // Shift_L / Control_L / Caps_Lock：真键盘按住也不重复
        for keysym in [0xffe1, 0xffe2, 0xffe3, 0xffe5] {
            assert!(!repeatable(&state, keysym), "0x{keysym:04x} 不该重复");
        }
        // 普通字母键：重复
        assert!(repeatable(&state, 0x61));

        // Ctrl+空格是默认的切换键：按住不放会来回切中英文，所以不重复
        let ctrl = State {
            ctrl_held: true,
            ..State::default()
        };
        assert!(!repeatable(&ctrl, pliers_engine::KEY_SPACE));
        // 没按 Ctrl 的空格照旧重复（组词完了按住空格就是一直打空格）
        assert!(repeatable(&state, pliers_engine::KEY_SPACE));
    }
}
