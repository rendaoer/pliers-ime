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
    echo "先导入：cargo run -p pliers-dict --release -- --freq jieba-dict.txt --out ~/.local/share/pliers/dict.db"
    exit 1
fi
echo "词库：$DICT"
LOGS="$(mktemp -d)"
pass=0
fail=0

run() { # run <名字> <按键> <期望提交> <模式> [额外环境变量]
    local name="$1" keys="$2" expect="$3" mode="$4" extra="${5:-}"
    local log="$LOGS/$name.log"
    local png=""
    [ "$name" = "active" ] && png="${PNG_ARG:-}"

    MOCK_PNG="$png" python3 tools/mock_compositor.py "$SOCK" "$keys" "$expect" "$mode" >"$log" 2>&1 &
    local mock=$!
    sleep 0.4
    env PLIERS_DICT="$DICT" PLIERS_CONFIG="$CFG" $extra WAYLAND_DISPLAY="$SOCK" timeout 30 ./target/debug/pliers >>"$log" 2>&1
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
run page           "ni"      ""    page          # → 挪 9 次自动翻到第二页
run caps           "nihao "  ""    caps          # Caps Lock 打开 = 打大写英文，不进组词
run enter          "nihao"   nihao enter         # 回车提交原文
run escape         "nihao"   ""    escape        # Esc 取消
run mixed          "aaa"     aaaA  mixed         # 组词中 Shift+A：并进预编辑，空格整串上屏
run shift          "a"       ""    shift         # 纯 Shift+A：4 个事件全转发

echo "== 中英文切换 =="
run english        "hello "   ""    english       # Ctrl+空格 切英文后打英文
run switch         "nihao"   nihao switch        # 组词中切英文：半截拼音先上屏

echo "== 组词中敲符号 =="
run symbol         "nihao"   你好  symbol        # 符号必须排在文字后面

echo "== 高分屏 =="
run hidpi          "nihao "  你好  hidpi  "PLIERS_SCALE=2"

echo "== 快捷键 / 焦点 =="
run shortcut       "a"       ""    shortcut
run shortcut_nomods "a"      ""    shortcut_nomods
run inactive       "nihao "  ""    inactive
run inactive_nomods "nihao " ""    inactive_nomods
run shift_nomods   "a"       ""    shift_nomods

echo
echo "通过 $pass，失败 $fail（日志在 $LOGS）"
[ "$fail" -eq 0 ]
