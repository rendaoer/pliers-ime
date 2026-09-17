//! 最小输入法：打 "nihao" 再按空格 → 输出 "你好"
//!
//! 直接用 wayland-client 跟合成器说协议，不再经过 imekit 之类的封装。
//! 一共用三个协议：
//!
//!   zwp_input_method_manager_v2 / zwp_input_method_v2
//!       我们以"输入法"的身份注册进去，合成器在输入框获得焦点时发 activate
//!   zwp_input_method_keyboard_grab_v2
//!       抓键盘：抓到手之后所有按键先送到我们这里，合成器不再自己处理
//!   zwp_virtual_keyboard_v1
//!       虚拟键盘：不归我们管的按键，用它原样发回给应用
//!   zwp_input_popup_surface_v2（＋ wl_shm）
//!       候选框：一个被指定成 "input_popup" role 的 surface，位置由合成器摆在光标旁，
//!       内容是我们自己往共享内存里画的像素
//!
//! 两个容易混的编号：
//!   keycode  物理按键编号（Linux evdev，空格 = 57）
//!   keysym   字符/符号编号（X11 那套，空格 = 0x20、'a' = 0x61、退格 = 0xff08）
//! 合成器只给 keycode，配上一份 XKB 键盘布局，我们用 xkbcommon 翻出 keysym 再判断按键。

use std::error::Error;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;

use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2 as grab, zwp_input_method_manager_v2 as im_manager,
    zwp_input_method_v2 as im, zwp_input_popup_surface_v2 as popup,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1 as vk_manager, zwp_virtual_keyboard_v1 as vk,
};
use xkbcommon::xkb;

const KEY_SPACE: u32 = 0x20;
const KEY_BACKSPACE: u32 = 0xff08;
const KEYMAP_FORMAT_XKB_V1: u32 = 1; // wl_keyboard.keymap_format 里的 XKB_V1

// 候选框长什么样：一共就一块背景 + 一根跟着拼音变长的色条
const POPUP_HEIGHT: i32 = 56; // 高（逻辑像素）
const POPUP_STEP: i32 = 26; // 拼音每多一个字母，框就宽这么多
const POPUP_MAX_LEN: usize = 16; // 最多给多长的拼音预先准备好缓冲区
const POPUP_BG: [u8; 4] = [0x2E, 0x2A, 0x2A, 0xFF]; // 深灰，字节序是 B G R A
const POPUP_FG: [u8; 4] = [0xFF, 0xA8, 0x4F, 0xFF]; // 蓝色

#[derive(Default)]
struct State {
    // 合成器那边的对象
    im: Option<im::ZwpInputMethodV2>,
    grab: Option<grab::ZwpInputMethodKeyboardGrabV2>,
    vk: Option<vk::ZwpVirtualKeyboardV1>,
    xkb: Option<xkb::State>, // 键盘布局，用来把 keycode 翻成 keysym
    vk_ready: bool,          // 键盘布局已经转给虚拟键盘了吗

    // 注册表里捡到的全局对象，启动时用一次就被 take 走
    seat: Option<wl_seat::WlSeat>,
    im_manager: Option<im_manager::ZwpInputMethodManagerV2>,
    vk_manager: Option<vk_manager::ZwpVirtualKeyboardManagerV1>,
    // 候选框要用：compositor 建 surface，shm 提供像素内存
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,

    // 协议状态
    active: bool, // 现在有输入框在用吗（没有的话按键只能原样转发）
    serial: u32,  // 收到过几次 done；commit 的 serial 必须等于这个数
    time: u32,    // 最近一次按键的时间戳，转发按键时要带上

    // 候选框
    popup: Option<Popup>,
    caret: (i32, i32, i32, i32), // 合成器告知的光标矩形（相对候选框），只是提示

    // 输入法自己的状态
    buffer: String,     // 攒着的拼音
    consumed: Vec<u32>, // 被我们吃掉的按键，它们的抬起事件也要吃掉
    quit: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut state = State::default();

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
    // 所以事件处理中发出的请求（预编辑、提交、转发按键）不用自己 flush。
    while !state.quit {
        queue.blocking_dispatch(&mut state)?;
    }
    Ok(())
}

impl State {
    /// 把 buffer 当作预编辑文本发给应用（应用把它显示在输入框里），并同步候选框
    fn show_preedit(&mut self) {
        if let Some(im) = &self.im {
            let cursor = self.buffer.len() as i32;
            im.set_preedit_string(self.buffer.clone(), cursor, cursor);
            // 协议是双缓冲的：set_preedit_string / commit_string 只是改"待生效"状态，
            // commit(serial) 才让合成器把它变成当前状态。serial 必须是最近一次 done 的编号。
            im.commit(self.serial);
        }
        self.sync_popup();
    }

    /// 结束组词：清掉预编辑，把最终文本提交给应用
    fn commit_text(&mut self, text: &str) {
        if let Some(im) = &self.im {
            im.set_preedit_string(String::new(), 0, 0);
            im.commit_string(text.to_string());
            im.commit(self.serial);
        }
        self.buffer.clear();
        self.sync_popup();
    }

    /// 候选框跟着拼音走：有拼音就贴一块，没有就藏起来
    fn sync_popup(&self) {
        let Some(popup) = &self.popup else { return };
        if self.buffer.is_empty() {
            popup.hide();
        } else {
            popup.show(self.buffer.len());
        }
    }

    /// 建候选框：一个 input_popup role 的 surface + 一堆 shm 缓冲区
    fn create_popup(
        &self,
        im_obj: &im::ZwpInputMethodV2,
        qh: &QueueHandle<Self>,
    ) -> Result<Popup, Box<dyn Error>> {
        let compositor = self.compositor.as_ref().ok_or("合成器没有 wl_compositor")?;
        let shm = self.shm.as_ref().ok_or("合成器没有 wl_shm")?;
        Popup::new(compositor, shm, im_obj, qh)
    }

    /// 把按键原样交给应用（通过虚拟键盘）
    fn forward(&self, keycode: u32, pressed: bool) {
        if !self.vk_ready {
            return; // 还没拿到键盘布局，转发出去的应用会收到错的字符
        }
        if let Some(vk) = &self.vk {
            vk.key(self.time, keycode, u32::from(pressed));
        }
    }

    /// 处理一个按键：要么攒进拼音，要么转发给应用
    fn on_key(&mut self, keycode: u32, keysym: u32, pressed: bool) {
        // 没有输入框在用（焦点在 XWayland 应用或不支持 text-input 的应用上）时，
        // 键盘抓取仍然在我们手里，所以必须原样转发，绝不能组词吞键
        if !self.active {
            self.forward(keycode, pressed);
            return;
        }

        // 抬起事件：账本里有的按键继续吃掉（一次清光，长按会重复记账），
        // 账本里没有的原样转发给应用
        if !pressed {
            if self.consumed.contains(&keycode) {
                self.consumed.retain(|k| *k != keycode);
            } else {
                self.forward(keycode, false);
            }
            return;
        }

        match keysym {
            // 字母 a-z：攒进拼音 buffer，并作为预编辑文本显示
            0x61..=0x7a => {
                self.buffer.push(keysym as u8 as char);
                self.show_preedit();
                self.consumed.push(keycode);
            }

            // 空格：有拼音就转成汉字提交，没有就当普通空格交给应用
            KEY_SPACE if !self.buffer.is_empty() => {
                // 想加词就往这个 match 里加；不认识的拼音原样提交
                let text = match self.buffer.as_str() {
                    "nihao" => "你好",
                    other => other,
                }
                .to_string();
                self.commit_text(&text);
                self.consumed.push(keycode);
            }

            // 退格：删掉一个字母
            KEY_BACKSPACE if !self.buffer.is_empty() => {
                self.buffer.pop();
                self.show_preedit();
                self.consumed.push(keycode);
            }

            // 其他按键（Shift、Ctrl、Esc、回车……）都交给应用
            _ => self.forward(keycode, true),
        }
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
                        Err(e) => eprintln!("ime-aa: 创建候选框失败：{e}"),
                    }
                }
            }

            // 焦点离开输入框：按住没放的键不会再有抬起事件，账本一起清掉
            im::Event::Deactivate => {
                state.active = false;
                state.buffer.clear();
                state.consumed.clear();
                state.sync_popup(); // 把候选框藏起来
            }

            // done：合成器那边状态更新完了（比如 activate 后紧跟一个 done），
            // 这里的 serial 就是 commit 要用的编号
            im::Event::Done => state.serial += 1,

            // 合成器把输入法协议收回去了（比如被别的输入法顶掉），退出就行
            im::Event::Unavailable => {
                // 注意：同一个 seat 上只要出现第二个 zwp_input_method_v2，合成器就会给
                // 旧的那个发 unavailable（smithay: InputMethodHandle::add_instance）
                eprintln!("ime-aa: 有另一个输入法接管了这个 seat，协议被收回，退出");
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
                // SAFETY: fd 是合成器刚发来的，size 是它声明的文件长度
                match unsafe {
                    xkb::Keymap::new_from_fd(
                        &ctx,
                        fd,
                        size as usize,
                        xkb::KEYMAP_FORMAT_TEXT_V1,
                        xkb::KEYMAP_COMPILE_NO_FLAGS,
                    )
                } {
                    Ok(Some(keymap)) => state.xkb = Some(xkb::State::new(&keymap)),
                    _ => eprintln!("ime-aa: 解析 XKB 键盘布局失败"),
                }
                if let (Some(vk_obj), Some(copy)) = (&state.vk, copy) {
                    vk_obj.keymap(KEYMAP_FORMAT_XKB_V1, copy.as_fd(), size);
                    state.vk_ready = true;
                }
            }

            // 按键：先翻译成 keysym，再交给上面的 on_key 处理
            grab::Event::Key {
                time,
                key,
                state: key_state,
                ..
            } => {
                state.time = time;
                let Some(xkb) = &state.xkb else { return };
                // Wayland 给的是 evdev 编号，XKB 的编号比它大 8
                let keysym = xkb.key_get_one_sym(xkb::Keycode::new(key + 8)).raw();
                let pressed = matches!(key_state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                state.on_key(key, keysym, pressed);
            }

            // 修饰键状态（Shift/Ctrl/Alt…）：更新我们自己的布局状态，
            // 同时同步给虚拟键盘，否则转发出去的 Ctrl+C 之类会变成别的字符
            grab::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(xkb) = &mut state.xkb {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                if state.vk_ready
                    && let Some(vk) = &state.vk
                {
                    vk.modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }

            _ => {}
        }
    }
}

// 候选框：合成器给位置，我们给像素。
//
// 关键点：surface 被 get_input_popup_surface 指定成 "input_popup" role 之后，
// 位置就完全由合成器决定了（niri 会摆在光标下方，放不下就翻到上方），客户端
// 没法自己挪；能控制的只有"画多大、画什么"。
struct Popup {
    surface: wl_surface::WlSurface,
    _popup: popup::ZwpInputPopupSurfaceV2, // 留着别 drop，drop 掉候选框就没了
    buffers: Vec<wl_buffer::WlBuffer>,     // buffers[i] 对应长度为 i+1 的拼音
}

impl Popup {
    fn new(
        compositor: &wl_compositor::WlCompositor,
        shm: &wl_shm::WlShm,
        im_obj: &im::ZwpInputMethodV2,
        qh: &QueueHandle<State>,
    ) -> Result<Self, Box<dyn Error>> {
        let surface = compositor.create_surface(qh, ());
        let popup_obj = im_obj.get_input_popup_surface(&surface, qh, ());

        // 每个可能的拼音长度准备一块缓冲区，一次放进同一个 shm pool：
        // 偏移 0, size0, size0+size1, ...（每块按 64 字节对齐，省得踩到边界问题）
        let sizes: Vec<i32> = (1..=POPUP_MAX_LEN)
            .map(|len| popup_width(len) * 4 * POPUP_HEIGHT)
            .collect();
        let total: i32 = sizes.iter().map(|s| (s + 63) / 64 * 64).sum();

        let file = shm_file(total as usize)?;
        let pool = shm.create_pool(file.as_fd(), total, qh, ());

        let mut buffers = Vec::new();
        let mut offset = 0i32;
        for (i, &size) in sizes.iter().enumerate() {
            let (w, pixels) = popup_pixels(i + 1);
            file.write_all_at(&pixels, offset as u64)?;
            buffers.push(pool.create_buffer(
                offset,
                w,
                POPUP_HEIGHT,
                w * 4,
                wl_shm::Format::Argb8888,
                qh,
                (),
            ));
            offset += (size + 63) / 64 * 64;
        }

        Ok(Self {
            surface,
            _popup: popup_obj,
            buffers,
        })
    }

    /// 贴一块和拼音一样长的框
    fn show(&self, len: usize) {
        let len = len.clamp(1, POPUP_MAX_LEN);
        let w = popup_width(len);
        self.surface.attach(Some(&self.buffers[len - 1]), 0, 0);
        self.surface.damage(0, 0, w, POPUP_HEIGHT);
        self.surface.commit();
    }

    /// attach 一个空 buffer 就是"隐藏"
    fn hide(&self) {
        self.surface.attach(None, 0, 0);
        self.surface.commit();
    }
}

fn popup_width(len: usize) -> i32 {
    48 + POPUP_STEP * len.clamp(1, POPUP_MAX_LEN) as i32
}

/// 画一块 ARGB8888 的像素：深灰底 + 一根跟拼音一样长的色条
fn popup_pixels(len: usize) -> (i32, Vec<u8>) {
    let w = popup_width(len);
    let mut px = vec![0u8; (w * POPUP_HEIGHT * 4) as usize];
    let mut put = |x: i32, y: i32, c: [u8; 4]| {
        let i = ((y * w + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&c);
    };
    for y in 0..POPUP_HEIGHT {
        for x in 0..w {
            put(x, y, POPUP_BG);
        }
    }
    // 下边留一条 8px 的色条，长度随拼音增长（有文字了以后这里换成候选词）
    for y in (POPUP_HEIGHT - 18)..(POPUP_HEIGHT - 10) {
        for x in 12..(12 + POPUP_STEP * len as i32).min(w - 12) {
            put(x, y, POPUP_FG);
        }
    }
    (w, px)
}

/// 建一块共享内存文件当 wl_shm pool 的后备存储。
/// /dev/shm 是 tmpfs，性能最好；沙箱/容器里没有就退回临时目录。
fn shm_file(size: usize) -> std::io::Result<std::fs::File> {
    let name = format!("ime-aa-popup-{}", std::process::id());
    let mut last_err = None;
    for dir in ["/dev/shm", "/tmp"] {
        let path = std::path::Path::new(dir).join(&name);
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path);
        match opened {
            Ok(file) => {
                file.set_len(size as u64)?;
                // 我们手上的 fd 会让文件继续存在，路径现在就可以删掉
                let _ = std::fs::remove_file(&path);
                return Ok(file);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("至少试过一个目录"))
}

impl Dispatch<popup::ZwpInputPopupSurfaceV2, ()> for State {
    fn event(
        state: &mut Self,
        _: &popup::ZwpInputPopupSurfaceV2,
        event: popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // 合成器告诉我们光标在候选框坐标系里的位置：只是个提示（位置还是它定），
        // 变化时打一行日志，方便确认这条链路是活的
        if let popup::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
            && state.caret != (x, y, width, height)
        {
            state.caret = (x, y, width, height);
            eprintln!("ime-aa: 光标矩形 {width}x{height} @ ({x},{y})（相对候选框左上角）");
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
    wl_buffer::WlBuffer,
);
