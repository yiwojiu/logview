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
| 过滤视图 | 仅显示匹配行，可作为交互式 `grep` 使用 |
| 级别着色 | FATAL / ERROR / WARN / INFO / DEBUG / TRACE 按级别区分颜色 |
| 编码自适应 | 优先 UTF-8，失败时回退 GB18030，Windows 中文环境的 GBK 日志不会乱码 |
| 中文字体 | 按平台自动探测并加载系统 CJK 字体 |
| 界面 | 深色 / 浅色主题切换，支持拖拽文件打开与命令行参数 |

界面组成：

```
┌────────────────────────────────────────────────────────────┐
│ [打开文件]  /var/log/app.log                    [主题切换]  │
│ 搜索框                  □区分大小写 □仅匹配行 □跟随尾部 ◀ ▶ │
├────────┬───────────────────────────────────────────────────┤
│  12845 │ 2026-09-13 08:12:03.441 [ERROR] 处理订单失败 …    │
│  12846 │ 2026-09-13 08:12:03.442 [INFO ] 重试第 1 次 …     │
│  12847 │ 2026-09-13 08:12:03.445 [WARN ] 耗时 1204ms …     │
├────────┴───────────────────────────────────────────────────┤
│ 1615656 行 │ 200.0 MB │ UTF-8 │ 就绪 │ 342 匹配             │
└────────────────────────────────────────────────────────────┘
```

## 性能实测

200 MB / 1,615,656 行，release 构建，macOS x86_64：

| 操作 | 耗时 |
|---|---|
| 打开文件（建立内存映射，不读取内容） | 0.000 s |
| 首屏可见 | 0.002 s |
| 全量建立行索引 | 0.126 s |
| 随机读取 5000 行 | 0.002 s |
| 检索关键字 | 0.005 – 0.024 s |

复现方式见[开发与测试](#开发与测试)。

## 安装

### 预编译产物

从 [Releases](https://github.com/yiwojiu/logview/releases) 下载对应平台的文件，
解压后即可运行：

| 文件 | 平台 |
|---|---|
| `logview-aarch64-apple-darwin.tar.gz` | macOS（Apple Silicon） |
| `logview-x86_64-apple-darwin.tar.gz` | macOS（Intel） |
| `logview-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64 |
| `logview-x86_64-pc-windows-msvc.zip` | Windows x86_64 |

发布页附带 `SHA256SUMS.txt`，可用于校验文件完整性：

```bash
shasum -a 256 -c SHA256SUMS.txt        # macOS
sha256sum -c SHA256SUMS.txt            # Linux
```

> macOS 产物未做代码签名。首次打开需在 Finder 中右键选择「打开」，
> 或先执行 `xattr -d com.apple.quarantine logview`。

### 从源码构建

需要稳定的 Rust 工具链（本项目在 1.88.0 上开发验证）。

```bash
git clone https://github.com/yiwojiu/logview.git
cd logview
cargo build --release
```

产物位于 `target/release/logview`，Windows 下为 `logview.exe`。

## 使用方法

```bash
logview                          # 启动空窗口，将日志文件拖入即可
logview /var/log/app.log         # 直接打开指定文件
```

打开文件后：

1. **打开文件** —— 点击「打开文件」按钮、将文件拖入窗口，或通过命令行参数传入。
2. **检索** —— 在搜索框输入关键字，输入停止约 250 ms 后自动开始检索并定位至首个命中项；
   使用 `◀` `▶` 在命中项之间跳转。
3. **过滤** —— 勾选「只显示匹配行」后仅保留命中行，适合快速筛查错误。
4. **跟随** —— 勾选「跟随尾部」后视图自动滚动至最新行，等价于 `tail -f`。
   手动跳转命中项时会自动取消跟随，避免被拉回底部。
5. **编码** —— 打开文件时自动判定，结果显示在状态栏；非 UTF-8 文件按 GB18030 解码。

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

## 项目结构

| 路径 | 职责 |
|---|---|
| `src/logstore.rs` | 内存映射、行偏移索引、增量索引线程、文件变更检测、编码判定与按行解码、后台检索 |
| `src/app.rs` | egui 界面：虚拟滚动列表、检索高亮、级别着色、工具栏与状态栏 |
| `src/fonts.rs` | 按平台探测并加载系统 CJK 字体 |
| `src/main.rs` | 程序入口与命令行参数处理 |
| `tests/store_test.rs` | 集成测试，覆盖编码判定、索引、检索、文件增长与轮转 |
| `examples/bench.rs` | 大文件压力测试，输出打开 / 索引 / 检索耗时 |
| `examples/fontcheck.rs` | 中文字体加载自检 |

`logstore` 与 `app` 之间仅通过 `LogStore` 的公开接口通信，界面改动不涉及数据层。

## 开发与测试

```bash
cargo fmt --all -- --check    # 格式检查
cargo clippy --all-targets    # 静态检查
cargo test                    # 运行测试（11 项集成测试）

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
- **检索命中数上限为 5 万**（`MAX_SEARCH_HITS`），超出部分不统计。
  结果被截断时界面会给出提示，避免误认为命中总数即为 5 万。
- **修改「区分大小写」选项后需重新触发检索**（在搜索框按回车即可）。
- **Windows 平台的文件映射语义**。本程序打开某个日志文件期间，日志写入方无法
  就地截断该文件：内存映射存活时，Windows 会拒绝 `SetEndOfFile` 调用。
  因此 Windows 下建议采用「重命名 + 新建文件」的轮转方式，程序会自动跟进新文件；
  就地截断式的轮转仅在 Unix 平台有效。

## 许可证

本项目采用双许可证，使用者可任选其一：

- [MIT License](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

与 Rust 生态主流项目（egui、eframe 等）保持一致，便于组合使用。
