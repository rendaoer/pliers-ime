#!/usr/bin/env python3
"""A tiny mock Wayland compositor for exercising ime-aa's live mode.

It implements just enough of the wire protocol -- wl_display / wl_registry /
wl_seat / wl_compositor / wl_shm / zwp_input_method_v2 / zwp_virtual_keyboard_v1
-- to act as a compositor that has a focused text field:

  1. it advertises the globals imekit binds,
  2. it activates the input method,
  3. it waits for the keyboard grab request,
  4. it sends an XKB keymap and then feeds a key sequence,
  5. it prints every pre-edit / commit / forwarded key the client sends back,
  6. it accepts the client's candidate-window surface (input_popup role) and
     reports when the client shows / hides it.

This is how the live path can be tested without a compositor that supports
input methods and without a physical keyboard:

    python3 tools/mock_compositor.py /tmp/mock-wl "nihao " 你好 &
    WAYLAND_DISPLAY=/tmp/mock-wl cargo run

Note on the wire format: file descriptors travel as ancillary data and take
*no* slot in the message body, so the keymap message is just
[format][size] plus one fd.

Usage: mock_compositor.py <socket-path> [keys] [expected-committed-text]
"""
import array
import os
import select
import socket
import struct
import subprocess
import sys
import tempfile
import time

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
}

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

    client, _ = server.accept()
    conn = Conn(client)

    objects = {}
    pending_sync = []
    synced = 0
    im_id = vk_id = grab_id = None
    log = []
    # 候选框：pool 的 fd 要一直留着，buffer 记住尺寸，attach 序列用来判定显示/隐藏
    shm_fds = []
    buffers = {}
    popup_id = None
    popup_events = []
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
                print(f"mock: shm pool id={args[0]} size={args[2]}", flush=True)
            elif iface == "wl_shm_pool" and opcode == 0:             # create_buffer
                objects[args[0]] = "wl_buffer"
                buffers[args[0]] = args[1:]                          # offset, w, h, stride, format
            elif iface == "zwp_input_method_v2" and opcode == 4:     # get_input_popup_surface
                popup_id = args[0]
                objects[popup_id] = "zwp_input_popup_surface_v2"
                print("mock: got popup surface request", flush=True)
            elif iface == "wl_surface" and opcode == 1:              # attach
                buffer_id = args[0] if args[0] != 0 else None
                if buffer_id is None:
                    popup_events.append(("hide",))
                    print("mock: popup hidden (attach NULL)", flush=True)
                else:
                    _, w, h, stride, fmt = buffers.get(buffer_id, (0, 0, 0, 0, 0))
                    popup_events.append(("show", w, h, stride, fmt))
                    print(f"mock: popup shown {w}x{h} stride={stride} format={fmt}", flush=True)
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
                if MODE == "inactive":
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
                    # 用户的实际场景：先敲几个小写字母组词（预编辑挂着），按住 Shift 敲 A，
                    # 最后按空格提交。期望整个 "aaaA" 一直待在预编辑里，
                    # 只在最后提交一次 —— 打字中途不往应用塞任何字符
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
                    for st in (1, 0):  # 空格：这时候才提交
                        conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV[" "], st))
                        time.sleep(0.03)
                elif base_mode == "shift":
                    # Shift+A：XKB 里 Shift = 1，字母 keysym 变成大写 'A'
                    shift = 42
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, shift, 1))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 1, 0, 0, 0))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV["a"], 1))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, EVDEV["a"], 0))
                    conn.send(grab_id, 1, struct.pack("<IIII", 0, 0, shift, 0))
                    if send_mods:
                        conn.send(grab_id, 2, struct.pack("<IIIII", 0, 0, 0, 0, 0))
                else:
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
                time.sleep(0.5)
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
    print(f"mock: popup surface   : {'yes' if popup_id is not None else 'NO'}", flush=True)
    print(f"mock: popup shown     : {[(w, h) for _, w, h, _, _ in shows]}", flush=True)
    print(f"mock: popup hidden    : {len(hides)} times", flush=True)

    # 候选框的基本卫生：ARGB8888、stride 正确、每次有拼音都贴一块、提交后藏起来
    popup_ok = (
        popup_id is not None
        and all(fmt == 0 and stride == w * 4 and h == 56 for _, w, h, stride, fmt in shows)
        and len(shows) == len(nonempty)
        and len(hides) == len(preedits) - len(nonempty)
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
        # 组词当中按 Shift+A，最后空格提交：整个 "aaaA" 一直待在预编辑里，
        # 只在最后提交一次（所以 commits 恰好是 ['aaaA']，不是挨个字符往外蹦）
        want = KEYS + "A"
        ok = (
            grab_id is not None
            and commits == [want]
            and preedits
            and preedits[-1] == ""  # 提交后预编辑要清干净
            and vk_mods == [1, 0]
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: commits {commits} (期望 [{want!r}] 一次), "
            f"最后一个预编辑 {preedits[-1] if preedits else None!r}",
            flush=True,
        )
    elif mode == "shift":
        # Shift+A：大写字母也留在预编辑里（"A"），打字中途不提交、不转发字符键。
        # 转发只该剩 Shift 自己的按下抬起 2 个，修饰键掩码 [Shift 按下 1, 松开 0]
        ok = (
            grab_id is not None
            and not commits
            and preedits
            and preedits[-1] == "A"
            and len(forwards) == 2
            and vk_mods == [1, 0]
            and len(shows) == 1  # buffer 里有一个字符，候选框该弹一次
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: 预编辑 {preedits[-1] if preedits else None!r} (期望 'A'), "
            f"commits {commits} (期望空), forwarded {len(forwards)}/2 keys",
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
        # Caps Lock 打开时：字母解出来是大写，照样攒进预编辑（"NIHAO"），
        # 空格时查表大小写不敏感 → 提交 你好。转发只该剩 Caps Lock 自己 2 个
        ok = (
            grab_id is not None
            and committed == EXPECT
            and nonempty == ["N", "NI", "NIH", "NIHA", "NIHAO"]
            and len(forwards) == 2  # CapsLock 按下抬起
        )
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r} (期望 {EXPECT!r}), "
            f"预编辑 {nonempty[-1] if nonempty else None!r}, forwarded {len(forwards)}/2 keys",
            flush=True,
        )
    else:
        ok = grab_id is not None and committed == EXPECT and popup_ok
        print(
            f"mock: {'PASS' if ok else 'FAIL'}: committed {committed!r}, expected {EXPECT!r}, "
            f"popup {'ok' if popup_ok else 'BAD'}",
            flush=True,
        )
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
