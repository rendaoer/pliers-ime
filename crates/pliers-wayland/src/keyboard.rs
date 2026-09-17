//! keycode → keysym 的翻译，以及修饰键状态的维护。
//!
//! 两个关键点（都是踩过坑的）：
//!
//! 1. **修饰键状态自己算。** smithay 只在"修饰键状态刚变"的那一次按键上才给输入法发
//!    `Modifiers` 事件（`mods_changed.then_some(...)`），拿它当唯一来源非常脆：一旦收不到，
//!    Ctrl 就变成普通字母、被组词吃掉。普通客户端本来也是自己按 keycode 喂 xkb 状态的。
//! 2. **`update_key` 和 `update_mask` 不要混用**（xkb 文档明确说了），所以合成器发来的
//!    `Modifiers` 事件只用来转发给虚拟键盘，不喂这里的 `xkb::State`。

use std::os::fd::OwnedFd;

use xkbcommon::xkb;

/// 一个键盘布局 + 当前状态
pub struct Keyboard {
    state: xkb::State,
}

impl Keyboard {
    /// 用合成器发来的键盘布局建一个状态机。
    /// `fd` 是 `Keymap` 事件的 fd —— `new_from_fd` 会吃掉它（调用方要留副本就先 dup）。
    pub fn new(ctx: &xkb::Context, fd: OwnedFd, size: usize) -> Option<Self> {
        // SAFETY: fd 是合成器刚发来的，size 是它声明的文件长度
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(
                ctx,
                fd,
                size,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        }
        .ok()
        .flatten()?;
        Some(Self {
            state: xkb::State::new(&keymap),
        })
    }

    /// 喂一个按键（Wayland 的 keycode，也就是 evdev 编号），返回它对应的 keysym
    pub fn update_key(&mut self, keycode: u32, pressed: bool) -> u32 {
        // XKB 的 keycode 比 evdev 大 8
        let kc = xkb::Keycode::new(keycode + 8);
        let dir = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        self.state.update_key(kc, dir);
        self.state.key_get_one_sym(kc).raw()
    }

    /// Ctrl / Alt / Super 按着吗（这几个一按，字母键就是快捷键）
    pub fn shortcut_mods(&self) -> bool {
        let mods = xkb::STATE_MODS_EFFECTIVE;
        self.state.mod_name_is_active(xkb::MOD_NAME_CTRL, mods)
            || self.state.mod_name_is_active(xkb::MOD_NAME_ALT, mods)
            || self.state.mod_name_is_active(xkb::MOD_NAME_LOGO, mods)
    }

    /// 只有 Ctrl 按着吗（用来认 Ctrl+空格；Alt+空格得留给应用）
    pub fn ctrl_held(&self) -> bool {
        self.state
            .mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE)
    }

    /// Shift / Caps Lock 按着吗（只用来打日志：大小写其实已经体现在 keysym 里了）
    pub fn shift_and_caps(&self) -> (bool, bool) {
        let mods = xkb::STATE_MODS_EFFECTIVE;
        (
            self.state.mod_name_is_active(xkb::MOD_NAME_SHIFT, mods),
            // MOD_NAME_CAPS 就是 xkb 里的 "Lock"，也就是 Caps Lock
            self.state.mod_name_is_active(xkb::MOD_NAME_CAPS, mods),
        )
    }

    /// 当前的修饰键掩码（depressed, latched, locked, group）。
    /// 应用要靠虚拟键盘的 `modifiers` 请求才知道 Shift/Ctrl 按着没有 ——
    /// smithay 的 `zwp_virtual_keyboard_v1.key()` 只发 `wl_keyboard.key`，不更新修饰键状态。
    pub fn masks(&self) -> (u32, u32, u32, u32) {
        (
            self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED),
            self.state.serialize_mods(xkb::STATE_MODS_LATCHED),
            self.state.serialize_mods(xkb::STATE_MODS_LOCKED),
            self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
        )
    }
}
