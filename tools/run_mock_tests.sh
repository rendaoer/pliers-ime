#!/usr/bin/env bash
# 用 mock 合成器把整条链路跑一遍：抓键盘 → 引擎 → 提交文本 / 贴候选框。
# 不需要真合成器，也不需要键盘 —— mock 假装自己有个输入框，把按键喂进来，
# 然后检查客户端回给它的每一条请求。
#
#     ./tools/run_mock_tests.sh                        # 全部场景
#     ./tools/run_mock_tests.sh --png target/popup.png # 顺便存一张候选框的图
#     ./tools/run_mock_tests.sh --dict ~/.local/share/pliers/dict.db
set -u
cd "$(dirname "$0")/.."

# 参数写法跟 shell 无关（${VAR=x cmd} 这种前缀在 zsh/bash/nushell 里写法不一样）：
#   --dict <路径>   词库。不给就依次找 target/dict.db、~/.local/share/pliers/dict.db，
#                   也认 PLIERS_DICT 环境变量。
#                   注意测试里的选词会记进 user_word，不想污染自己的词频就指向一份副本：
#                       cp ~/.local/share/pliers/dict.db target/dict-copy.db
#   --png  <路径>   把 active 场景的候选框存成 PNG
while [ $# -gt 0 ]; do
    case "$1" in
        --dict) DICT_ARG="$2"; shift 2 ;;
        --png)  PNG_ARG="$2";  shift 2 ;;
        -h|--help) sed -n '2,8p' "$0"; exit 0 ;;
        *) echo "不认识的参数 $1"; exit 2 ;;
    esac
done

cargo build -q || exit 1

# 测试自己的配置：不吃 ~/.config/pliers/config.toml 里用户改过的东西
# （不然你把方案改成双拼，这套断言就全变了）
CFG="$(mktemp)"
cat >"$CFG" <<'TOML'
[scheme]
kind = "full-pinyin"

[engine]
toggle_keys = ["ctrl+space"]
start_mode = "chinese"
indicator = true
TOML

SOCK="/tmp/pliers-mock-$$.sock"
# 词库：默认用仓库里导入好的那份（装到 ~/.local/share/pliers/ 之后就不用设了）
DICT="${DICT_ARG:-${PLIERS_DICT:-}}"
if [ -z "$DICT" ]; then
    for candidate in "$PWD/target/dict.db" "$HOME/.local/share/pliers/dict.db"; do
        if [ -f "$candidate" ]; then DICT="$candidate"; break; fi
    done
fi
if [ -z "$DICT" ] || [ ! -f "$DICT" ]; then
    echo "找不到词库（试过 target/dict.db 和 ~/.local/share/pliers/dict.db）"
    echo "先装一份：pliers --init（或 pliers dict build 自己构建）"
    exit 1
fi
# 测试用自己的一份词库副本：选词/记整句都会写进库里，别污染真库
SRC_DICT="$DICT"   # 原始词库，一直保持"干净"：需要候选顺序确定的场景从它拷
TEST_DICT="$(mktemp -d)/dict.db"
cp "$SRC_DICT" "$TEST_DICT"
# 还没并回主库的 WAL 也得一起拷：刚导入完的库、或者输入法正开着的库，
# 最新的写入（很可能包括音节表）还在 -wal 里，只拷 .db 会拿到一个不完整的库
[ -f "$SRC_DICT-wal" ] && cp "$SRC_DICT-wal" "$TEST_DICT-wal"
DICT="$TEST_DICT"
echo "词库：$DICT（测试用的副本）"
LOGS="$(mktemp -d)"
pass=0
fail=0

# 在线改配置：mock 用 control 场景（先等 1.5 秒再打字），趁这段时间说话
run_control() {
    local log="$LOGS/control.log"
    local sock="/tmp/pliers-ctl-$$.sock"
    local ctl="/tmp/pliers-cmd-$$.sock"
    local bad=0

    python3 tools/mock_compositor.py "$sock" "nihc " 你好 control >"$log" 2>&1 &
    local mock=$!
    sleep 0.4
    env PLIERS_DICT="$DICT" PLIERS_CONFIG="$CFG" PLIERS_SOCKET="$ctl" \
        WAYLAND_DISPLAY="$sock" timeout 30 ./target/debug/pliers >>"$log" 2>&1 &
    local ime=$!
    sleep 0.4

    say() { # say <输出里该出现的内容> <命令...>
        local want="$1"; shift
        local out
        out=$(PLIERS_SOCKET="$ctl" ./target/debug/pliers "$@" 2>&1)
        case "$out" in
            *"$want"*) echo "        \$ pliers $* → 有「$want」✓" ;;
            *) echo "        \$ pliers $* → 没有「$want」：$out"; bad=1 ;;
        esac
    }

    # 输出的排版在命令行这边：一项一行
    say "全拼（整句候选开）" status
    say "重新读了配置文件" reload
    say "改好了：scheme.kind = double-pinyin" set scheme.kind double-pinyin
    say "改好了：scheme.layout = flypy" set scheme.layout flypy
    say "scheme.layout 只认" set scheme.layout flpy   # 键位名写错：报错，而且不能改坏正在跑的
    say "双拼（小鹤）" status

    # 交互模式（真终端才有上下选；这里用 script 开个 pty）
    # ↓↓↓↓↓ 到第 6 项「一页候选数」→ 回车 → ↓ 选到第二个值 → 回车 → q
    # （值列表的光标先停在"现在这个值"上，所以从 9 往上/下挪一格就够）
    if command -v script >/dev/null 2>&1; then
        out=$(printf '\033[B\033[B\033[B\033[B\033[B\r\033[B\rq' |
            timeout 15 script -qec "PLIERS_SOCKET=$ctl ./target/debug/pliers set" /dev/null 2>&1)
        case "$out" in
            *"❯"*) echo "        \$ pliers set（上下选）→ 列表带选中箭头 ✓" ;;
            *) echo "        \$ pliers set（上下选）→ 没看到列表"; bad=1 ;;
        esac
        say "一页 1 个" status   # 9 往下挪一格 = 回到 1（值列表是绕圈的）
    fi

    # 管道（没终端）时退回行式：输编号、再输值
    out=$(printf '3\nfalse\nq\n' | PLIERS_SOCKET="$ctl" ./target/debug/pliers set 2>&1)
    case "$out" in
        *"改好了：scheme.sentence = false"*) echo "        \$ pliers set（管道/行式）→ 改好了 scheme.sentence ✓" ;;
        *) echo "        \$ pliers set（管道/行式）→ 没改成：$out"; bad=1 ;;
    esac

    wait "$ime" 2>/dev/null
    wait "$mock" 2>/dev/null
    rm -f "$ctl"

    if [ "$bad" -eq 0 ] && grep -q "PASS" "$log"; then
        pass=$((pass + 1))
        printf '  \033[32mPASS\033[0m %-16s %s\n' control "$(grep -o 'mock: PASS.*' "$log" | head -1 | cut -c13-)"
    else
        fail=$((fail + 1))
        printf '  \033[31mFAIL\033[0m %-16s 见日志\n' control
        cp "$log" "target/mock-control.log"
        echo "       日志：target/mock-control.log"
    fi
}

# 自动重读：跑着的时候直接改配置文件（没人通知它），一秒内该生效；改坏了要继续用旧的
run_watch() {
    local log="$LOGS/watch.log"
    local sock="/tmp/pliers-watchm-$$.sock"
    local ctl="/tmp/pliers-watchc-$$.sock"
    local cfg="$LOGS/watch.toml"
    local bad=0

    cat >"$cfg" <<TOML
[scheme]
kind = "double-pinyin"   # 小鹤，跟下面喂的 nihc 对得上
layout = "flypy"

[dict]
max_candidates = 9
TOML

    python3 tools/mock_compositor.py "$sock" "nihc " 你好 watch >"$log" 2>&1 &
    local mock=$!
    sleep 0.4
    env PLIERS_DICT="$DICT" PLIERS_CONFIG="$cfg" PLIERS_SOCKET="$ctl" \
        WAYLAND_DISPLAY="$sock" timeout 30 ./target/debug/pliers >>"$log" 2>&1 &
    local ime=$!
    sleep 0.5

    check() { # check <说明> <输出里该有的>
        local out
        out=$(PLIERS_SOCKET="$ctl" ./target/debug/pliers status 2>&1)
        case "$out" in
            *"$2"*) echo "        $1 → 有「$2」✓" ;;
            *) echo "        $1 → 没看到「$2」：$out"; bad=1 ;;
        esac
    }

    sed -i 's/max_candidates = 9/max_candidates = 3/' "$cfg"   # 只改文件，不通知实例
    sleep 1.2
    check "改了文件（没人通知它）" "一页 3 个"

    printf '[scheme]\nkind = "双拼"\n' >>"$cfg"                # 写坏了
    sleep 1.2
    check "文件写坏了，继续用旧的" "一页 3 个"

    wait "$ime" 2>/dev/null
    wait "$mock" 2>/dev/null
    rm -f "$ctl"

    if [ "$bad" -eq 0 ] && grep -q "PASS" "$log"; then
        pass=$((pass + 1))
        printf '  \033[32mPASS\033[0m %-16s %s\n' watch "$(grep -o 'mock: PASS.*' "$log" | head -1 | cut -c13-)"
    else
        fail=$((fail + 1))
        printf '  \033[31mFAIL\033[0m %-16s 见日志\n' watch
        cp "$log" "target/mock-watch.log"
        echo "       日志：target/mock-watch.log"
    fi
}

run() { # run <名字> <按键> <期望提交> <模式> [额外环境变量]
    local name="$1" keys="$2" expect="$3" mode="$4" extra="${5:-}"
    local log="$LOGS/$name.log"
    local png=""
    [ "$name" = "active" ] && png="${PNG_ARG:-}"

    # 这几个场景的断言跟"第几个候选"有关，得从**没被前面场景写过**的词库拷一份：
    #   segment/forget —— 前面场景会把拼过的句子记进去（记性会顶到第一位）
    #   pick/nav       —— 前一个场景选过的词会加 100 万权重，把候选顺序顶乱
    #                     （pick 选「事件」之后，nav 的「事件」就跑到第一位了）
    local dict="$DICT"
    case "$name" in
        segment|forget|pick|nav)
            dict="$LOGS/$name.db"
            cp "$SRC_DICT" "$dict"
            [ -f "$SRC_DICT-wal" ] && cp "$SRC_DICT-wal" "$dict-wal"
            ;;
    esac

    MOCK_PNG="$png" python3 tools/mock_compositor.py "$SOCK" "$keys" "$expect" "$mode" >"$log" 2>&1 &
    local mock=$!
    sleep 0.4
    env PLIERS_DICT="$dict" PLIERS_CONFIG="$CFG" $extra WAYLAND_DISPLAY="$SOCK" timeout 30 ./target/debug/pliers >>"$log" 2>&1
    wait "$mock"

    if grep -q "PASS" "$log"; then
        pass=$((pass + 1))
        printf '  \033[32mPASS\033[0m %-16s %s\n' "$name" "$(grep -o 'mock: PASS.*' "$log" | head -1 | cut -c13-)"
    else
        fail=$((fail + 1))
        printf '  \033[31mFAIL\033[0m %-16s %s\n' "$name" "$(grep -o 'mock: FAIL.*' "$log" | head -1 | cut -c13-)"
        cp "$log" "target/mock-$name.log"
        echo "       日志：target/mock-$name.log"
    fi
}

echo "== 正常组词 =="
run active         "nihao "  你好  active
run active_nomods  "nihao "  你好  active_nomods
run pick           "shijian2" 事件 pick          # 数字选词（第 2 个候选；事件在两种词库下都是第 2 个）
run nav            "shijian"  事件 nav           # ↓ 换候选再空格
run page           "ni"      ""    page          # → 挪 9 次自动翻到第二页
run pagekey        "ni"      ""    pagekey       # ↓ 一下整页翻到第二页
run caps           "nihao "  ""    caps          # Caps Lock 打开 = 打大写英文，不进组词
run enter          "nihao"   nihao enter         # 回车提交原文
run escape         "nihao"   ""    escape        # Esc 取消
run mixed          "aaa"     aaaA  mixed         # 组词中 Shift+A：并进预编辑，空格整串上屏
run shift          "a"       ""    shift         # 纯 Shift+A：4 个事件全转发

echo "== 英文候选（英文单词补全）=="
# 只敲了 kuber，空格上屏的是补全后的 kubernetes —— 词表编译在二进制里，跟词库无关
run englishword    "kuber "  kubernetes englishword

echo "== 中英文切换 =="
run english        "hello "   ""    english       # Ctrl+空格 切英文后打英文
run switch         "nihao"   nihao switch        # 组词中切英文：半截拼音先上屏
run notice         ""        ""    notice        # 切完一个键都不按：「英」提示自己到点消失

echo "== 组词中敲符号 =="
run symbol         "nihao"   你好  symbol        # 符号必须排在文字后面

echo "== 中文标点 =="
# `nihao,` → 一步上屏「你好，」（全角），`comma` 这个键不转发给应用
run punct          "n,i,h,a,o,comma" "你好，" script

echo "== 分段上屏 + 记忆 =="
run segment        "nihaoma" ""    segment       # 挑两段拼出「你好马」，再打一遍直接出
run forget         "nihaoma" ""    forget        # 再打一遍时按 Del 删掉它，退回「你好吗」
# Del 在组词当中一律吃掉：`nihaoma` 之后连按 Del，一个键都不该漏给应用
# （漏出去在终端里就是 `^[[3~` 那串东西）
run del_swallow    "n,i,h,a,o,m,a,del,del,del,space" "你好吗" script

echo "== 高分屏 =="
run hidpi          "nihao "  你好  hidpi  "PLIERS_SCALE=2"

echo "== 在线改配置（pliers set / status / reload）=="
# 客户端跑着的时候把方案换成小鹤双拼，之后 mock 才喂 `nihc`：
# 提交的必须还是「你好」—— 说明真的换了引擎，不只是回了一句话
run_control
# 自动重读：改配置文件不用通知它，保存后一秒内生效
run_watch

echo "== 快捷键 / 焦点 =="
run shortcut       "a"       ""    shortcut
run shortcut_nomods "a"      ""    shortcut_nomods
run inactive       "nihao "  ""    inactive
run inactive_nomods "nihao " ""    inactive_nomods
run shift_nomods   "a"       ""    shift_nomods

echo
echo "通过 $pass，失败 $fail（日志在 $LOGS）"
[ "$fail" -eq 0 ]
