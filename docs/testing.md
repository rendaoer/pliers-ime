# 测试与联调

[← README](../README.md) · 相关：[词库](dictionary.md#lookup不开输入法看候选) · [踩坑](pitfalls.md)

## 单测：不需要合成器、不需要词库

```bash
cargo test --workspace        # 全部 208 个
cargo test -p pliers-engine   # 只看引擎（160 个）：切词、方案、词库、英文候选、按键状态机
cargo test -p pliers-ime      # 命令行那套（12 个）：状态输出、配置校验、交互选择
cargo test -p pliers-popup    # 候选框布局与像素（18 个）
cargo test -p pliers-wayland  # 模式提示的超时、长按重复的节拍、命令 socket 一问一答（13 个）
```

## 跑起来

```bash
cargo run                     # workspace 里默认就跑 pliers
```

然后把焦点放到输入框里，敲 `n i h a o` 再按空格。能不能用取决于**应用自己有没有
实现 text-input 协议**：Firefox / GTK / Qt 应用都行，alacritty 0.17（基于 winit）
实测也可以。

排查按键问题时加个环境变量，它会把每个按键的判定过程打到 stderr：

```bash
PLIERS_DEBUG=1 cargo run
# pliers: 收到 keycode=30 keysym=0x0061 按下（shift=false caps=false ctrl/alt/super=false 预编辑=""）
# pliers:   → 预编辑 "a"
```

改配置不用重启：另开一个终端 `pliers status` 看现状、`pliers set scheme.kind double-pinyin`
直接切方案 —— 见[配置](config.md#跑着的时候改配置不用重启)。

注意**同一个 seat 上只能有一个输入法**：起第二个实例时，合成器会给旧的那个发
`unavailable`（smithay 的 `InputMethodHandle::add_instance`），旧实例会打印
「有另一个输入法接管了这个 seat」然后退出。所以别同时跑两个。

（词库本身不会打架：turso 允许同一个库开多个连接，我试过两个连接一个写一个读都正常。
真正冲突的只有 Wayland 那个 seat。）

## 只看候选框

```bash
cargo run -p pliers-popup --example dump_popup   # 存成 target/popup.png
```

## mock 合成器（可选，不需要就删掉 `tools/`）

`tools/mock_compositor.py` 是一个 mock 合成器，实现了 wl_display / wl_registry / wl_seat /
wl_compositor / wl_shm / zwp_input_method_v2 / zwp_virtual_keyboard_v1 的够用部分，
可以在没有真合成器、没有物理键盘的情况下跑通整条协议链路。它不只检查提交了什么文本，
还会**把候选框那块 shm 读回来**（数笔画像素、确认真的画了字），并模拟合成器发
`wl_buffer.release`：

```bash
./tools/run_mock_tests.sh                             # 27 个场景 + 2 项在线改配置检查，全绿才算过
./tools/run_mock_tests.sh --png target/popup.png      # 顺便存一张候选框实拍
```

词库默认依次找 `target/dict.db`、`~/.local/share/pliers/dict.db`，也可以 `--dict <路径>` 指定。
测试里的选词会写进 `user_word`，**不想污染自己的词频就指向一份副本**：

```bash
cp ~/.local/share/pliers/dict.db target/dict-copy.db
./tools/run_mock_tests.sh --dict target/dict-copy.db
```

脚本会拷一份词库来用（从不写原始那份），另外**每个场景发一个全新的空 `user.db`**
（`PLIERS_USER_DB`）—— 用户数据跟词库分家之后，"上一个场景选过的词把候选顺序顶乱"
这种事就不可能发生了（以前真踩过：`pick` 选完「事件」，`nav` 里「事件」就跑到第一位），
你自己那份 `user.db` 也不会被测试动到。

### 场景

| 场景 | 喂什么 | 验什么 |
| --- | --- | --- |
| `active` | `nihao ` | 提交「你好」，候选框宽度随候选变化、框里有字 |
| `active_nomods` | 同上，但合成器一个 `modifiers` 事件都不发 | 修饰键状态自己算得出来 |
| `pick` | `shijian2` | 数字选词直接上屏第 2 个候选（「事件」），按键连抬起都不转发 |
| `nav` | `shijian` + `→` + 空格 | 左右挪候选：不改预编辑串，提交第 2 个候选 |
| `pagekey` | `ni` + `↓` + 空格 | 上下整页翻：一下就到第二页，提交的不是第一页的「你」 |
| `page` | `ni` + 9× `→` + 空格 | 左右挪着挪着挪出这一页，会自动翻到下一页 |
| `caps` | Caps Lock 打开后 `nihao ` | 大写不参与匹配：零提交、零候选框、14 个事件全转发 |
| `enter` / `escape` | `nihao` + 回车 / Esc | 原样提交 / 取消，按键不给应用 |
| `mixed` | `aaa` + `Shift+A` + 空格 | 大写并进预编辑（预编辑串到过 `aaaA`，候选框收起），空格整串原样上屏 |
| `shift` | `Shift+A` | 纯大写：4 个事件全转发 |
| `english` | Ctrl+空格 + `hello ` | 切到英文后全部按键原样转发、零提交 |
| `englishword` | `kuber ` | **英文补全**：只敲了 5 个字母，上屏的是补全后的 `kubernetes`（词表编译在二进制里，跟词库无关） |
| `repeat` | `nihao` + 按住退格 1 秒 | **长按重复**：合成器只报了 `repeat_info`，输入法自己按节拍删 —— 预编辑一路 `niha`→`nih`→`ni`→`n`→空，删空之后每一拍转发给应用 |
| `switch` | `nihao` + Ctrl+空格 + `hi` | 切换键把半截拼音 `nihao` 上屏，切完 `hi` 原样转发 |
| `notice` | 只按 Ctrl+空格，之后一个键都不按 | 「英」提示**自己**到点消失（大约半秒后收掉），不是等下一个按键 |
| `symbol` | `nihao` + `/` | **先**提交「你好」**再**把 `/` 转给应用（比请求到达的先后顺序） |
| `punct` | `nihao,` | 一步上屏「你好，」（全角），`,` 不转发给应用 |
| `segment` | `nihaoma` | 分段挑两次拼一句，再打一遍一次上屏（记性）；断言的是"第三遍提交 = 前两段拼起来"，不写死具体字 |
| `forget` | `nihaoma` | 记住的那句被 `Del` 删掉后，退回词库自己给的第一候选「你好吗」 |
| `del_swallow` | `nihaoma` + 3× `Del` + 空格 | 组词当中 `Del` 一律吃掉，一个键都不漏给应用 |
| `hidpi` | 同 `active`，但客户端带 `PLIERS_SCALE=2` | 候选框按 2 倍像素画（高 84）且发了 `set_buffer_scale(2)` |
| `control` | 客户端跑着时 `pliers set` 切小鹤，再喂 `nihc` | 提交的仍是「你好」—— 真的换了引擎；顺带跑一遍交互模式和 `status` |
| `watch` | 直接改配置文件（没人通知它） | 一秒内自动生效；文件写坏了继续用旧配置 |
| `shortcut` / `shortcut_nomods` | Ctrl+A / Ctrl+C | 8 个事件全部转发，不能被当成拼音吃掉 |
| `inactive` / `inactive_nomods` | `nihao ` | 先发 `deactivate`：12 个按键事件全部原样转发、零提交、候选框一次都不弹 |
| `shift_nomods` | `Shift+A` | 不给 modifiers 事件时也是零提交、零预编辑、4/4 转发、不贴框 |

（`\x08` 是退格、`\x1b` 是 Esc。场景名都可以加 `_nomods` 后缀，表示"合成器一个 `modifiers`
事件都不发" —— 用来验证修饰键状态是自己从 keymap 推出来的，而不是等合成器喂。）

### 单跑一个场景

也可以自己拼：第二、三个参数是「要喂的按键」和「期望提交的文本」，第四个是场景。

```bash
python3 tools/mock_compositor.py /tmp/mock-wl "nihao " 你好 &
PLIERS_DICT=target/dict.db WAYLAND_DISPLAY=/tmp/mock-wl cargo run
# mock: pre-edit updates: ['n', 'ni', 'nih', 'niha', 'nihao', '']
# mock: popup shown     : [(90, 42), (168, 42), (318, 42), (318, 42), (318, 42)]
# mock: 框里数出 581 个笔画像素（不透明 13298）
# mock: PASS: committed '你好', expected '你好', popup ok
```

按键串里的特殊键写成 `,` 分隔的名字（`comma` / `period` / `down` / `up` / `del` /
`space` / `enter` / `esc`），场景名和 `run_mock_tests.sh` 里的一致。
