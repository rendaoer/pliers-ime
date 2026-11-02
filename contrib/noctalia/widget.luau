-- pliers 输入法状态：中 / 英
--
-- 每秒通过 Unix socket 问一次状态（`status` 里的 `mode=`），点一下发 `toggle`。
-- 这样不依赖 `pliers` 在 Noctalia 的 PATH 上，也不用管 libxkbcommon 之类的
-- 动态库 —— 插件只需要系统里的 `nc`（NixOS 默认有）。
--
-- socket 位置跟 pliers 自己找的一样：PLIERS_SOCKET > $XDG_RUNTIME_DIR/pliers.sock。

local runtime = noctalia.getenv("XDG_RUNTIME_DIR") or "/run/user/1000"
local socket = noctalia.getenv("PLIERS_SOCKET")
if socket == nil or socket == "" then
  socket = runtime .. "/pliers.sock"
end

local function quote(s)
  return "'" .. s:gsub("'", "'\\''") .. "'"
end

local function mode_command()
  return "printf 'status\\n' | nc -U " .. quote(socket)
    .. " | tr '\\t' '\\n' | sed -n 's/^mode=//p'"
end

local function toggle_command()
  return "printf 'toggle\\n' | nc -U " .. quote(socket) .. " >/dev/null 2>&1"
end

local pending = false
local last = nil

noctalia.setUpdateInterval(1000)

local function paint(text, tip)
  if last ~= text then
    barWidget.setText(text)
    last = text
  end
  barWidget.setTooltip(tip)
end

local function refresh()
  if pending then
    return
  end
  pending = true
  noctalia.runAsync(mode_command(), function(result)
    pending = false
    if result == nil then
      paint("拼", "pliers：没有回话")
      return
    end
    local mode = (result.stdout or ""):gsub("%s+$", "")
    if result.exitCode == 0 and mode == "中" then
      paint("中", "pliers：中文（左键切英文）")
    elseif result.exitCode == 0 and mode == "英" then
      paint("英", "pliers：英文（左键切中文）")
    elseif result.exitCode == 0 and mode ~= "" then
      paint("拼", "pliers：状态未知（" .. mode .. "）")
    else
      local err = (result.stderr or ""):gsub("%s+$", "")
      paint("拼", "pliers 没在运行" .. (err ~= "" and ("：" .. err) or ""))
    end
  end)
end

function update()
  refresh()
end

function onClick()
  noctalia.runAsync(toggle_command(), function(_result)
    refresh()
  end)
end

refresh()
