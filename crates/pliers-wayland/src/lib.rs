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

mod keyboard;
mod popup;

use std::error::Error;
use std::os::fd::AsFd;

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

use keyboard::Keyboard;
use pliers_engine::{Action, Engine, KeyInput, Mode, Preedit};
use pliers_popup::Painter;
use popup::PopupSurface;

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
pub fn run(engine: Engine, options: Options) -> Result<(), Box<dyn Error>> {
    let debug = options.debug || std::env::var_os("PLIERS_DEBUG").is_some();

    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut state = State {
        engine: Some(engine),
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

    // 主循环：没事件就睡在 blocking_dispatch 里。它内部会先 flush 再读 socket，
    // 所以事件处理中发出的请求（预编辑、提交、转发按键、贴候选框）不用自己 flush。
    while !state.quit {
        queue.blocking_dispatch(&mut state)?;
    }
    Ok(())
}

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
    /// 屏幕上现在挂的是不是那个模式提示（下一个键一来就收掉）
    notice: bool,
    /// Shift / Caps Lock 按着吗（只用于调试日志，大小写已经在 keysym 里了）
    shift_held: bool,
    caps_lock: bool,
    /// 上一次同步给虚拟键盘的修饰键掩码（depressed, latched, locked, group）
    last_mods: (u32, u32, u32, u32),

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
    debug: bool,
    quit: bool,
}

impl State {
    /// 引擎（run() 之前不该有人调用）
    fn engine(&mut self) -> &mut Engine {
        self.engine.as_mut().expect("引擎还没放进来")
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
                Action::Forward => {}
                Action::Swallow => eprintln!("pliers:   → 吃掉这个抬起"),
            }
        }
        match action {
            Action::UpdatePreedit(preedit) => self.set_preedit(&preedit),
            Action::Commit(text) => self.commit_text(&text),
            Action::Forward => self.forward(keycode, pressed),
            // 被吃掉的键什么都不用做（连它的抬起也吃掉）
            Action::Swallow => {}
        }

        // 中英文切换：把应用里挂着的预编辑清掉，再弹一下「中」/「英」；
        // 别的按键一来就把提示收掉（候选框得跟引擎的当前状态一致）
        let mode_now = self.engine().mode();
        if mode_now != mode_before {
            self.set_preedit(&Preedit::default());
            self.show_notice(mode_now);
        } else if self.notice {
            self.hide_notice();
        }
    }

    /// 在光标处弹一个小方块显示「中」/「英」（复用候选框：就一个候选，不带序号）
    fn show_notice(&mut self, mode: Mode) {
        self.notice = false;
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
        popup.show_notice(painter, mode.label(), scale);
        self.notice = true;
    }

    fn hide_notice(&mut self) {
        self.notice = false;
        if let Some(popup) = &mut self.popup {
            popup.hide();
        }
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

    /// 候选框跟着组词状态走：有候选就贴一帧，没有就收起来
    fn sync_popup(&mut self, preedit: &Preedit) {
        // 候选框贴的是真状态，那个「中」/「英」提示就不算数了 ——
        // 不然下一个按键会把正在显示的候选一起收掉
        self.notice = false;
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
