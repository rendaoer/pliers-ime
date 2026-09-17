#!/usr/bin/env bash
# 用 mock 合成器把整条链路跑一遍：抓键盘 → 引擎 → 提交文本 / 贴候选框。
# 不需要真合成器，也不需要键盘 —— mock 假装自己有个输入框，把按键喂进来，
# 然后检查客户端回给它的每一条请求。
#
#     ./tools/run_mock_tests.sh            # 全部场景
#     MOCK_PNG=target/popup.png ./tools/run_mock_tests.sh   # 顺便存一张候选框的图
set -u
cd "$(dirname "$0")/.."

cargo build -q || exit 1

SOCK="/tmp/ime-aa-mock-$$.sock"
# 词库：默认用仓库里导入好的那份（装到 ~/.local/share/ime-aa/ 之后就不用设了）
DICT="${IME_AA_DICT:-$PWD/target/dict.db}"
if [ ! -f "$DICT" ]; then
    echo "找不到词库 $DICT，先跑：cargo run -p ime-dict -- --freq target/jieba-dict.txt --out target/dict.db"
    exit 1
fi
LOGS="$(mktemp -d)"
pass=0
fail=0

run() { # run <名字> <按键> <期望提交> <模式> [额外环境变量]
    local name="$1" keys="$2" expect="$3" mode="$4" extra="${5:-}"
    local log="$LOGS/$name.log"
    local png=""
    [ "$name" = "active" ] && png="${MOCK_PNG:-}"

    MOCK_PNG="$png" python3 tools/mock_compositor.py "$SOCK" "$keys" "$expect" "$mode" >"$log" 2>&1 &
    local mock=$!
    sleep 0.4
    env IME_AA_DICT="$DICT" $extra WAYLAND_DISPLAY="$SOCK" timeout 30 ./target/debug/ime-aa >>"$log" 2>&1
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
run pick           "nihao2"  倪浩  pick          # 数字选词（第 2 个候选）
run nav            "nihao"   倪浩  nav           # ↓ 换候选再空格
run caps           "nihao "  你好  caps          # Caps Lock 打的大写
run enter          "nihao"   nihao enter         # 回车提交原文
run escape         "nihao"   ""    escape        # Esc 取消
run mixed          "aaa"     aaaA  mixed         # 组词当中 Shift+A
run shift          "a"       ""    shift         # 纯 Shift+A

echo "== 高分屏 =="
run hidpi          "nihao "  你好  hidpi  "IME_AA_SCALE=2"

echo "== 快捷键 / 焦点 =="
run shortcut       "a"       ""    shortcut
run shortcut_nomods "a"      ""    shortcut_nomods
run inactive       "nihao "  ""    inactive
run inactive_nomods "nihao " ""    inactive_nomods
run shift_nomods   "a"       ""    shift_nomods

echo
echo "通过 $pass，失败 $fail（日志在 $LOGS）"
[ "$fail" -eq 0 ]
