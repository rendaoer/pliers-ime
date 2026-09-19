# 踩过的坑 + 已知不足

[← README](../README.md) · 相关：[实现](internals.md) · [使用](usage.md)

## 几个容易踩的坑

1. **抓到的键盘要自己转发。** 抓取一旦生效，合成器就不再处理任何按键，全丢给我们。
   没有输入框在用（焦点在 XWayland 应用、或不支持 text-input 的应用上）时也必须原样
   转发，否则那些应用直接打不了字 —— 见 `pliers-engine` 的 `Engine::on_key()` 开头那个
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
   这个玩具输入法干脆**只收小写**：keysym 落在 `A`–`Z` 就按"普通英文字符"处理，不再去猜
   它到底是 Shift 还是 Caps Lock 弄出来的。要做"Shift 切英文模式"之类，才必须靠修饰键状态
   （`shift_held` / `caps_lock`）而不是看 keysym。
3. **修饰键状态自己算，别指望合成器的 `Modifiers` 事件。** smithay 只在"修饰键状态刚变"
   的那一次按键上才带这个事件（`mods_changed.then_some(...)`），拿它当唯一来源非常脆：
   一旦收不到，`shift_held`/`ctrl` 全是 false —— 表现就是 **Ctrl+A 被当成拼音的 a 吃掉**。
   正确做法跟普通客户端一样：自己拿 keymap 建一个 `xkb::State`，每个按键事件喂
   `update_key(keycode, Down/Up)`，修饰键、Caps Lock 全都从这推出来
   （`crates/pliers-wayland/src/keyboard.rs`）。
   注意 xkb 文档说 `update_key` 和 `update_mask` 不要混用，所以 `Modifiers` 事件只用来
   转发给虚拟键盘，不再喂我们自己的状态。
4. **预编辑状态下别把字符键转发给应用。** 应用这时正处在"组词"状态里，对转发过去的按键
   反应不可靠：实测 Firefox 里输入 `aaa`（预编辑挂着）再按 `Shift+A`，我们明明把
   `key(Shift)` + `modifiers(0x1)` + `key(a)` 都发了出去，输入框里却 **A 和 a 都不出现**。
   所以组词当中来的字符只有两条路：小写字母进 buffer 当拼音；**大写字母、数字、符号
   并进 buffer 当"英文原文"**（`n` + `Shift+N` → `nN`，空格原样上屏）。真要往应用里
   送字符（敲 `/` 这种明确表示"组词结束"的符号），得**先把组词收掉**
   （`Action::CommitAndForward`：先 `commit_string` 再转发，应用那边就是"文字在前、符号在后"）。
   转发只留给"命令类"按键：方向键、Ctrl/Alt/Super 快捷键，以及没有组词时的全部按键。
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
10. **焦点一走，拼音就再也交不出去了。** 合成器是先发 `deactivate`、紧接着就收掉这次
    text-input 会话（smithay：`deactivate_input_method()` 之后马上 `text_input_leave()`，
    活动 text-input 被清空；GTK 那边收到 `leave` 就把预编辑丢了，源码里还写着
    "after disable, incoming state changes won't take effect anyway"）。等我们读到
    `deactivate` 再 `commit_string`，要么没人收，要么落到**下一个**输入框里 —— 后者更糟，
    所以这里只能丢掉（`im::Event::Deactivate` 分支，`PLIERS_DEBUG=1` 会 print 出来丢了什么）。
    所有外挂式 Wayland 输入法都躲不过这一条：**想保住打了一半的拼音，就在切窗口前按空格
    或者回车把它收掉**。能救的场合我们都救了：切中英文、敲符号都会先上屏。

11. **转发出去的按下，它的抬起也得转发** —— 而"长按重复"会让这变成一个来回摆的决定。
    自动重复是输入法自己的活（见[实现](internals.md#按住不放重复得自己做)），所以一个物理
    按键会被喂很多次"按下"：按住退格删预编辑时前几拍被我们吃掉，预编辑删空之后每一拍又
    **转发**给应用。抬起只有一次，它必须跟着**最后一次**决定走。
    一开始的写法是"按下时记一笔账，抬起时按账本决定吃不吃"，结果账本里还留着"吃掉"
    那一笔 —— 抬起被吞了，应用以为退格一直按着：**GTK 的 Wayland 后端和 X11/XWayland
    都会替按住不放的键自动重复**（GTK 源码里就是 `keyboard_repeat` + `server_repeat_rate`），
    表现是"我明明松手了，再输入什么都被删掉"。修法是在 `Engine::on_key()` 外面套一层
    （见那条注释）：这一次的 `Action` 是转发，就把账本撤掉。

## 已知不足

* **没有简拼**（`nh` → 你好）、**没有模糊音**（zh=z、an=ang）、**没有联想**（上屏之后猜下一个词）。
  这三样都得再动 `scheme.rs` 和词库索引。
* **多音字**：rime 词库（白霜拼音）里**一行一个读音**、各自带语料统计出来的权重，
  所以长=chang/zhang、行=hang/xing、乐=yue/le 都是现成的，打 `chang` 第一个就是「长」。
  这是换掉 IBus 词表（词表没有读音标注，只能靠"次要读音占该字条目数的百分之几"去猜）
  的主要原因之一。
* **分段候选只挑"最长的那一段" + 最短的单音节那段**：`nihaoma` 会先给「你好」相关的
  （吃前 5 个字符）和「你」（吃 2 个），中间长度的切法不会一次列出来 —— 想要「你」+「号码」
  这种切法，得靠整句候选或者用户自己记住。
* **整句候选是词频一元模型，没有语言模型**：词库里没有整词时，同一个拼音串可能先给一个
  不太对的切法。换 rime 词库之后好很多（`nihaoma` 在库里就有「你好吗」，直接命中排第一）。
  选过一次正确的之后 `user_word` 会把它顶上去。要根治得加二元语言模型。
* **五笔只有骨架**：码表方案（`Table`）读的是 `scheme='wubi'` 的行，但仓库里没有码表数据；
  而且码表查询是**前缀**查询（打 `w` 要出所有 w 开头的字），正好是最慢的那种，
  真要用得在导入时按前缀预先算好 top-N。
* 词库是**一次性投入，但不算贵**：rime 词库约 100 万条，导入 7 秒、库 86 MB。
  改词频想重新导入，等几秒就行。
* 组词当中按 Ctrl/Alt/Super 快捷键时，按键虽然正确转发给应用了，预编辑串还挂在那儿
  （正经输入法会先把拼音提交或取消掉）。
* **焦点离开输入框时，正在组的拼音只能丢掉**（不是没实现，是协议不给机会：合成器发
  `deactivate` 的同时就把这次 text-input 会话收掉了，见上面第 10 条）。切中英文、敲符号
  这两条路已经改成"先上屏"了。
* 翻页只翻 `pool_size`（默认 90 个 = 10 页）那么深，再往后的候选没取。
* **英文候选是"中文模式里的英文补全"，不是一个英文输入法**：只认小写（大写走英文原文那条路）、
  只在切不出拼音时才出、按空格上屏的是补全后的词（想原样按回车）。写整段英文还是按
  Ctrl+空格 切英文模式省心。词表是通用英语 + 开发常用词，**专业词和项目内部术语得自己加**
  （`[english] path`）；词表里也难免带点字幕语料的名字和俚语，看着不顺眼就按 `Del` 拉黑。
* 英文候选目前**不参与"整句"**：它只按前缀补一个词，没有"打了半句英文再补"这回事。
* 缩放只取整数倍（1.5x 的屏幕用 2 倍 buffer 再让合成器缩回去，够清楚但不是像素级完美）。
* 字体只加载一套，没有逐字回退：某个字缺字形就是空白。
