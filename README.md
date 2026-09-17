# ime-aa

最小可用的 Wayland 中文输入法例子：敲 `nihao` + 空格 → 输出「你好」，组词时输入框旁边会
弹出一块候选框。整个实现就是 `src/main.rs` 一个文件，没有词库、没有拼音引擎、没有 GUI 框架。

它直接用 `wayland-client` 跟合成器说协议，不经过 imekit 之类的封装——因为候选框要用
`zwp_input_popup_surface_v2`，而那个 surface 必须在**创建它的那条连接**上画。

## 用了哪些协议

| 协议 | 作用 |
| --- | --- |
| `zwp_input_method_manager_v2` / `zwp_input_method_v2` | 以输入法身份注册；输入框获得焦点时收到 `activate` |
| `zwp_input_method_keyboard_grab_v2` | 抓键盘：抓到手之后所有按键先送到我们这里，合成器不再自己处理 |
| `zwp_virtual_keyboard_v1` | 虚拟键盘：不归我们管的按键，用它原样发回给应用 |
| `zwp_input_popup_surface_v2` + `wl_shm` | 候选框：自己往共享内存画像素，位置交给合成器 |

按键判断用的是 **keysym**（X11 的字符编号：空格 `0x20`、`a`–`z` 是 `0x61..=0x7a`、
退格 `0xff08`），但合成器只给 **keycode**（Linux evdev，空格是 57）。中间的翻译靠合成器
发来的 XKB 键盘布局 + xkbcommon；同一份布局还会原样转给虚拟键盘，这样应用收到的字符
跟真实键盘布局一致。

## 运行

```bash
cargo run
```

然后把焦点放到输入框里，敲 `n i h a o` 再按空格。能不能用取决于**应用自己有没有
实现 text-input 协议**：Firefox / GTK / Qt 应用都行，alacritty 0.17（基于 winit）
实测也可以。

| 按键 | 行为 |
| --- | --- |
| `a`–`z` | 攒进拼音 buffer，并用 `set_preedit_string` 显示为预编辑文本 |
| 空格 | 有拼音就转换并提交（`nihao` → 你好），否则当普通空格转发给应用 |
| 退格 | 删掉一个字母 |
| 其他键 | 用虚拟键盘原样转发，普通打字不受影响 |

要加词，就改 `main.rs` 里那个 `match buffer.as_str()`。

## 四个容易踩的坑

1. **抓到的键盘要自己转发。** 抓取一旦生效，合成器就不再处理任何按键，全丢给我们。
   没有输入框在用（焦点在 XWayland 应用、或不支持 text-input 的应用上）时也必须原样
   转发，否则那些应用直接打不了字 —— 见 `on_key()` 开头的 `!self.active` 分支。
2. **提交是双缓冲的。** `set_preedit_string` / `commit_string` 只改「待生效」状态，
   要再调 `commit(serial)` 才生效。`serial` 必须等于合成器已经发过的 `done` 次数
   （协议原文：*must be equal to the number of done events already issued*），
   所以代码里每收到一个 `done` 就 `serial += 1`。
3. **抓取只需要抓一次。** `deactivate` 之后抓取并不会自己消失（只有连接断开或对象
   被销毁才会），每次 `activate` 都重新抓一遍会攒出好几个 grab，按键可能被重复处理。
4. **不用手动 flush。** 自己管连接的好处：`blocking_dispatch()` 内部会先 flush 再读
   socket，事件处理里发出的请求（预编辑、提交、转发按键、贴候选框）下一轮就会写出去。
   用 imekit 时那个「拿 `commit(serial)` 当 flush 用」的 hack 可以删掉了。

## 候选框是怎么做出来的

1. 有输入框时 `im.get_input_popup_surface(&surface, qh, ())`：合成器给这个 surface
   指派 `input_popup` role。role 一辈子只能定一次（协议：*If the surface already has
   an assigned role, the compositor must issue a protocol error*），所以**不能用任何
   GUI 框架的窗口**——Slint / GTK / Qt / eframe 建的窗口早就是 `xdg_toplevel` 了。
2. **位置不用你管**：niri 会先水平纠偏，再尝试摆在光标下方，放不下就翻到上方
   （`src/handlers/xdg_shell.rs` 的 `PopupKind::InputMethod` 分支）。合成器还会发
   `text_input_rectangle` 告诉你光标相对候选框的位置，那只是个提示，你没法自己挪窗口。
3. **大小 = 你 attach 的 buffer 大小**，内容 = 你自己往 `wl_shm` 里画的像素。
   代码里为每个可能的拼音长度预先建好一块 ARGB8888 缓冲区（一次性放进同一个 shm pool），
   需要时 `attach` + `damage` + `commit`；`attach(None, 0, 0)` 就是隐藏。
4. 因为每块缓冲区的内容只跟长度有关、建好就不改，所以**不需要**处理 `wl_buffer.release`
   事件——要画会变的内容（候选词、动画）时才需要双缓冲 + release 追踪。
5. 现在是 1 倍缩放，HiDPI 屏幕上会糊；要清晰就得 `set_buffer_scale(n)` 并按 n 倍画像素，
   `text_input_rectangle` 给的也是逻辑坐标。

下一步就剩把色条换成文字：`tiny-skia`（画圆角背景）+ `cosmic-text`（中文排版，用系统
Noto Sans CJK），替换 `popup_pixels()` 就行，协议部分不用动。

## 联调工具（可选，不需要就删掉 `tools/`）

`tools/mock_compositor.py` 是一个 mock 合成器，实现了 wl_display / wl_registry / wl_seat /
wl_compositor / wl_shm / zwp_input_method_v2 / zwp_virtual_keyboard_v1 的够用部分，可以在没有
真合成器、没有物理键盘的情况下跑通整条协议链路（连候选框的显示/隐藏一起验）：

```bash
python3 tools/mock_compositor.py /tmp/mock-wl "nihao " 你好 &
WAYLAND_DISPLAY=/tmp/mock-wl cargo run
# mock: pre-edit updates: ['n', 'ni', 'nih', 'niha', 'nihao', '']
# mock: popup shown     : [(74, 56), (100, 56), (126, 56), (152, 56), (178, 56)]
# mock: popup hidden    : 1 times
# mock: PASS: committed '你好', expected '你好', popup ok
```

第二、三个参数是「要喂的按键」和「期望提交的文本」；`\x08` 是退格、`\x1b` 是 Esc。
第四个参数填 `inactive` 会先发一个 `deactivate`（模拟焦点在没有 text-input 的应用上），
这时所有按键都应当被原样转发、不产生任何提交、也不该弹候选框：

```bash
python3 tools/mock_compositor.py /tmp/mock-wl "nihao " "" inactive &
WAYLAND_DISPLAY=/tmp/mock-wl cargo run
# mock: PASS: forwarded 12/12 keys, commits [], popup shown 0
```
