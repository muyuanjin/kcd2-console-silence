-- ConsoleSilence Lua shim.
--
-- 这一层是**唯一**知道"选项表长什么样"的地方：跨界的原语只接受位置参数和字符串/数字
-- （那些在 IFunctionHandler 上的读取是无歧义的），表解析、默认值、错误语义留在这里。
--
-- 装好之后调用方看到的表面就是 docs/CONCEPT.md §6.2 那六个函数；
-- 六个 `*_raw` 会被从表上摘掉，普通玩家看不到它们。

local mute_raw, unmute_raw, clear_raw =
    ConsoleSilence.mute_raw, ConsoleSilence.unmute_raw, ConsoleSilence.clear_raw
local list_raw, stats_raw, which_raw =
    ConsoleSilence.list_raw, ConsoleSilence.stats_raw, ConsoleSilence.which_raw
local enabled_raw, observe_raw, fold_raw = ConsoleSilence.enabled_raw, ConsoleSilence.observe_raw, ConsoleSilence.fold_raw

ConsoleSilence.mute_raw, ConsoleSilence.unmute_raw, ConsoleSilence.clear_raw = nil, nil, nil
ConsoleSilence.list_raw, ConsoleSilence.stats_raw, ConsoleSilence.which_raw = nil, nil, nil
ConsoleSilence.enabled_raw, ConsoleSilence.observe_raw, ConsoleSilence.fold_raw = nil, nil, nil

local function fail(where, message)
    error("ConsoleSilence." .. where .. ": " .. tostring(message), 3)
end

-- 静音一行/一族。裸串永远按字面量（contains）处理；要别的语义必须写进 opts。
function ConsoleSilence.mute(pattern, opts)
    if type(pattern) ~= "string" or pattern == "" then
        fail("mute", "pattern must be a non-empty string")
    end
    local kind, action, owner, ttl = "contains", "drop", "", 0
    if opts ~= nil then
        if type(opts) ~= "table" then
            fail("mute", "opts must be a table when given")
        end
        kind = opts.kind or kind
        action = opts.action or action
        owner = opts.owner or owner
        ttl = opts.ttl_ms or opts.ttl or ttl
    end
    local result = mute_raw(tostring(kind), pattern, tostring(action), tostring(owner), tonumber(ttl) or 0)
    if type(result) == "string" then
        fail("mute", result)
    end
    return result
end

-- 幂等：不存在的 id 也算成功（删两次不是错误）。
function ConsoleSilence.unmute(id)
    if type(id) ~= "number" then
        fail("unmute", "id must be the number returned by mute()")
    end
    return unmute_raw(id)
end

-- 只清运行期规则；给了 owner 就只清那一个 owner 的。
function ConsoleSilence.clear(owner)
    if owner ~= nil and type(owner) ~= "string" then
        fail("clear", "owner must be a string or nil")
    end
    return clear_raw(owner or "")
end

function ConsoleSilence.list()
    return list_raw()
end

function ConsoleSilence.stats()
    return stats_raw()
end

-- 总开关：false = 一行都不吞（钩子还在，用来做 A/B 对比）。
function ConsoleSilence.enabled(on)
    return enabled_raw(on ~= false)
end

-- 观察模式：把每一行写进 consolesilence.log（抓"到底谁在刷"用，别忘关）。
function ConsoleSilence.observe(on)
    return observe_raw(on == true)
end

-- 重复行折叠：同一段文字持续刷屏时只留头几行 + 一条汇总。默认关闭。
function ConsoleSilence.fold(on)
    return fold_raw(on == true)
end

-- 这一行会被谁吞？只读镜子：不推进节流窗口、不加命中数。
function ConsoleSilence.which(line)
    if type(line) ~= "string" then
        fail("which", "line must be a string")
    end
    return which_raw(line)
end
