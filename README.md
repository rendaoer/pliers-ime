# ime-aa

最小可用的 Wayland 中文输入法例子：敲 `nihao` + 空格 → 输出「你好」，组词时输入框旁边
弹出一块候选框，里面是「你好 尼好 妮好 拟好」，`↓`/`Tab` 或数字 `1`–`9` 挑，
空格上屏选中的那个。没有词库、没有拼音引擎、没有 GUI 框架。

它直接用 `wayland-client` 跟合成器说协议，不经过 imekit 之类的封装 —— 因为候选框要用
`zwp_input_popup_surface_v2`，而那个 surface 必须在**创建它的那条连接**上画。

## 代码结构

一个 cargo workspace，按"谁管什么"切成四块：

| crate | 管什么 | 依赖 |
| --- | --- | --- |
| `crates/ime-engine` | 按键 → 决定：组词 buffer、候选词、按键账本、什么时候提交 | 无（纯逻辑，可单测） |
| `crates/ime-popup` | 候选框长什么样：找字体、排版、画像素、共享内存文件 | ab_glyph / fontdb + ime-engine |
| `crates/ime-wayland` | 跟合成器说协议：注册、抓键盘、转发按键、贴候选框 | wayland-client / xkbcommon |
| `crates/ime-aa` | `main()`：把上面几个拼起来 | ime-engine + ime-wayland |

```
ime-engine  ←──┬──  ime-popup  ──┐
               └────────────────┴──  ime-wayland  ←──  ime-aa (bin)
```

这么切的好处：**词表、组词规则、候选框长相都能脱离合成器跑测试**，而协议层里剩下的
全是"Wayland 要求这么做"的东西。想改哪块就只动哪块：

* 加个词 → `crates/ime-engine/src/dict.rs` 的 `DICT`
* 换个候选框长相（甚至换成 egui/Slint 画）→ `crates/ime-popup`
* 加协议功能（比如 `delete_surrounding_text`）→ `crates/ime-wayland`

`crates/ime-wayland` 内部也分了三个文件：`lib.rs`（状态 + 各个 Dispatch 实现）、
`keyboard.rs`（keycode→keysym、修饰键状态）、`popup.rs`（候选框 surface）。

## 用了哪些协议

| 协议 | 作用 |
| --- | --- |
| `zwp_input_method_manager_v2` / `zwp_input_method_v2` | 以输入法身份注册；输入框获得焦点时收到 `activate` |
| `zwp_input_method_keyboard_grab_v2` | 抓键盘：抓到手之后所有按键先送到我们这里，合成器不再自己处理 |
| `zwp_virtual_keyboard_v1` | 虚拟键盘：不归我们管的按键，用它原样发回给应用 |
| `zwp_input_popup_surface_v2` + `wl_shm` | 候选框：自己往共享内存画像素，位置交给合成器 |
| `wl_output` | 只为了问一句"屏幕缩放多少"：1.5x/2x 的屏幕要按倍数多画像素，不然字是糊的 |

按键判断用的是 **keysym**（X11 的字符编号：空格 `0x20`、`a`–`z` 是 `0x61..=0x7a`、
退格 `0xff08`），但合成器只给 **keycode**（Linux evdev，空格是 57）。中间的翻译靠合成器
发来的 XKB 键盘布局 + xkbcommon；同一份布局还会原样转给虚拟键盘，这样应用收到的字符
跟真实键盘布局一致。

## 运行与测试

```bash
cargo run                  # 跑主程序（workspace 里默认就跑 ime-aa）
cargo test --workspace     # 引擎 + 候选框的单测（44 个，不需要合成器）
cargo test -p ime-engine   # 只看组词逻辑
./tools/run_mock_tests.sh  # 拿 mock 合成器把 15 个场景跑一遍（含候选框像素）
```

只想看候选框长什么样、不想开输入法：

```bash
cargo run -p ime-popup --example dump_popup   # 存成 target/popup.png
```

然后把焦点放到输入框里，敲 `n i h a o` 再按空格。能不能用取决于**应用自己有没有
实现 text-input 协议**：Firefox / GTK / Qt 应用都行，alacritty 0.17（基于 winit）
实测也可以。

排查按键问题时加个环境变量，它会把每个按键的判定过程打到 stderr：

```bash
IME_AA_DEBUG=1 cargo run
# ime-aa: 收到 keycode=30 keysym=0x0061 按下（shift=false caps=false ctrl/alt/super=false 预编辑=""）
# ime-aa:   → 预编辑 "a"
```

注意**同一个 seat 上只能有一个输入法**：起第二个实例时，合成器会给旧的那个发
`unavailable`（smithay 的 `InputMethodHandle::add_instance`），旧实例会打印
「有另一个输入法接管了这个 seat」然后退出。所以别同时跑两个。

| 按键 | 行为 |
| --- | --- |
| `a`–`z` / `A`–`Z`（Shift、Caps Lock 打出来的大写也一样） | 攒进 buffer，用 `set_preedit_string` 显示为预编辑文本 —— 打字中途**不会**往应用里塞任何字符 |
| 空格 | 有内容就把**选中的候选**提交（`nihao` → 你好，大小写不敏感，`NIHAO` 也算），否则当普通空格转发给应用 |
| `↓` / `Tab` / `↑` / `Shift+Tab` | 换候选（绕圈）。组词当中方向键不去动输入框里的光标 |
| `1`–`9` | 直接选第几个候选上屏（`nihao2` → 尼好）；超出候选个数就什么也不做 |
| 回车 / 小键盘回车 | 有内容就原样提交，**不**把回车给应用（免得顺手提交表单） |
| Esc | 有内容就取消这次组词，这个键谁也不给（免得顺手退出全屏） |
| 退格 | 删掉一个字符 |
| Ctrl / Alt / Super + 任何键 | **不组词**，原样转发（Ctrl+A 全选、Ctrl+C 中断、Alt+F 菜单……） |
| 其他键 | 用虚拟键盘原样转发，普通打字不受影响 |

所以 `aaaa` 之后按住 Shift 敲 `AAAA`，预编辑里就是 `aaaaAAAA`（候选框跟着变长），
按空格/回车才整体落到输入框；查不到词表的内容原样提交，大小写照旧保留。

要加词，就改 `crates/ime-engine/src/dict.rs` 里的 `DICT` —— 每个拼音对应**一串**候选，
第一个是主候选（空格直接上屏的那个）：

```rust
pub const DICT: &[(&str, &[&str])] = &[
    ("nihao", &["你好", "尼好", "妮好", "拟好"]),
    // ...
];
```

## 几个容易踩的坑

1. **抓到的键盘要自己转发。** 抓取一旦生效，合成器就不再处理任何按键，全丢给我们。
   没有输入框在用（焦点在 XWayland 应用、或不支持 text-input 的应用上）时也必须原样
   转发，否则那些应用直接打不了字 —— 见 `ime-engine` 的 `Engine::on_key()` 开头那个
   `!key.active` 分支。
2. **`key_get_one_sym()` 不做 Control 变换。** 按住 Ctrl 时 `a` 解出来**还是 `0x61`**
   （只有 `key_get_utf8()` 会给出 `\x01`）。所以如果只按 keysym 判断字母，Ctrl+A 会变成
   往拼音里塞一个 `a`：Firefox 全选失灵、终端 Ctrl+C 也发不出去。必须靠修饰键状态区分
   （`shortcut_mods`），这是实测出来的：

   ```
   无修饰键： key_get_one_sym = 0x61 ('a')
   按住 Ctrl： key_get_one_sym = 0x61 ('a')      ← 坑在这
   按住 Ctrl： key_get_utf8    = b'\x01'
   ```

   反过来，**大写的 keysym 也不等于按了 Shift**：Caps Lock 打开时 `a` 同样解成 `A`。
   这个玩具输入法把大小写都收进预编辑，所以暂时不需要用这个区别做判断；但以后要做
   "Shift 切英文模式"之类，就必须靠修饰键状态（`shift_held` / `caps_lock`）而不是看 keysym。
3. **修饰键状态自己算，别指望合成器的 `Modifiers` 事件。** smithay 只在"修饰键状态刚变"
   的那一次按键上才带这个事件（`mods_changed.then_some(...)`），拿它当唯一来源非常脆：
   一旦收不到，`shift_held`/`ctrl` 全是 false —— 表现就是 **Ctrl+A 被当成拼音的 a 吃掉**。
   正确做法跟普通客户端一样：自己拿 keymap 建一个 `xkb::State`，每个按键事件喂
   `update_key(keycode, Down/Up)`，修饰键、Caps Lock 全都从这推出来
   （`crates/ime-wayland/src/keyboard.rs`）。
   注意 xkb 文档说 `update_key` 和 `update_mask` 不要混用，所以 `Modifiers` 事件只用来
   转发给虚拟键盘，不再喂我们自己的状态。
4. **预编辑状态下别把字符键转发给应用，也别中途往应用里塞字符。** 应用这时正处在
   "组词"状态里，对转发过去的按键反应不可靠：实测 Firefox 里输入 `aaa`（预编辑挂着）
   再按 `Shift+A`，我们明明把 `key(Shift)` + `modifiers(0x1)` + `key(a)` 都发了出去，
   输入框里却 **A 和 a 都不出现**。所以字母（含大写）一律进 buffer 当预编辑，只在
   空格/回车时用 `commit_string` 整体交给应用。转发只留给"命令类"按键：方向键、
   Ctrl/Alt/Super 快捷键，以及没有输入框时的全部按键。
5. **应用的大写字母靠虚拟键盘的 `modifiers` 请求。** smithay 的 `zwp_virtual_keyboard_v1.key()`
   只发 `wl_keyboard.key`，**不会**顺手更新修饰键状态；只有 `modifiers()` 请求才会给应用
   发 `wl_keyboard.modifiers`。所以输入法自己推出掩码后要用
   `serialize_mods()` 算出来再发一遍（变了才发），否则应用收到 `Shift+a` 也只会打小写。
   （字母走预编辑之后，这条主要影响快捷键，比如 Ctrl+C。）
6. **Caps Lock 是靠按键事件翻转的。** 合成器可能只给你一个 `modifiers` 事件说"Lock 位亮了"，
   但真键盘上是先有 Caps Lock 键（keycode 58）的按下，xkb 才翻转 Lock。自己喂 `update_key`
   两种来源就都能覆盖。
7. **提交是双缓冲的。** `set_preedit_string` / `commit_string` 只改「待生效」状态，
   要再调 `commit(serial)` 才生效。`serial` 必须等于合成器已经发过的 `done` 次数
   （协议原文：*must be equal to the number of done events already issued*），
   所以代码里每收到一个 `done` 就 `serial += 1`。
8. **抓取只需要抓一次。** `deactivate` 之后抓取并不会自己消失（只有连接断开或对象
   被销毁才会），每次 `activate` 都重新抓一遍会攒出好几个 grab，按键可能被重复处理。
9. **不用手动 flush。** 自己管连接的好处：`blocking_dispatch()` 内部会先 flush 再读
   socket，事件处理里发出的请求（预编辑、提交、转发按键、贴候选框）下一轮就会写出去。
   用 imekit 时那个「拿 `commit(serial)` 当 flush 用」的 hack 可以删掉了。

## 候选框是怎么做出来的

**协议那半边**（`crates/ime-wayland/src/popup.rs`）：

1. 有输入框时 `im.get_input_popup_surface(&surface, qh, ())`：合成器给这个 surface
   指派 `input_popup` role。role 一辈子只能定一次（协议：*If the surface already has
   an assigned role, the compositor must issue a protocol error*），所以**不能用任何
   GUI 框架的窗口**——Slint / GTK / Qt / eframe 建的窗口早就是 `xdg_toplevel` 了。
2. **位置不用你管**：niri 会先水平纠偏，再尝试摆在光标下方，放不下就翻到上方
   （`src/handlers/xdg_shell.rs` 的 `PopupKind::InputMethod` 分支）。合成器还会发
   `text_input_rectangle` 告诉你光标相对候选框的位置，那只是个提示，你没法自己挪窗口。
3. **大小 = 你 attach 的 buffer 大小**，内容 = 你自己往 `wl_shm` 里画的像素；
   `attach(None, 0, 0)` 就是隐藏。
4. **缓冲区要轮换。** 候选词每敲一个字就变，不能一边被合成器显示一边改写那块内存。
   所以只在收到 `wl_buffer.release` 之后才重画那块 buffer（`Slot.busy`）：宽度没变
   就原地重画，宽度变了才新建一块，不用的旧块扔掉——这才是双缓冲该有的样子。
5. **HiDPI 自己处理。** 屏幕是 1.5x / 2x 时按倍数多画几倍像素（本机 niri 是 1.5x，
   它报 2），再用 `set_buffer_scale(2)` 告诉合成器"这块 buffer 是 2 倍密度"。
   注意 `damage()` 用的是 surface 坐标（逻辑像素），不是 buffer 像素，要除回去。

**内容那半边**（`crates/ime-popup`）：

6. **字体去系统里找**：`fontdb` 扫 `/usr/share/fonts` 这些目录，优先 Noto Sans CJK SC
   之类的常见中文字体；找不到就只画个空框，不报错。`IME_AA_FONT=/path/to/font.ttc`
   可以指定一套。
7. **光栅化用 `ab_glyph`**（字符 → 轮廓 → 灰度图）。这里有个坑：它的 `PxScale` 是
   **行高**不是字号，而中文行高是字号的 1.4 倍上下，直接填字号字会小一圈 ——
   代码里先乘一个 `height_unscaled / units_per_em` 换算（`font.rs` 的 `ratio`）。
8. **竖直居中按"墨迹"算**：把所有字的覆盖率图的上下边界并起来取中点，而不是用字体的
   ascent/descent —— 中文行高留白很大，按行高居中看着会偏上。
9. **画布就是一块 ARGB8888**：`wl_shm` 的 ARGB8888 在内存里是 **B,G,R,A**，而且 alpha
   是**预乘**的（50% 透明的白要存成 `(128,128,128,128)`，存成 `(255,255,255,128)`
   会亮一截）。圆角用"点到圆角矩形的距离"当覆盖率，天然抗锯齿（`paint.rs`）。

## 已知不足

- 组词当中按 Ctrl/Alt/Super 快捷键时，按键虽然正确转发给应用了，预编辑串还挂在那儿
  （正经输入法会先把拼音提交或取消掉）。
- 候选超过 9 个要翻页，现在只显示前 9 个（`MAX_ITEMS`）。
- 词表是手写的十几条，没有拼音切分、词频、模糊音；`nihao` 之外的词得自己往 `DICT` 里加。
- 缩放只取整数倍（1.5x 的屏幕用 2 倍 buffer 再让合成器缩回去，够清楚但不是像素级完美）；
  要做到完美得配 `wp_fractional_scale_v1` + `wp_viewporter`。
- 字体只加载一套，没有逐字回退：某个字缺字形就是空白。

## 联调工具（可选，不需要就删掉 `tools/`）

`tools/mock_compositor.py` 是一个 mock 合成器，实现了 wl_display / wl_registry / wl_seat /
wl_compositor / wl_shm / zwp_input_method_v2 / zwp_virtual_keyboard_v1 的够用部分，
可以在没有真合成器、没有物理键盘的情况下跑通整条协议链路。它不只检查提交了什么文本，
还会**把候选框那块 shm 读回来**（数笔画像素、确认真的画了字），并模拟合成器发
`wl_buffer.release`：

```bash
./tools/run_mock_tests.sh                       # 15 个场景，全绿才算过
MOCK_PNG=target/popup.png ./tools/run_mock_tests.sh   # 顺便存一张候选框实拍
```

也可以单跑一个场景，第二、三个参数是「要喂的按键」和「期望提交的文本」
（上面那串宽度里 300 是 `ni` —— 它本身就是词表里的词，一次冒出 5 个候选，框就宽了）：

```bash
python3 tools/mock_compositor.py /tmp/mock-wl "nihao " 你好 &
WAYLAND_DISPLAY=/tmp/mock-wl cargo run
# mock: pre-edit updates: ['n', 'ni', 'nih', 'niha', 'nihao', '']
# mock: popup shown     : [(64, 42), (300, 42), (80, 42), (91, 42), (318, 42)]
# mock: 框里数出 537 个笔画像素（不透明 13298）
# mock: PASS: committed '你好', expected '你好', popup ok
```

第四个参数是场景（`active` / `inactive` / `shortcut` / `shift` / `mixed` / `enter` /
`escape` / `caps` / `pick` / `nav` / `hidpi`，都可以加 `_nomods` 后缀表示"合成器一个
modifiers 事件都不发"）：

| 场景 | 喂什么 | 验什么 |
| --- | --- | --- |
| `active` | `nihao ` | 提交「你好」，候选框宽度随候选变化、框里有字 |
| `pick` | `nihao2` | 数字选词直接上屏「尼好」，按键连抬起都不转发 |
| `nav` | `nihao` + `↓` + 空格 | 换候选不改预编辑串，提交「尼好」 |
| `hidpi` | 同 `active`，但客户端带 `IME_AA_SCALE=2` | 候选框按 2 倍像素画（高 84）且发了 `set_buffer_scale(2)` |
| `inactive` | `nihao ` | 先发 `deactivate`：12 个按键事件全部原样转发、零提交、候选框一次都不弹 |
| `shortcut` | Ctrl+A / Ctrl+C | 8 个事件全部转发，不能被当成拼音吃掉 |
| `mixed` | `aaa` + `Shift+A` + 空格 | 整个 `aaaA` 只在最后提交一次 |
| `caps` | Caps Lock 打开后 `nihao ` | 大写照样能查表 → 「你好」 |
| `enter` / `escape` | `nihao` + 回车 / Esc | 原样提交 / 取消，按键不给应用 |

（`\x08` 是退格、`\x1b` 是 Esc。）
