# RE_FINDINGS — 控制台打印层（KCD2 1.5.6）

本文件只记录**做过取证**的事实，以及每条事实的取证方式。凡是没做到的，写在
"未取证"一节里，不当作已知。

环境：`WHGame.dll` 89,180,672 字节（1.5.6；size `0x5B2D000`，sha256 `bdf8f9e4…4479f7`）。

---

## 1. 需求的实际形状

现象：用户从默认 LUT 切到 ERVWHDO 里的其他 filter 之后，"防关闭 LUT 的保护"开始
刷控制台；脚本层压不掉命令输出，所以在 native 层处理。

> **版本**：测试用的 ERVWHDO 构建里 `ervwhdo.lua` = 722,608 B
> （`Data\ERVWHDO-part1.pak`，94,399,354 B）。下面引用的行号以该构建为准；它的
> `SetLUTSettings` 里**直接**调用 `StartLUTGuard(1500000)`，所以"换一个 filter 就
> 开始刷"是一条必然走到的路径。

模组侧机制（`Mods/ERVWHDO/Data/ERVWHDO-part*.pak` → `Scripts/mods/ervwhdo.lua`）：

```lua
local function ApplyLUTChart(path)
    System.ExecuteCommand("r_ColorGradingChartImage " .. path)
end

ERVWHDO.lutGuard = { timer_id = nil, duration = 0, elapsed = 0, interval = 16 }

function ERVWHDO.StartLUTGuard(durationMs)          -- 8110 行
    ...
    ApplyLUTChart(ERVWHDO.selectedLUT)
    local function tick()
        ApplyLUTChart(ERVWHDO.selectedLUT)          -- 每 16 ms 一次
        guard.elapsed = guard.elapsed + guard.interval
        ...
        guard.timer_id = Script.SetTimer(guard.interval, tick)
    end
    guard.timer_id = Script.SetTimer(guard.interval, tick)
end
```

调用点给的 `duration` 是 `1500000` ms（25 分钟；`SetLUTSettings` 8583 行、天气/读档
等多处），即最长约 **93,750 次** `ExecuteCommand`。`tick` 里没有任何 `LogAlways`，
所以"每 tick 一行"只可能来自引擎侧，不来自这段脚本。

## 2. `IConsole` 打印层（本次新取证）

### 2.1 定位链

```
gEnv                        = WHGame 锚点扫描（"exec autoexec.cfg" → lea → mov [rip+],rcx − 0xA8）
gEnv->pConsole (+0xA8)      = CXConsole 实例
*(CXConsole)                = vtable
vtable + 26*8               = IConsole::PrintLine
```

运行时实测（`consolesilence.log`）：

```
WHGame.dll base=0x7FF837580000 size=0x5B2D000
gEnv=0x7FF83BEAD800 (pConsole=0x7FF83BEAD8A8)
PrintLine hooked: console=0x1FE80C85090 vtable=0x7FF83B34F850 slot=0x7FF83B34F920
                  original=0x7FF837E59348
```

`0x7FF837E59348 − 0x7FF837580000 = 0x8D9348`，正是 vtable 槽位 26 里存的地址。

### 2.2 vtable 归属与槽位语义（IDA）

`re/scripts/vtable_probe.py`（打开 `WHGame_V1.5.6.dll.i64`）：

- vtable RVA `0x3DCF850` 的 RTTI 类名 = **`.?AVCXConsole@@`**（vtable 前 8 字节是
  指向 Complete Object Locator 的指针，COL → TypeDescriptor → 名字）。
- 逐槽反编译与 CryEngine SDK 的 `IConsole` 声明顺序逐条对齐：

| 槽 | 反编译结论 | 对应 `IConsole` 方法 |
|---|---|---|
| 0 | `sub_182474444(a1, flags)`，`flags&1` 时调用 `sub_181AB5160(a1, 576)` | 虚析构（scalar deleting） |
| 20 | 载入 `"Textures/Console/loadscreen_default.dds"`，写 `a1+108` | `SetLoadingImage` |
| 21 | `char f(a1, int lineNo, char* buf, int size)`；`*buf=0`；`lineNo >= *(int*)(a1+56)` 时返回 0 | `GetLineNo` |
| 22 | 4 字节，返回 `*(int*)(a1+56)` | `GetLineCount` |
| 26 | thunk → `sub_1808D9350` | `PrintLine` |
| 27 | thunk → `sub_182474D90` | `PrintLinePlus` |

`sub_1808D9350`（`PrintLine`）形状：把参数格式化成 `CryString` → 去掉末尾
`\n`/`\r` → 把行内所有 `\n`、`\r` 换成空格 → `sub_1808D9734(a1+24, &str)`
（环形行缓冲入队）→ 超过 `dword_18491D298` 条就裁剪 →
对 `a1+256..a1+264` 里的每个 **output sink** 调 `(*(vtable+8))(sink, str)`。

也就是说：控制台"显示什么"的权威来源是 **`a1+24` 那个环形缓冲**，公开入口是
`PrintLine` / `PrintLinePlus`。

### 2.3 `geEnv->pConsole->PrintLine` 索引的旁证

随游戏发布的 `kcd2db.asi` 用同一份 SDK 头文件编译，`gEnv->pConsole->PrintLine(...)`
在二进制里编译成唯一的 `call qword ptr [rax+0D0h]`（`0xD0 = 26*8`，文件偏移
`0x3498D`）。它在实机上工作，等于对"头文件布局 == 零售游戏布局"做了一次行为验证。

## 3. 引擎侧什么时候回显 chart cvar？——**只在控制台打开时**

`tools/lua-run.ps1` 通过 kcd2db 的 TCP Lua runner 在游戏线程执行脚本，同时
`IConsole::PrintLine` 被本 ASI 钩住（观察模式把每一行落盘）。

| 探针 | 控制台行 |
|---|---|
| `System.ExecuteCommand("totally_not_a_command_xyz 1")` | `$6[Warning] Unknown command: totally_not_a_command_xyz` ✅ 说明命令错误确实走 `PrintLine` |
| `System.ExecuteCommand("r_ColorGradingChartImage")`（无参数） | 无 |
| 同上 + 不存在的路径 | 无 |
| 同上 + 有效 chart（`…/1.New.Line.Cinema.dds`） | 无 |
| 依次应用模组自带的全部 36 张 chart | 无 |
| `ERVWHDO.StartLUTGuard(2000)`（≈125 次执行） | 无 |
| `ERVWHDO.StartLUTGuard(15000)`（≈937 次执行） | 无 |
| 交付版菜单路径 `ERVWHDO:SetLUTSettings(chart)`（自动 arm 1500000 ms 守卫，跑 20 s） | 无（守卫确实 armed：探针读到 `guardArmed=true`） |

上面的探针表是在**控制台关闭**的状态下跑的，所以一条都不出现。控制台打开时行为相反：
`CXConsole::ExecuteString` 的工作函数里有
`if (!bSilentMode && this->GetStatus()) PrintLine(cmd);`，于是守卫每 16 ms 一条命令就变成
每 16 ms 一行回显，内容正是 `r_ColorGradingChartImage Data//Textures//colorcharts//cinematic//3.a24.dds`
（用户截图即为此现象）。也就是说：这一行来自引擎对**命令本身**的回显，不是脚本里的
`LogAlways`，也不是维度告警（后者是另一族，见 §3.1）。这也是过滤器要钩 `ExecuteString`
而不是只钩 `PrintLine` 的原因。

### 3.1 引擎确实会"每次应用都告警"的一族

`re/scripts/find_string.py` 查 `"Unsupported color chart dimensions"`：

```
va=0x184773E00 rva=0x4773E00
xref 0x1804E3E95 in sub_1804E3D88
```

`sub_1804E3D88` 校验 chart 的宽高（要求 `256 x 16`，即 16³ LUT 的 16 张 16×16
拼图），不满足时经 `qword_18492D8E0`（**`ILog`**，vtable 槽 40）输出这条告警——
**走日志接口，不走控制台 `PrintLine`**。模组自带的 36 张 chart 都是 16532 字节
（128 字节头 + 256×16×4），全部合规，所以这条在该构建上不会触发。

这解释了"用户自定义/损坏的 LUT → 每秒 60 条告警"这类环境，但它进的是
`kcd.log`；要让它在控制台也消失，得另钩 `ILog`（本 ASI 明确不做，理由见 §5）。

## 4. 因此本 ASI 的取舍

| 层 | 拦不拦 | 理由 |
|---|---|---|
| `IConsole::PrintLine`（槽 26） | **拦** | 用户"在控制台里看见的那一行"的唯一公开入口；吞在这里不影响 `kcd.log` |
| `IConsole::PrintLinePlus`（槽 27） | 不拦（未取证它的调用面） | 它只"续写上一行"，用于进度条；拦它风险大于收益 |
| `ILog`（日志中枢） | **不拦** | 拦它会把 `kcd.log` 一起吞——作者和用户排查问题要靠这份日志 |

因为确切的刷屏文本曾无法复现（§3），所以过滤不依赖它：

1. 按 token 的规则（`r_ColorGradingChartImage`）由模组在运行时声明；命中走
   `ExecuteString` 的 `bSilentMode`，命令照跑、回显根本不产生（所以 `dropped` 不动）；
2. **重复行折叠**：完全相同的一行在 1 s 内出现到第 3 次就折叠，不需要知道文本，
   所以刷屏文案换了也仍然成立。

`Unsupported color chart dimensions` 不在可拦范围内（§3.1：走 `ILog`，槽 40），不要为它注册规则。

## 5. 未取证 / 边界

- 那一行的原文**已知**（用户截图）：
  `r_ColorGradingChartImage Data//Textures//colorcharts//cinematic//3.a24.dds`。
  2026-11-03 手动取证：控制台显示（`con_show 1`）、零规则时，环形缓冲里稳定有 ~200 条
  chart 行在滚动，`seen` 涨到 27k；在控制台敲
  `#ConsoleSilence.mute("r_ColorGradingChartImage", { owner = "ERVWHDO" })` 后滚动停止，
  `commandsSilenced` 涨到 852，全程 `dropped = 0`（行根本没产生，不是被丢掉）。
  控制台关闭时不产生这一行——这就是 §3 探针表"一条都没有"的原因。
- `PrintLinePlus` 的实际调用面没统计过，因此没有钩它。
- 只对 1.5.6 的 `WHGame.dll` 做过实机验证；`gEnv` 定位是锚点扫描（跨版本自适应），
  vtable 槽位用"槽位必须指向 `WHGame.dll` 内"做 fail-closed 校验，但换引擎大版本
  仍需重新取证（`re/scripts/vtable_probe.py` 一条命令）。
- 环形缓冲上限 `dword_18491D298` 的具体数值没读（只用于转储条数上限，不影响过滤）。
