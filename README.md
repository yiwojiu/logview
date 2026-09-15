<p align="center">
  <img src="assets/icon.png" width="112" alt="logview 图标">
</p>

# logview

[![CI](https://github.com/yiwojiu/logview/actions/workflows/ci.yml/badge.svg)](https://github.com/yiwojiu/logview/actions/workflows/ci.yml)
[![Release](https://github.com/yiwojiu/logview/actions/workflows/release.yml/badge.svg)](https://github.com/yiwojiu/logview/actions/workflows/release.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)
[![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-lightgrey.svg)](#安装)

> Cross-platform desktop viewer for very large log files, built with Rust and egui.
> Opens 200 MB / 1.6 M-line files instantly and never loads the whole file into memory.

跨平台大日志查看器，基于 Rust 与 egui 实现，macOS / Windows / Linux 共用同一套代码。

目标场景是几百 MB 量级的服务端日志：用文本编辑器打开需要漫长等待甚至直接卡死，
而 `grep` 又看不到上下文。本项目通过内存映射与行偏移索引，使这类文件的打开、
检索和实时跟随均保持在毫秒级。

---

## 目录

- [特性](#特性)
- [性能实测](#性能实测)
- [安装](#安装)
- [使用方法](#使用方法)
- [设计要点](#设计要点)
- [项目结构](#项目结构)
- [开发与测试](#开发与测试)
- [跨平台构建](#跨平台构建)
- [发布流程](#发布流程)
- [已知限制](#已知限制)
- [许可证](#许可证)

## 特性

| 特性 | 说明 |
|---|---|
| 大文件秒开 | 内存映射配合行偏移索引，文件内容不读入内存，索引边构建边显示 |
| 虚拟滚动 | 每帧仅布局并解码可见的数十行，百万行文件滚动依然流畅 |
| 实时跟随 | 等价于 `tail -f`，自动检测文件追加；支持日志轮转（重命名后重建） |
| 全文检索 | 字节级匹配，可选择区分大小写，命中项高亮并支持前后跳转 |
| 正则检索 | 可切换为正则表达式，例如 `ERROR\|FATAL`、`orderId=\d+` |
| 命中定位 | 当前跳转到的匹配行整行加底色，百万行里也能一眼看出落在哪一条 |
| 过滤视图 | 仅显示匹配行，可作为交互式 `grep` 使用 |
| 行首识别 | 解析行首的时间戳与级别：日期淡出、时间弱化；WARN / ERROR 另带左侧色条与底色标签 |
| 编码自适应 | 优先 UTF-8，失败时回退 GB18030，Windows 中文环境的 GBK 日志不会乱码 |
| 中文字体 | 按平台自动探测并加载系统 CJK 字体 |
| 界面 | 主题三态切换（跟随系统 / 浅色 / 深色），支持拖拽文件打开与命令行参数 |

界面组成：

```
┌────────────────────────────────────────────────────────────────┐
│ [打开文件]  /var/log/app.log     □仅匹配行 □跟随尾部 □自动换行 ◑ │
│ 搜索框                       □区分大小写 □正则    ◀ ▶ 3/342 清空 ? │
├────────┬───────────────────────────────────────────────────────┤
│  12845 │ 2026-09-13 08:12:03.441 ERROR 处理订单失败 …           │
│  12846 │ 2026-09-13 08:12:03.442 INFO  重试第 1 次 …            │
│  12847 │▌2026-09-13 08:12:03.445 WARN  耗时 1204ms …            │
├────────┴───────────────────────────────────────────────────────┤
│ 1615656 行 │ 200.0 MB │ UTF-8 │ 就绪 │ 342 匹配                  │
└────────────────────────────────────────────────────────────────┘
```

工具栏刻意分成两层：上层是「看什么文件、怎么显示」，下层是「搜什么、跳到哪」。
「打开文件」是唯一带实心底色的按钮（它是打开软件后第一个要点的东西），
其余控件一律弱化。右侧的 `◑` 是主题开关，图标表示当前状态。

WARN / ERROR 的行会在最左侧挂一条色条（上图用 `▌` 表示），滚动时异常能自己跳出来；
命中导航只在有检索词时出现，空着比显示一个 `0 / 0` 干净。

## 性能实测

200 MB / 1,615,656 行，release 构建，macOS x86_64：

| 操作 | 耗时 |
|---|---|
| 打开文件（建立内存映射，不读取内容） | 0.000 s |
| 首屏可见 | 0.002 s |
| 全量建立行索引 | 0.126 s |
| 随机读取 5000 行 | 0.002 s |
| 子串检索 | 0.023 s |
| 正则检索 | 0.241 s |

正则检索要逐行解码后再匹配，约比子串检索慢十倍——但 200 MB 也只要 0.24 秒，
日常使用感知不到。子串检索走的是原始字节匹配，所以更快。

检索一律**扫完整个文件**。保留上限只限制"可跳转"的命中，总数照实统计，所以不给
"命中够多就收工"的捷径：先返回的那版是 0.009 s / 0.025 s，快出来的几十毫秒，
换来的是总数准确、文件后半段也不会变成盲区。

复现方式见[开发与测试](#开发与测试)。

## 安装

### 预编译产物

从 [Releases](https://github.com/yiwojiu/logview/releases) 下载对应平台的文件，
解压后即可运行：

| 文件 | 平台 |
|---|---|
| `logview-v0.1.9-aarch64-apple-darwin.tar.gz` | macOS（Apple Silicon） |
| `logview-v0.1.9-x86_64-apple-darwin.tar.gz` | macOS（Intel） |
| `logview-v0.1.9-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64 |
| `logview-v0.1.9-x86_64-pc-windows-msvc.zip` | Windows x86_64 |

文件名里的版本号与 Release 标签一致，下载到本地堆几个版本也不会混淆
（v0.1.8 及更早的包名不带版本号）。

macOS 的压缩包解压后是 `logview.app`，**双击即可运行**——不是裸二进制，
所以不会弹出终端窗口。若想在终端里使用，执行
`logview.app/Contents/MacOS/logview`。

发布页附带 `SHA256SUMS.txt`，可用于校验文件完整性：

```bash
shasum -a 256 -c SHA256SUMS.txt        # macOS
sha256sum -c SHA256SUMS.txt            # Linux
```

Windows 需要 [VC++ 2015-2022 运行库](https://aka.ms/vs/17/release/vc_redist.x64.exe)
（绝大多数机器已预装；缺失时 Windows 会提示缺少 `VCRUNTIME140.dll`）。

Linux 产物要求 **glibc ≥ 2.17**（CentOS/RHEL 7+、Debian 8+、Ubuntu 14.04+ 都满足）。
发布流程用 [zig](https://ziglang.org/) 当链接器把这个底线钉死，否则默认构建机
（Ubuntu 24.04，glibc 2.39）编出来的产物会在 CentOS 8、Ubuntu 22.04 这类机器上报
`GLIBC_2.34 not found` —— 看着像文件坏了，其实是构建环境太新。产物只硬依赖
`libc` / `libm` / `libgcc_s`，X11、Wayland、OpenGL 都是**运行时**动态加载的。

> **Linux 上需要图形环境。** 这是 GUI 程序：进程能起来，但要弹出窗口得有 X11 或 Wayland
> （纯 SSH 终端里跑不了，远程看建议在本地或远程桌面上开 Windows 版）。
> 起不来时原因会打到 stderr，终端里能看到。

想让它出现在应用菜单里、并带上自己的图标，把压缩包里的 `.desktop` 与 `icons/` 装到用户目录：

```bash
install -Dm755 logview ~/.local/bin/logview
install -Dm644 logview.desktop ~/.local/share/applications/logview.desktop
cp -r icons/* ~/.local/share/icons/
update-desktop-database ~/.local/share/applications 2>/dev/null || true
```

`icons/` 是 hicolor 主题的标准尺寸（16 / 32 / 64 / 128 / 256 / 512）。
**图标为什么要在外面装**：Linux 不像 Windows 把图标嵌进可执行文件，也不像 macOS 打进 `.app`——
窗口图标是运行时设的（X11 下走 `_NET_WM_ICON`，不装也能看到），
而 **Wayland 下合成器要靠 `.desktop` 把窗口与图标对上**，不装就只有默认图标。

渲染走 **D3D12**（wgpu），远程桌面会话、虚拟机与没装显卡驱动的机器上都能正常打开；
没有硬件显卡时会回退到软件渲染（WARP），界面能用但滚动会慢一些。
不用 OpenGL 是因为远程桌面的显示驱动往往只提供 OpenGL 1.1，那种环境下 OpenGL
后端根本建不起窗口——而"在服务器上看日志"恰恰是这类工具的常见用法。
万一窗口仍然起不来，程序会弹出对话框说明原因（release 没有 stderr，不弹就只能看到
「双击没反应」）。

> **macOS 首次打开的额外一步。** 产物只做了 ad-hoc 签名，没有 Apple 开发者
> 签名与公证，因此 Gatekeeper 会拦下第一次启动——这是预期行为，不表示文件损坏。
>
> 处理方式：把 `logview.app` 拖入「应用程序」，然后打开
> **系统设置 → 隐私与安全性**，在页面下方找到关于 logview 的提示，点击「仍要打开」。
> 之后即可正常双击启动，不会再被拦。
>
> 想免除这一步需要 Apple 开发者账号做正式签名与公证（年费 99 美元）。

### 从源码构建

需要稳定的 Rust 工具链（本项目在 1.88.0 上开发验证）。

```bash
git clone https://github.com/yiwojiu/logview.git
cd logview
cargo build --release
```

产物位于 `target/release/logview`，Windows 下为 `logview.exe`。

Windows 的 exe 文件图标由 `build.rs` 处理：它把 `assets/icon.rc` 编译成 PE 资源再交给链接器。
资源编译器 `rc.exe` 依次从 `RC` 环境变量、Windows SDK 安装目录、`PATH` 中查找；三者都没有时
只打印一条警告并跳过，不会中断构建，产物退化为系统默认图标。其他平台不需要这一步。

> 运行时窗口与任务栏的图标走的是另一条路径（`ViewportBuilder::with_icon` + `assets/icon-128.rgba`），
> 两者互不替代：只有后者时，窗口内看起来正常，但资源管理器里的 exe 仍是系统默认图标。

Windows 的 release 构建切换为图形子系统（`#![windows_subsystem = "windows"]`），双击运行不再附带
控制台窗口。代价是进程没有 stderr：为此 release 构建会把 panic 内容弹成对话框，避免再次出现
「窗口一闪就没了」却查不到原因的情况。debug 构建保留控制台，`cargo run` 时照旧直接看终端输出。

## 使用方法

```bash
logview                          # 启动空窗口，将日志文件拖入即可
logview /var/log/app.log         # 直接打开指定文件
```

打开文件后：

1. **打开文件** —— 点击「打开文件」按钮、将文件拖入窗口，或通过命令行参数传入。
2. **检索** —— 在搜索框输入关键字，输入停止约 250 ms 后自动开始检索并定位至首个命中项；
   使用 `◀` `▶` 或 `n` / `N` 在命中项之间跳转。
   勾选「正则」后可输入正则表达式（如 `ERROR|FATAL`、`orderId=\d+`），
   表达式非法时状态栏会给出提示。

   双击行内的词同样会检索它，但**视图原地不动**：读到一半时被拽回文件开头是最让人恼火的一种打断。
   此时游标落在视口下方最近的命中上，`n` 就是「从当前位置往下找下一个」。
3. **过滤** —— 勾选「只显示匹配行」后仅保留命中行，适合快速筛查错误。
4. **跟随** —— 勾选「跟随尾部」（默认关闭，打开文件先停在开头）后视图自动滚动至最新行，
   等价于 `tail -f`。手动跳转命中项时会自动取消跟随，避免被拉回底部。
5. **编码** —— 打开文件时自动判定，结果显示在状态栏；非 UTF-8 文件按 GB18030 解码。
6. **看级别** —— 行首的时间戳会淡化成次要灰（对比度保证在 WCAG AA 以上，看得清但让位于正文）；
   WARN / ERROR 额外带左侧色条与
   底色标签，滚动时一眼扫得出异常。级别只从行首一小段里认，且必须是独立单词，
   所以正文里提到 `ERROR` 不会被误当成错误行；认不出的行按普通文本显示。

### 快捷键

| 按键 | 动作 |
|---|---|
| `⌘F` 或 `/` | 聚焦搜索框 |
| `n` / `N` | 下一个 / 上一个命中 |
| `g` / `G` | 跳到文件开头 / 末尾 |
| `Esc` | 清空检索 |
| `⌘O` | 打开文件 |
| 双击行内文字 | 就地检索该词；视图不移动，`n` 从当前位置往下找下一个命中 |

行内文字也可以像文本编辑器那样拖拽划选，再用 `⌘C` 复制。

裸字母键仅在搜索框未获得焦点时才作为快捷键，所以在搜索框内可以正常输入 `n`、`g` 等字符。
Windows 与 Linux 上 `⌘` 对应 `Ctrl`。

## 设计要点

核心目标是在**不将文件读入内存**的前提下支持任意位置随机访问。为此做出以下取舍：

- **以内存映射代替读取**。文件通过 `mmap` 映射为只读视图，打开操作不复制任何数据，
  因而与文件大小无关。
- **仅索引行起始位置**。不保存行内容，只维护一个数组记录每行的起始字节偏移，
  定位任意行的时间复杂度为 O(1)。
- **解码发生在渲染时刻**。仅对当前可见的数十行执行编码转换，文件中绝大部分行
  在整个会话期间不会被解码——这是能够秒开大文件的主要原因。
- **索引后台增量构建**。索引线程分批回传行偏移，界面在索引尚未完成时即可交互。
- **检索在字节层面完成**。UTF-8 日志直接使用 `memchr::memmem` 在原始字节中匹配，
  避免构造字符串；仅在忽略大小写时退化为逐行处理。
- **按行解码而非整体转换**。GB18030 等编码不预先整体转换，避免额外占用一份文件大小的内存。
- **行号与像素的换算与渲染行距同源**。虚拟滚动按固定行距布局，所以跳转定位使用的行距
  必须与 `show_rows` 的模型一致（文本行高 + `item_spacing`），差一点都会随行号累积成
  大幅偏移。行高取自运行时字体度量而非写死的像素值，各平台中文字体与屏幕缩放不同也不失真。
- **只解析行首，不给整行染色**。日志行的信息是结构化的，所以只认行首那一小段：
  时间戳整体淡化为可读的次要灰、级别单独着色。识别范围限于时间戳之后的头几个 token，且 token 里
  一旦出现非 ASCII 就停下——中文没有空格分词，整段中文会算作一个 token，
  正文里提到的 `ERROR`（如"检测到 3 个 ERROR 已忽略"）因此不会被误判成错误行。
  刻意**不做**时间戳 / logger / 正文的分列对齐：那要假设固定的日志格式，
  猜错会让非标准格式的日志比不排版更难读。
- **只强调 WARN 与 ERROR**。左侧色条与底色标签只给这两级，INFO / DEBUG 仅调整文字色。
  正常行在日志里占绝大多数，若每行都挂色条，左侧会连成一条满屏的线，
  色条也就失去了"让异常跳出来"的作用。色块用 `TextFormat::background` 实现，
  不改变字符位置，所以不会牵动上面那条行距换算。

## 项目结构

| 路径 | 职责 |
|---|---|
| `src/logstore.rs` | 内存映射、行偏移索引、增量索引线程、文件变更检测、编码判定与按行解码、后台检索 |
| `src/app.rs` | egui 界面：虚拟滚动列表、检索高亮、级别着色、工具栏与状态栏 |
| `src/fonts.rs` | 按平台探测并加载系统 CJK 字体 |
| `src/main.rs` | 程序入口、命令行参数、release 下的图形子系统与 panic 对话框 |
| `build.rs` | 构建脚本：Windows 下把 `assets/icon.rc` 编译成 PE 资源链接进 exe |
| `assets/` | 图标素材；`icon.ico` 供资源编译，`icon-128.rgba` 供运行时窗口图标 |
| `tests/store_test.rs` | 集成测试，覆盖编码判定、索引、检索、文件增长与轮转 |
| `examples/bench.rs` | 大文件压力测试，输出打开 / 索引 / 检索耗时 |
| `examples/fontcheck.rs` | 中文字体加载自检 |

`logstore` 与 `app` 之间仅通过 `LogStore` 的公开接口通信，界面改动不涉及数据层。

## 开发与测试

```bash
cargo fmt --all -- --check    # 格式检查
cargo clippy --all-targets    # 静态检查
cargo test                    # 运行测试（单元测试 + 18 项集成测试）

# 中文字体自检；退出码非 0 表示字体未生效
cargo run --example fontcheck

# 压力测试：生成 200 MB 日志并测量各项耗时
cargo run --release --example bench /tmp/bench.log 200
```

CI 在 ubuntu / macos / windows 三个平台执行格式检查、Clippy、测试与构建，
配置见 [`.github/workflows/ci.yml`](.github/workflows/ci.yml)。

## 跨平台构建

```bash
# 构建当前平台产物
cargo build --release

# macOS 交叉编译 Windows（需先安装目标与链接器）
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu

# macOS 交叉编译 Linux（musl 静态链接）
rustup target add x86_64-unknown-linux-musl
brew install FiloSottile/musl-cross/musl-cross
cargo build --release --target x86_64-unknown-linux-musl
```

产物位于 `target/<target>/release/`。

交叉编译到 Windows 的 GNU 目标（`x86_64-pc-windows-gnu`）**不会**嵌入 exe 图标：
PE 资源是通过 MSVC 链接器认识的 `.res` 送入的，这条路径只覆盖 MSVC 目标。

## 发布流程

推送以 `v` 开头的标签即可触发 [`.github/workflows/release.yml`](.github/workflows/release.yml)，
自动构建四个平台的产物、生成 SHA256 校验和并创建 GitHub Release：

```bash
git tag v0.1.0
git push origin v0.1.0
```

## 已知限制

- **仅支持 UTF-8 与 GB18030**。UTF-16 编码的日志会按 GB18030 解码而产生乱码，
  如需支持可增加 BOM 判定分支。
- **索引占用内存**。行偏移数组按每行 8 字节计算，1 GB、平均行宽 50 字节的日志
  约占 160 MB 内存。极端场景可改用 `u32` 配合分块以降低占用。
- **可跳转的命中行数上限为 100 万行**（`MAX_NAVIGABLE_LINES`）。额度按**行**计：一行里同一个词
  出现多次只占一份。全文照常扫描、总数照实统计，所以只有命中行数真的超过 100 万
  （约等于上千万行的巨型日志）才会截断；此时状态栏会给出可跳转的行数、覆盖到第几行，
  以及全文命中总数。
- **「自动换行」开启时跳转不保证精确**。行高不再固定（一条长行会占多行），
  行号与像素之间不再是线性关系，`n` / `N` 与双击取词后的定位会有偏差；
  关闭「自动换行」时按固定行距精确定位。
- **Windows 平台的文件映射语义**。本程序打开某个日志文件期间，日志写入方无法
  就地截断该文件：内存映射存活时，Windows 会拒绝 `SetEndOfFile` 调用。
  因此 Windows 下建议采用「重命名 + 新建文件」的轮转方式，程序会自动跟进新文件；
  就地截断式的轮转仅在 Unix 平台有效。

## 许可证

本项目采用双许可证，使用者可任选其一：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

与 Rust 生态主流项目（egui、eframe 等）保持一致，便于组合使用。
