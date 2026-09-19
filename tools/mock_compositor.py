#!/usr/bin/env python3
"""A tiny mock Wayland compositor for exercising pliers's live mode.

It implements just enough of the wire protocol -- wl_display / wl_registry /
wl_seat / wl_compositor / wl_shm / zwp_input_method_v2 / zwp_virtual_keyboard_v1
-- to act as a compositor that has a focused text field:

  1. it advertises the globals imekit binds,
  2. it activates the input method,
  3. it waits for the keyboard grab request,
  4. it sends an XKB keymap and then feeds a key sequence,
  5. it prints every pre-edit / commit / forwarded key the client sends back,
  6. it accepts the client's candidate-window surface (input_popup role),
     reports when the client shows / hides it, and reads the pixels back --
     so a test can check the popup really contains rendered glyphs, not just
     a coloured rectangle. Set MOCK_PNG=<path> to save the last frame as a PNG.

This is how the live path can be tested without a compositor that supports
input methods and without a physical keyboard:

    python3 tools/mock_compositor.py /tmp/mock-wl "nihao " 你好 &
    WAYLAND_DISPLAY=/tmp/mock-wl cargo run

Note on the wire format: file descriptors travel as ancillary data and take
*no* slot in the message body, so the keymap message is just
[format][size] plus one fd.

Usage: mock_compositor.py <socket-path> [keys] [expected-committed-text] [mode]

Modes: active | inactive | shortcut | shift | mixed | enter | escape | caps |
       pick (数字选词) | nav (方向键换候选) | page (翻页) | symbol (组词中敲符号) |
       englishword (英文补全：kuber + 空格 → kubernetes) |
       switch (组词中切中英文) | control (先等一会儿再打字，留给 `pliers set` 用) |
       watch (等 6 秒，留给"改配置文件看它自动重读"用) |
       segment (分段上屏 + 记住拼出来的句子) | forget (Del 忘掉自己拼的句子) |
       script (KEYS 写成"按键脚本"：字母照写，特殊键写 down/up/del/space/enter/esc，逗号分隔),
       each optionally with a _nomods suffix.
"""
import array
import mmap
import os
import select
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

SOCK_PATH = sys.argv[1] if len(sys.argv) > 1 else "/tmp/mock-wl"
KEYS = sys.argv[2] if len(sys.argv) > 2 else "nihao "
EXPECT = sys.argv[3] if len(sys.argv) > 3 else "你好"
# active   = 有输入框（正常组词转换）
# inactive = 先发 deactivate，模拟焦点在没有 text-input 的应用上
MODE = sys.argv[4] if len(sys.argv) > 4 else "active"

# evdev keycodes
EVDEV = {
    "a": 30, "b": 48, "c": 46, "d": 32, "e": 18, "f": 33, "g": 34, "h": 35,
    "i": 23, "j": 36, "k": 37, "l": 38, "m": 50, "n": 49, "o": 24, "p": 25,
    "q": 16, "r": 19, "s": 31, "t": 20, "u": 22, "v": 47, "w": 17, "x": 45,
    "y": 21, "z": 44, " ": 57, "\x08": 14, "\n": 28, "\x1b": 1,
    # 数字 1-9：输入法用它们直接选候选
    "1": 2, "2": 3, "3": 4, "4": 5, "5": 6, "6": 7, "7": 8, "8": 9, "9": 10,
    # 符号：组词当中敲它们时，输入法要先把候选上屏、再把这个键转过来
    "/": 53, "0": 11, ";": 39, "'": 40, "[": 26, "]": 27,
    # 标点（中文标点开着的时候，这几个打成全角）
    ",": 51, ".": 52,
}
# 方向键（组词时用来翻候选）
EVDEV_DOWN = 108
EVDEV_UP = 103
EVDEV_RIGHT = 106
# Del（候选框里按它 = 忘掉自己拼的句子）
EVDEV_DELETE = 111

# 候选框高度（逻辑像素）：pliers_popup 的版面高度，mock 这边没有 wl_output 所以缩放是 1
POPUP_HEIGHT = 42

# Request signatures. "H" is a file descriptor: it consumes no message body
# bytes, it is taken from the received ancillary data.
REQ = {
    ("wl_display", 0): "n",                        # sync
    ("wl_display", 1): "n",                        # get_registry
    ("wl_registry", 0): "usun",                    # bind
    ("wl_seat", 3): "",                            # release
    ("zwp_input_method_manager_v2", 0): "on",      # get_input_method
    ("zwp_input_method_manager_v2", 1): "",        # destroy
    ("zwp_input_method_v2", 0): "s",               # commit_string
    ("zwp_input_method_v2", 1): "sii",             # set_preedit_string
    ("zwp_input_method_v2", 2): "uu",              # delete_surrounding_text
    ("zwp_input_method_v2", 3): "u",               # commit(serial)
    ("zwp_input_method_v2", 4): "on",              # get_input_popup_surface
    ("zwp_input_method_v2", 5): "n",               # grab_keyboard
    ("zwp_input_method_v2", 6): "",                # destroy
    ("zwp_virtual_keyboard_manager_v1", 0): "on",  # create_virtual_keyboard
    ("zwp_virtual_keyboard_manager_v1", 1): "",    # destroy
    ("zwp_virtual_keyboard_v1", 0): "uHu",         # keymap(format, fd, size)
    ("zwp_virtual_keyboard_v1", 1): "uuu",         # key(time, key, state)
    ("zwp_virtual_keyboard_v1", 2): "uuuu",        # modifiers
    ("zwp_virtual_keyboard_v1", 3): "",            # destroy
    # 候选框相关：compositor + shm + surface
    ("wl_compositor", 0): "n",                     # create_surface
    ("wl_compositor", 1): "no",                    # create_region
    ("wl_shm", 0): "nHu",                          # create_pool(id, fd, size)
    ("wl_shm_pool", 0): "niiiiu",                  # create_buffer(id, offset, w, h, stride, format)
    ("wl_shm_pool", 1): "",                        # destroy
    ("wl_surface", 0): "",                         # destroy
    ("wl_surface", 1): "oii",                      # attach(buffer, x, y)
    ("wl_surface", 2): "iiii",                     # damage
    ("wl_surface", 6): "",                         # commit
    ("wl_surface", 8): "i",                        # set_buffer_scale
    ("wl_buffer", 0): "",                          # destroy
}


def pad(n):
    return (-n) % 4


def read_pixels(fd, size):
    """把客户端那块 shm 读出来（候选框的像素就在里面）"""
    try:
        mapped = mmap.mmap(fd, size, prot=mmap.PROT_READ)
    except (OSError, ValueError) as e:
        print(f"mock: 读不了 shm：{e}", flush=True)
        return None
    data = mapped[:]
    mapped.close()
    return data


def count_glyph_pixels(data, offset, width, height, stride):
    """数"明显是字"的像素：不透明、而且接近白色（未选中的候选是浅色字）。

    框底是深灰、选中项是橙块，都不满足"三个通道都亮"，所以数出来的就是笔画。
    """
    inked = opaque = 0
    for y in range(height):
        row = offset + y * stride
        for x in range(width):
            i = row + x * 4
            b, g, r, a = data[i], data[i + 1], data[i + 2], data[i + 3]
            if a > 0:
                opaque += 1
            if a > 200 and r > 150 and g > 150 and b > 150:
                inked += 1
    return inked, opaque


def write_png(path, data, offset, width, height, stride, zoom=3):
    """把一帧候选框存成 PNG：放大 zoom 倍，底下垫棋盘格，透明的地方看得见"""
    out_w, out_h = width * zoom, height * zoom
    raw = bytearray()
    for y in range(out_h):
        raw.append(0)  # 每行的 filter 字节
        for x in range(out_w):
            i = offset + (y // zoom) * stride + (x // zoom) * 4
            b, g, r, a = data[i], data[i + 1], data[i + 2], data[i + 3]
            keep = 255 - a
            # 棋盘格是 8 个逻辑像素一格
            dark = ((x // (8 * zoom)) + (y // (8 * zoom))) % 2 == 0
            bg = 0x55 if dark else 0x99
            # 候选框的像素是预乘过的 BGRA，直接 over 上去
            raw += bytes([min(255, r + bg * keep // 255),
                          min(255, g + bg * keep // 255),
                          min(255, b + bg * keep // 255), 255])
    chunk = lambda tag, body: (struct.pack(">I", len(body)) + tag + body
                               + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF))
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", out_w, out_h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
           + chunk(b"IEND", b""))
    with open(path, "wb") as f:
        f.write(png)
    print(f"mock: 候选框存到 {path}（{out_w}x{out_h}）", flush=True)


class Conn:
    def __init__(self, sock):
        self.sock = sock
        self.buf = b""
        self.fds = []

    def send(self, obj, opcode, payload=b"", fds=()):
        msg = struct.pack("<II", obj, ((8 + len(payload)) << 16) | opcode) + payload
        if fds:
            self.sock.sendmsg([msg], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array("i", fds))])
        else:
            self.sock.sendall(msg)

    def send_string(self, text):
        raw = text.encode() + b"\0"
        return struct.pack("<I", len(raw)) + raw + b"\0" * pad(len(raw))

    def read_messages(self):
        """Read everything available; None means the client went away."""
        while True:
            ready, _, _ = select.select([self.sock], [], [], 0)
            if not ready:
                break
            data, ancdata, _, _ = self.sock.recvmsg(65536, socket.CMSG_SPACE(4 * 16))
            for level, kind, cdata in ancdata:
                if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                    arr = array.array("i")
                    arr.frombytes(cdata[: len(cdata) - (len(cdata) % 4)])
                    self.fds.extend(arr)
            if not data:
                return None
            self.buf += data

        out = []
        while len(self.buf) >= 8:
            obj, size_op = struct.unpack_from("<II", self.buf, 0)
            size = size_op >> 16
            opcode = size_op & 0xFFFF
            if len(self.buf) < size:
                break
            out.append((obj, opcode, self.buf[8:size]))
            self.buf = self.buf[size:]
        return out


def parse_args(signature, body, fds):
    values = []
    offset = 0
    for kind in signature:
        if kind in "uino":
            (value,) = struct.unpack_from("<I", body, offset)
            values.append(value)
            offset += 4
        elif kind == "H":
            values.append(fds.pop(0) if fds else None)
        elif kind == "s":
            (length,) = struct.unpack_from("<I", body, offset)
            offset += 4
            values.append(body[offset : offset + length - 1].decode("utf-8", "replace"))
            offset += length + pad(length)
        else:
            raise ValueError(f"unsupported signature element {kind}")
    return values


def main():
    try:
        os.unlink(SOCK_PATH)
    except FileNotFoundError:
        pass

    keymap_text = subprocess.run(
        ["xkbcli", "compile-keymap"], capture_output=True, check=True
    ).stdout
    keymap_file = tempfile.NamedTemporaryFile(delete=False, suffix=".xkb")
    keymap_file.write(keymap_text)
    keymap_file.flush()

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(SOCK_PATH)
    server.listen(1)
    print(f"mock: listening on {SOCK_PATH}, will type {KEYS!r}", flush=True)

    # 客户端迟迟不来（比如它启动就失败了）别把测试挂死：等 20 秒就走
    server.settimeout(20)
    try:
        client, _ = server.accept()
    except TimeoutError:
        print("mock: 20 秒了还没有客户端连上来，退出", flush=True)
        return 2
    conn = Conn(client)

    objects = {}
    pending_sync = []
    synced = 0
    im_id = vk_id = grab_id = None
    log = []
    # 候选框：pool 的 fd 要一直留着，buffer 记住尺寸，attach 序列用来判定显示/隐藏。
    # frames 里留着每帧的像素，用来数"框里到底有没有字"
    shm_fds = []
    pools = {}
    buffers = {}
    frames = []
    popup_id = None
    popup_events = []
    # 客户端最后告诉我们的 buffer 缩放（1 = 普通屏，2 = 高分屏）
    buffer_scale = 1
    deadline = time.time() + 20

    while time.time() < deadline:
        messages = conn.read_messages()
        if messages is None:
            break
        for obj, opcode, body in messages:
            iface = objects.get(obj, "wl_display" if obj == 1 else "?")
            signature = REQ.get((iface, opcode))
            if signature is None:
                print(f"mock: ?? {iface} opcode={opcode} body={body!r}", flush=True)
                continue
            args = parse_args(signature, body, conn.fds)

            if iface == "wl_display" and opcode == 1:                 # get_registry
                objects[args[0]] = "wl_registry"
                for name, interface, version in (
                    (1, "wl_compositor", 6),
                    (2, "wl_seat", 9),
                    (3, "zwp_input_method_manager_v2", 1),
                    (4, "zwp_virtual_keyboard_manager_v1", 1),
                    (5, "wl_shm", 1),
                ):
                    conn.send(
                        args[0], 0,
                        struct.pack("<I", name) + conn.send_string(interface) + struct.pack("<I", version),
                    )
            elif iface == "wl_display" and opcode == 0:               # sync
                objects[args[0]] = "wl_callback"
                pending_sync.append(args[0])
            elif iface == "wl_registry" and opcode == 0:              # bind
                _, interface, _, new_id = args
                objects[new_id] = interface
                if interface == "wl_seat":
                    conn.send(new_id, 1, conn.send_string("seat-0"))
            elif iface == "zwp_input_method_manager_v2" and opcode == 0:
                im_id = args[1]
                objects[im_id] = "zwp_input_method_v2"
            elif iface == "zwp_virtual_keyboard_manager_v1" and opcode == 0:
                vk_id = args[1]
                objects[vk_id] = "zwp_virtual_keyboard_v1"
            elif iface == "wl_compositor" and opcode == 0:           # create_surface
                objects[args[0]] = "wl_surface"
            elif iface == "wl_shm" and opcode == 0:                  # create_pool
                objects[args[0]] = "wl_shm_pool"
                shm_fds.append(args[1])                              # 别让 fd 被回收
                pools[args[0]] = (args[1], args[2])                  # fd, size
                print(f"mock: shm pool id={args[0]} size={args[2]}", flush=True)
            elif iface == "wl_shm_pool" and opcode == 0:             # create_buffer
                objects[args[0]] = "wl_buffer"
                buffers[args[0]] = args[1:] + [pools.get(obj)]       # offset,w,h,stride,fmt,pool
            elif iface == "zwp_input_method_v2" and opcode == 4:     # get_input_popup_surface
                popup_id = args[0]
                objects[popup_id] = "zwp_input_popup_surface_v2"
                print("mock: got popup surface request", flush=True)
            elif iface == "wl_surface" and opcode == 8:              # set_buffer_scale
                buffer_scale = args[0]
                print(f"mock: buffer scale = {buffer_scale}", flush=True)
            elif iface == "wl_surface" and opcode == 1:              # attach
                buffer_id = args[0] if args[0] != 0 else None
                if buffer_id is None:
                    popup_events.append(("hide", time.monotonic()))
                    print("mock: popup hidden (attach NULL)", flush=True)
                else:
                    offset, w, h, stride, fmt, pool = buffers.get(
                        buffer_id, (0, 0, 0, 0, 0, None)
                    )
                    # 最后一个字段是时刻：`notice` 场景要量"提示挂了多久才自己消失"
                    popup_events.append(("show", w, h, stride, fmt, time.monotonic()))
                    print(f"mock: popup shown {w}x{h} stride={stride} format={fmt}", flush=True)
                    # 把这块 shm 读出来数一数字的像素：能验到"候选框里真有字"
                    if pool is not None:
                        data = read_pixels(pool[0], pool[1])
                        if data is not None and offset + h * stride <= len(data):
                            inked, opaque = count_glyph_pixels(data, offset, w, h, stride)
                            frames.append((w, h, data, offset, stride, inked))
                            print(f"mock: 框里数出 {inked} 个笔画像素（不透明 {opaque}）", flush=True)
                    # 真合成器渲染完就会 release，客户端才能重画这块内存
                    conn.send(buffer_id, 0)
                    # 合成器会顺带告知光标矩形（相对候选框左上角）
                    if popup_id is not None:
                        conn.send(popup_id, 0, struct.pack("<iiii", 0, -h, 2, 24))
            elif iface == "zwp_input_method_v2" and opcode == 5:      # grab_keyboard
                grab_id = args[0]
                objects[grab_id] = "zwp_input_method_keyboard_grab_v2"
                print("mock: got keyboard grab request", flush=True)
                # XKB keymap: body is [format][size], the fd is ancillary data.
                conn.send(grab_id, 0, struct.pack("<II", 1, len(keymap_text)), fds=[keymap_file.fileno()])
                conn.send(grab_id, 3, struct.pack("<ii", 25, 600))          # repeat_info
                if not MODE.endswith("_nomods"):
                    conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))  # modifiers
                if MODE.removesuffix("_nomods") == "inactive":
                    conn.send(im_id, 1)  # deactivate
                    print("mock: sending deactivate before the keys", flush=True)
                time.sleep(0.3)
                # xxx_nomods = 合成器一个 modifiers 事件都不发（真机上的情况），
                # 用来验证输入法自己按 keycode 推修饰键状态也照样能工作
                base_mode = MODE.removesuffix("_nomods")
                send_mods = MODE == base_mode
                if base_mode == "shortcut":
                    # Ctrl+A、Ctrl+C：必须原样转发，不能被当成拼音的 a / c 吃掉。
                    # 修饰键掩码用的是 XKB 固定位序：Shift=1 Lock=2 Control=4 Mod1=8 … Mod4=64
                    ctrl = 29
                    for code in (EVDEV["a"], EVDEV["c"]):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 1))
                        if send_mods:
                            conn.send(grab_id, 2, struct.pack("<IIIII", 0, 4, 0, 0, 0))
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, code, 1))
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, code, 0))
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 0))
                        if send_mods:
                            conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))
                        time.sleep(0.05)
                elif base_mode == "mixed":
                    # 组词当中按 Shift+A：大写字母并进预编辑（不能当场提交，
                    # 更不能先蹦出「你」再补个 A），空格才把整串 "aaaA" 原样上屏
                    for ch in KEYS:
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                    shift = 42
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, shift, 1))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 1, 0, 0, 0))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV["a"], 1))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV["a"], 0))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, shift, 0))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))
                    time.sleep(0.05)
                    for st in (1, 0):  # 空格：这时候才把 "aaaA" 上屏
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.03)
                elif base_mode == "shift":
                    # Shift+A：XKB 里 Shift = 1，字母 keysym 变成大写 'A'。
                    # 大写不参与匹配 → 没进组词、没候选，四个按键事件全转发
                    shift = 42
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, shift, 1))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 1, 0, 0, 0))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV["a"], 1))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV["a"], 0))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, shift, 0))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))
                elif base_mode == "english":
                    # Ctrl+空格 切到英文，再打一句英文。切完之后所有键都该原样转发：
                    # 不进组词、不提交、也不该有非空的预编辑
                    ctrl = 29
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 1))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 4, 0, 0, 0))
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.05)
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 0))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))
                    time.sleep(0.1)
                    for ch in KEYS:
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                elif base_mode == "notice":
                    # 只按一次 Ctrl+空格（切到英文，弹一下「英」），之后**一个键都不按**：
                    # 提示必须自己到点消失。以前是"等下一个按键才收"，
                    # 切完模式不打字，那个小方块就一直挂在光标那儿
                    ctrl = 29
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 1))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 4, 0, 0, 0))
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.05)
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 0))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))
                elif base_mode == "segment":
                    # 分段上屏：KEYS（nihaoma）先挑「你好」把前一段上屏，剩下的 ma 再挑一个字
                    # —— 然后**再打一遍**，这次该一次上屏整句（词库/整句都给不出来，
                    # 只有"记性"能）
                    def send(code, state):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, code, state))

                    def tap(code, pause=0.04):
                        for state in (1, 0):
                            send(code, state)
                            time.sleep(pause)

                    for ch in KEYS:
                        tap(EVDEV[ch], 0.03)
                    # →→：整句候选占前两个，「你好」是第 3 个（候选框是横排的，
                    # 左右才是挪一个；上下是整页翻）
                    for _ in range(2):
                        tap(EVDEV_RIGHT, 0.03)
                    tap(EVDEV[" "], 0.06)  # 上屏「你好」，预编辑里剩 ma
                    time.sleep(0.1)
                    tap(EVDEV_RIGHT, 0.03)  # ma 的候选里挑第 2 个（是哪个字看词库）
                    tap(EVDEV[" "], 0.06)  # 上屏「马」
                    time.sleep(0.1)
                    for ch in KEYS:  # 再打一遍同一串键
                        tap(EVDEV[ch], 0.03)
                    time.sleep(0.1)
                    tap(EVDEV[" "], 0.06)  # 这次该直接上屏「你好马」
                elif base_mode == "script":
                    # KEYS 当成按键脚本：`n,i,h,c,a,a,del,space`
                    # 字母照写；特殊键写 down/up/del/space/enter/esc
                    named = {
                        "down": EVDEV_DOWN,
                        "up": EVDEV_UP,
                        "del": EVDEV_DELETE,
                        "space": EVDEV[" "],
                        "enter": EVDEV["\n"],
                        "esc": EVDEV["\x1b"],
                        "comma": EVDEV[","],
                        "period": EVDEV["."],
                    }

                    def tap(code, pause=0.04):
                        for state in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, code, state))
                            time.sleep(pause)

                    for name in KEYS.split(","):
                        name = name.strip()
                        if not name:
                            continue
                        if name in named:
                            tap(named[name], 0.06)
                        else:
                            for ch in name:
                                tap(EVDEV[ch], 0.03)
                elif base_mode == "forget":
                    # 和 segment 一样先分段拼一句并让它记住，
                    # 但第三遍按的是 Del + 空格：删掉之后该回到词库自己给的第一候选
                    def tap(code, pause=0.04):
                        for state in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, code, state))
                            time.sleep(pause)

                    for ch in KEYS:
                        tap(EVDEV[ch], 0.03)
                    for _ in range(2):  # →→ 挪到「你好」（前两个是整句候选）
                        tap(EVDEV_RIGHT, 0.03)
                    tap(EVDEV[" "], 0.06)
                    time.sleep(0.1)
                    tap(EVDEV_RIGHT, 0.03)  # ma 的候选里第 2 个
                    tap(EVDEV[" "], 0.06)
                    time.sleep(0.1)
                    for ch in KEYS:  # 再打一遍：这时第一条是记住的那句
                        tap(EVDEV[ch], 0.03)
                    time.sleep(0.1)
                    tap(EVDEV_DELETE, 0.06)  # Del：删掉它
                    time.sleep(0.1)
                    tap(EVDEV[" "], 0.06)  # 空格：这次该上屏「你好吗」
                elif base_mode == "symbol":
                    # 组词当中敲符号（/）：应该先把选中的候选上屏，再把符号转过来。
                    # 顺序反了的话，应用里会先冒出符号、文字跟在后面（",你好"）
                    for ch in KEYS:
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV["/"], st))
                        time.sleep(0.03)
                elif base_mode == "switch":
                    # 组词当中按 Ctrl+空格 切中英文：打了一半的拼音要先上屏，不能消失。
                    # 切完是英文模式，后面的字母全部原样转发
                    for ch in KEYS:
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                    ctrl = 29
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 1))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 4, 0, 0, 0))
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.03)
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, ctrl, 0))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))
                    time.sleep(0.1)
                    for ch in "hi":
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                elif base_mode == "page":
                    # 打 ni 之后一直按 →：挪出第一页该自动翻到第二页（←→ 是挪一个，
                    # 翻页是它的副作用；↓ 才是干脆利落地整页翻，见 pagekey）。
                    # 第 10 个候选具体是什么跟词库有关，所以只验"翻页确实生效"
                    for ch in KEYS:
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                    for _ in range(9):
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV_RIGHT, st))
                            time.sleep(0.02)
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.03)
                elif base_mode == "pagekey":
                    # ↓ 是**整页翻**（keycode 108）：打 ni 之后按一下就到第二页，
                    # 空格上屏的该是第二页的第一个，而不是第一页的「你」
                    for ch in KEYS:
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV_DOWN, st))
                        time.sleep(0.03)
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.03)
                elif base_mode == "nav":
                    # 组词之后按 →（keycode 106）换候选，再空格上屏选中的那个。
                    # 候选框是横排的，所以左右是"挪一个"、上下是"整页翻"（见 pagekey）。
                    # → 不该改动预编辑串，也不该漏给应用
                    for ch in KEYS:
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[ch], st))
                            time.sleep(0.03)
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV_RIGHT, st))
                        time.sleep(0.03)
                    for st in (1, 0):
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.03)
                else:
                    if base_mode == "watch":
                        # 自动重读配置文件的测试用：留 6 秒给外面的测试改文件、看 status
                        time.sleep(6)
                    if base_mode == "control":
                        # 在线改配置的测试用：先等 1.5 秒，让外面那套
                        # `pliers set scheme.kind double-pinyin` / `set scheme.layout flypy`
                        # 先把方案换掉 —— 之后喂进来的键就该按**新方案**解
                        time.sleep(1.5)
                    if base_mode == "caps":
                        # Caps Lock 打开：真键盘是按一下 Caps Lock 键（keycode 58），
                        # xkb 靠这个按键事件翻转 Lock 位，合成器随后再补一个 modifiers 事件。
                        # 所以输入法不能只看 modifiers 事件，得自己跟着按键喂 xkb 状态
                        for st in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, 58, st))
                        if send_mods:
                            conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 2, 0))
                        time.sleep(0.05)
                    for ch in KEYS:
                        code = EVDEV[ch]
                        for state in (1, 0):  # pressed, released
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, code, state))
                            time.sleep(0.03)
                    if base_mode in ("enter", "escape"):
                        code = EVDEV["\n"] if base_mode == "enter" else EVDEV["\x1b"]
                        for state in (1, 0):
                            conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, code, state))
                            time.sleep(0.03)
                # 按键发完了：再等一小会儿收客户端的提交，然后就可以收摊了
                #（不用一直等到 20 秒的总超时）。
                # `notice` 例外：它要量"提示挂了多久才自己消失"，喂完就得马上回事件循环 ——
                # 睡这 0.5 秒的话，期间到达的事件会攒成一批，时间戳全挤在一个瞬间
                if base_mode != "notice":
                    time.sleep(0.5)
                deadline = min(deadline, time.time() + 1.5)
            elif iface == "zwp_input_method_v2":
                log.append((opcode, args))
                print(f"mock: input method request opcode={opcode} args={args}", flush=True)
            elif iface == "zwp_virtual_keyboard_v1":
                if opcode != 0:  # keymap forwarding is not interesting
                    log.append((f"vk{opcode}", args))
                    print(f"mock: virtual keyboard opcode={opcode} args={args}", flush=True)

        for callback in list(pending_sync):
            pending_sync.remove(callback)
            synced += 1
            conn.send(callback, 0, struct.pack("<I", synced))
            conn.send(1, 1, struct.pack("<I", callback))     # wl_display.delete_id
            if synced == 2 and im_id is not None:
                conn.send(im_id, 0)                          # activate
                conn.send(im_id, 5)                          # done
        time.sleep(0.02)

    commits = [args[0] for op, args in log if op == 0]
    preedits = [args[0] for op, args in log if op == 1]
    serials = [args[0] for op, args in log if op == 3]
    forwards = [args for op, args in log if op == "vk1"]
    shows = [e for e in popup_events if e[0] == "show"]
    hides = [e for e in popup_events if e[0] == "hide"]
    nonempty = [p for p in preedits if p]
    vk_mods = [args[0] for op, args in log if op == "vk2"]

    print("\nmock: ---- result ----", flush=True)
    print(f"mock: keyboard grab   : {'yes' if grab_id is not None else 'NO'}", flush=True)
    print(f"mock: pre-edit updates: {preedits}", flush=True)
    print(f"mock: commits         : {commits}", flush=True)
    print(f"mock: commit serials  : {serials}", flush=True)
    print(f"mock: forwarded keys  : {forwards}", flush=True)
    print(f"mock: modifier masks  : {vk_mods}", flush=True)
    glyph_pixels = frames[-1][5] if frames else 0
    print(f"mock: popup surface   : {'yes' if popup_id is not None else 'NO'}", flush=True)
    print(f"mock: popup glyphs    : {glyph_pixels} 个笔画像素", flush=True)
    print(f"mock: buffer scale    : {buffer_scale}", flush=True)
    print(f"mock: popup shown     : {[(w, h) for _, w, h, _, _, _ in shows]}", flush=True)
    print(f"mock: popup hidden    : {len(hides)} times", flush=True)

    # 候选框的基本卫生：ARGB8888、stride 正确、高矮对、每次有拼音都贴一块、
    # 提交后藏起来，而且框里真的画了字（不是只有一块底色）
    popup_ok = (
        popup_id is not None
        and all(
            fmt == 0 and stride == w * 4 and h == POPUP_HEIGHT * buffer_scale
            for _, w, h, stride, fmt, _ in shows
        )
        and len(shows) == len(nonempty)
        and len(hides) == len(preedits) - len(nonempty)
        and glyph_pixels > 100
    )

    committed = "".join(commits)
    # 场景名去掉 _nomods 后缀后判断（_nomods = 合成器一个 modifiers 事件都不发）
    mode = MODE.removesuffix("_nomods")
    if mode == "inactive":
        # 没有输入框时：每个按键（按下+抬起）都应原样转发，且不应有任何提交、不该弹候选框
        ok = (
            grab_id is not None
            and not commits
            and len(forwards) == 2 * len(KEYS)
            and not shows
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: forwarded {len(forwards)}/{2 * len(KEYS)} keys, "
            f"commits {commits}, popup shown {len(shows)}",
            flush=True,
        )
    elif mode == "shortcut":
        # Ctrl+A / Ctrl+C：4 个按键事件 ×2 组全部转发，不能有预编辑或提交，
        # 修饰键掩码只在变化时发：[Ctrl 按下 4, 松开 0] ×2
        ok = (
            grab_id is not None
            and not commits
            and not preedits
            and len(forwards) == 8
            and vk_mods == [4, 0, 4, 0]
            and not shows
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: forwarded {len(forwards)}/8 keys, "
            f"commits {commits}, pre-edits {preedits}, modifier masks {vk_mods}",
            flush=True,
        )
    elif mode == "mixed":
        # 组词当中按 Shift+A：那个大写 A 并进预编辑（预编辑串看得出 "aaaA"，而且
        # 不再出候选 —— 候选框最后是收起来的），**不当场提交**，空格才把整串原样上屏。
        # 转发的只剩 Shift 自己的按下抬起
        want = KEYS + "A"
        ok = (
            grab_id is not None
            and commits == [want]
            and nonempty
            and nonempty[-1] == want
            and len(forwards) == 2
            and vk_mods == [1, 0]
            and popup_events
            and popup_events[-1][0] == "hide"
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: commits {commits} (期望 [{want!r}] 一次), "
            f"预编辑 {nonempty[-1] if nonempty else None!r}, forwarded {len(forwards)}/2 keys, "
            f"贴框 {len(shows)} 次、收框 {len(hides)} 次（最后要收起来）",
            flush=True,
        )
    elif mode == "shift":
        # Shift+A：大写不参与匹配 —— 不进组词、没有预编辑、不弹候选框，
        # Shift 和 A 的按下抬起 4 个事件全部原样转发
        ok = (
            grab_id is not None
            and not commits
            and not preedits
            and len(forwards) == 4
            and vk_mods == [1, 0]
            and not shows
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: commits {commits} (期望空), "
            f"预编辑 {preedits} (期望空), forwarded {len(forwards)}/4 keys, "
            f"贴框 {len(shows)} 次 (期望 0)",
            flush=True,
        )
    elif mode == "hidpi":
        # 高分屏（PLIERS_SCALE=2）：候选框要按 2 倍像素画（高 84），
        # 并且用 set_buffer_scale 告诉合成器"这块 buffer 是 2 倍密度"，
        # 不然 1.5x/2x 的屏幕上字是糊的
        ok = (
            grab_id is not None
            and committed == EXPECT
            and buffer_scale == 2
            and popup_ok
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r}, "
            f"缩放 {buffer_scale}（期望 2）, popup {'ok' if popup_ok else 'BAD'}",
            flush=True,
        )
    elif mode == "page":
        # 翻页：→ 挪了 9 次之后，空格上屏的应该是第二页的第一个，
        # 而不是第一页的第一个「你」；候选框也一路跟着重贴
        ok = (
            grab_id is not None
            and committed not in ("", "你")
            and len(shows) >= 2
            and not forwards
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r} (期望不是第一页的「你」), "
            f"贴框 {len(shows)} 次, forwarded {len(forwards)} keys",
            flush=True,
        )
    elif mode == "pagekey":
        # ↓ 一下就是一整页：空格上屏的该是第二页的第一个，而不是第一页的「你」
        ok = (
            grab_id is not None
            and committed not in ("", "你")
            and len(shows) >= 2
            and not forwards
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r} (期望不是第一页的「你」), "
            f"贴框 {len(shows)} 次, forwarded {len(forwards)} keys",
            flush=True,
        )
    elif mode == "segment":
        # 分段挑两次拼成一句话，第三遍再打同一串键 —— 这次是**记住的句子**一次上屏。
        # 具体挑到哪两个字不写死（词库不同、候选就不同），断言的是机制：
        # 第三次提交 == 前两次拼起来的那句，而这只有"记性"能做到
        ok = (
            grab_id is not None
            and len(commits) == 3
            and commits[0] == "你好"  # 第一段是词库里真有的词
            and len(commits[1]) == 1  # 第二段就一个字（分段能细到单字）
            and commits[2] == commits[0] + commits[1]
            and "ma" in nonempty  # 中间预编辑里真的剩了 ma
            and not forwards
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: commits {commits} "
            f"(期望 挑两段 → 第三遍一次上屏「两段拼起来的那句」), 预编辑 {nonempty}, "
            f"forwarded {len(forwards)} keys",
            flush=True,
        )
    elif mode == "script":
        ok = grab_id is not None and committed == EXPECT and not forwards
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r}, expected {EXPECT!r}, "
            f"forwarded {len(forwards)} keys（期望 0）",
            flush=True,
        )
    elif mode == "forget":
        # 前两次是分段挑的，第三遍按 Del 把"记住的那句"删掉 → 退回词库自己给的第一候选。
        # 「你好吗」在 IBus 词库和 rime 词库下都是 nihaoma 的第一候选，所以这个断言稳。
        # 另外：Del 之后弹的那句提示不能被"Del 自己的抬起"收掉（不然候选框就没了），
        # 所以最后两次候选框事件必须是 show → hide
        tail_ok = len(popup_events) >= 2 and popup_events[-1][0] == "hide" and popup_events[-2][0] == "show"
        ok = (
            grab_id is not None
            and len(commits) == 3
            and commits[0] == "你好"
            and commits[2] == "你好吗"
            and commits[2] != commits[0] + commits[1]  # 记住的那句真的被删掉了
            and not forwards
            and tail_ok
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: commits {commits} "
            f"(期望 分段挑的那句被 Del 删掉 → 退回「你好吗」), forwarded {len(forwards)} keys, "
            f"候选框最后两次 {[e[0] for e in popup_events[-2:]]}（期望 show → hide）",
            flush=True,
        )
    elif mode == "symbol":
        # 敲符号那一瞬间的顺序：提交候选必须在转发符号**之前**。
        # log 是按到达顺序记的，比下标就行
        commit_at = next((i for i, (op, _) in enumerate(log) if op == 0), None)
        slash_at = next(
            (i for i, (op, args) in enumerate(log) if op == "vk1" and args[1] == EVDEV["/"]),
            None,
        )
        ok = (
            grab_id is not None
            and commits == [EXPECT]
            and commit_at is not None
            and slash_at is not None
            and commit_at < slash_at
            and len(forwards) == 2  # "/" 的按下和抬起都原样转给应用
            and popup_ok
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {commits!r} (期望 {EXPECT!r}), "
            f"提交下标 {commit_at} / 符号下标 {slash_at}（要前者小）, forwarded {len(forwards)} keys",
            flush=True,
        )
    elif mode == "switch":
        # 切中英文时半截拼音上屏：commits 恰好一次原始字母（不转换），
        # 切换键自己被吃掉，切完是英文模式 → hi 全部原样转发
        want = 2 + 2 * 2  # Ctrl 按下抬起 + h/i 各按下抬起
        ok = (
            grab_id is not None
            and commits == [EXPECT]
            and preedits[-1] == ""
            and len(forwards) == want
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {commits!r} (期望 [{EXPECT!r}] 原始字母), "
            f"forwarded {len(forwards)}/{want} keys, 最后一个预编辑 {preedits[-1] if preedits else None!r}",
            flush=True,
        )
    elif mode == "english":
        # 切到英文之后：Ctrl 的按下抬起照旧转发、空格的按下抬起被吃掉（它是热键），
        # 英文按键全部原样转发；没有提交、没有非空预编辑，而且弹过一次「英」提示
        want = 2 + 2 * len(KEYS)
        ok = (
            grab_id is not None
            and not commits
            and not nonempty
            # 转发出去的空格只有英文里那一个（按下+抬起）：切换用的那个被吃掉
            and sum(1 for _, key, _ in forwards if key == EVDEV[" "]) == 2
            and len(forwards) == want
            and len(shows) >= 1
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: forwarded {len(forwards)}/{want} keys, "
            f"commits {commits}, 非空预编辑 {nonempty}, 提示弹了 {len(shows)} 次",
            flush=True,
        )
    elif mode == "notice":
        # 提示要自己消失：切换之后**一个键都不再按**，所以最后一条候选框事件必须是 hide，
        # 而且是 show 之后挂了一小会儿才收 —— 既不是被切换键自己的抬起立刻收掉，
        # 也不是一直挂在那儿等下一个按键
        shown = [e for e in popup_events if e[0] == "show"]
        last = popup_events[-1] if popup_events else None
        held = (last[1] - shown[-1][5]) if (shown and last and last[0] == "hide") else None
        ok = (
            grab_id is not None
            and not commits
            and not nonempty
            and held is not None
            and 0.15 < held < 1.5
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: "
            + (
                f"提示挂了 {held:.2f}s 后自己收掉"
                if held is not None
                else f"提示没收掉（候选框事件 {[e[0] for e in popup_events]}）"
            )
            + f", commits {commits}, 非空预编辑 {nonempty}",
            flush=True,
        )
    elif mode == "pick":
        # 数字选词："nihao2" 里的 2 直接上屏第二个候选，按键（连抬起）都吃掉，
        # 所以既没有转发、也没有把 2 留进拼音
        ok = (
            grab_id is not None
            and committed == EXPECT
            and preedits[-1] == ""
            and not forwards
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r} (期望 {EXPECT!r}), "
            f"forwarded {len(forwards)} keys (期望 0)",
            flush=True,
        )
    elif mode == "nav":
        # 方向键换候选：预编辑串全程不变（↓ 之后还是 "nihao"），
        # ↓ 和空格的按下抬起都被吃掉，一个键都不该转发
        ok = (
            grab_id is not None
            and committed == EXPECT
            and nonempty[-1] == KEYS
            and not forwards
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r} (期望 {EXPECT!r}), "
            f"最后一个预编辑 {nonempty[-1] if nonempty else None!r}, forwarded {len(forwards)} keys",
            flush=True,
        )
    elif mode == "englishword":
        # 英文补全：只敲了 `kuber` + 空格，上屏的必须是补全后的 `kubernetes`
        #（词表编译在二进制里，这个词来自 tools/english-extra.txt 那份开发词）。
        # 预编辑里全程只有敲进去的那几个字母 —— 补全不改变"用户敲了什么"
        typed = KEYS.rstrip()
        ok = (
            grab_id is not None
            and committed == EXPECT
            and nonempty
            and nonempty[-1] == typed
            and not forwards
            and shows
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r}（期望补全成 {EXPECT!r}）, "
            f"预编辑 {nonempty[-1] if nonempty else None!r}, forwarded {len(forwards)} keys, "
            f"贴框 {len(shows)} 次",
            flush=True,
        )
    elif mode == "enter":
        # 组词中按回车：把原始拼音提交掉，回车本身不给应用
        ok = grab_id is not None and committed == "nihao" and not forwards
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r} (期望原始拼音), "
            f"forwarded {len(forwards)} keys (期望 0)",
            flush=True,
        )
    elif mode == "escape":
        # 组词中按 Esc：取消组词，既不提交也不转发
        ok = (
            grab_id is not None
            and not commits
            and not forwards
            and preedits
            and preedits[-1] == ""
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: commits {commits}, forwarded {len(forwards)} keys, "
            f"最后一个预编辑 {preedits[-1] if preedits else None!r}",
            flush=True,
        )
    elif mode == "caps":
        # Caps Lock 打开时：字母解出来全是大写 → 大写不参与匹配，
        # 不进组词、不出候选，每个按键（连 Caps Lock 自己）原样转发
        want = 2 + 2 * len(KEYS)
        ok = (
            grab_id is not None
            and not commits
            and not preedits
            and len(forwards) == want
            and not shows
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: commits {commits} (期望空), "
            f"预编辑 {preedits} (期望空), forwarded {len(forwards)}/{want} keys, "
            f"贴框 {len(shows)} 次 (期望 0)",
            flush=True,
        )
    elif mode in ("control", "watch"):
        # 这两种场景中途会改配置：重建引擎会多发几次"清空预编辑 + 收框"，
        # 所以只验"键最终还是照新配置解的"，候选框的计数就不比了
        ok = grab_id is not None and committed == EXPECT
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r}, expected {EXPECT!r}",
            flush=True,
        )
    else:
        ok = grab_id is not None and committed == EXPECT and popup_ok
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r}, expected {EXPECT!r}, "
            f"popup {'ok' if popup_ok else 'BAD'}",
            flush=True,
        )
    png = os.environ.get("MOCK_PNG")
    if png and frames:
        w, h, data, offset, stride, _ = frames[-1]
        write_png(png, data, offset, w, h, stride)

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
