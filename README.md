# logview

[![CI](https://github.com/iot_xush/logview/actions/workflows/ci.yml/badge.svg)](https://github.com/iot_xush/logview/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)

跨平台大日志查看器。Rust + egui，macOS / Windows / Linux 三端同一套代码。

200 MB / 161 万行的日志，打开和搜索都在几十毫秒内：

| 操作 | 耗时 |
|---|---|
| 打开（mmap 映射，不读内容） | 0.000s |
| 首屏可见 | 0.002s |
| 全量建立行索引 | 0.126s |
| 随机读 5000 行 | 0.002s |
| 搜索关键字 | 0.005 – 0.024s |

## 功能

- **大文件秒开**：mmap 映射 + 行偏移索引，1GB 日志和 10MB 占用一样，边建索引边显示
- **虚拟滚动**：只渲染屏幕上可见的几十行，200 万行的文件滚动不卡
- **tail -f 跟随**：自动检测文件追加，日志轮转（截断重写）也能正确重建索引
- **搜索**：字节级匹配，区分大小写可选，命中行号跳转（◀ ▶）
- **只显示匹配行**：过滤模式，把日志当 grep 用
- **级别着色**：FATAL/ERROR 红、WARN 橙、INFO 蓝、DEBUG/TRACE 灰
- **编码自适应**：UTF-8 优先，失败回退 GB18030，Windows 中文环境的 GBK 日志不乱码
- **深浅主题**切换，自动加载系统中文字体
- 支持拖拽文件进窗口，也支持 `logview app.log` 命令行打开

## 跑起来

```bash
cargo run                      # 空窗口，拖文件进去
cargo run -- /var/log/app.log  # 直接打开
cargo run --release -- /var/log/app.log
```

## 验证

```bash
cargo test                     # 核心逻辑测试（编码/索引/搜索/轮转/追加）
cargo run --example fontcheck  # 检查中文字体是否加载成功（退出码 1 = 未生效）
cargo run --release --example bench /tmp/big.log 200   # 200MB 压力测试
```

## 架构

| 文件 | 职责 |
|---|---|
| `src/logstore.rs` | mmap + 行偏移索引、增量索引线程、tail 检测、编码嗅探与按行解码、后台搜索 |
| `src/app.rs` | egui 界面：虚拟滚动列表、高亮、级别着色、状态栏 |
| `src/fonts.rs` | 按平台探测并加载系统 CJK 字体 |

几个关键取舍：

- **不把文件读进内存**。只保存「每行起始偏移」数组，随机访问任意行是 O(1)。
- **解码只发生在渲染时**。大文件里绝大多数行永远不会被解码，这是能秒开的主要原因。
- **搜索走字节**。UTF-8 日志直接用 `memchr::memmem` 在原始字节上找，不做字符串转换；忽略大小写时才逐行处理。
- **索引后台增量**。用 channel 批量回传，不阻塞 UI，打开瞬间就能看到前面的行。

## 跨平台构建

三端代码一致，但要在本机编译目标平台的产物：

```bash
# 当前平台
cargo build --release

# macOS → Windows（需先装目标与链接器）
rustup target add x86_64-pc-windows-gnu
brew install mingw-w64
cargo build --release --target x86_64-pc-windows-gnu

# macOS → Linux
rustup target add x86_64-unknown-linux-gnu
brew install FiloSottile/musl-cross/musl-cross   # musl 静态链接
cargo build --release --target x86_64-unknown-linux-musl
```

产物在 `target/<target>/release/logview`（Windows 是 `logview.exe`）。

### 发布版本

推一个 `v` 开头的 tag 即可，`.github/workflows/release.yml` 会自动构建四个平台的产物、
生成 SHA256 校验和，并创建 GitHub Release（含自动生成的更新说明）：

```bash
git tag v0.1.0
git push origin v0.1.0
```

产物：

| 文件 | 平台 |
|---|---|
| `logview-aarch64-apple-darwin.tar.gz` | macOS（Apple Silicon） |
| `logview-x86_64-apple-darwin.tar.gz` | macOS（Intel） |
| `logview-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64 |
| `logview-x86_64-pc-windows-msvc.zip` | Windows x86_64 |

注：macOS 产物未做代码签名，用户首次打开需要右键→打开，或 `xattr -d com.apple.quarantine logview`。
Linux 产物链接的是 Ubuntu 24.04 的 glibc，太老的发行版可能跑不起来。

需要 dmg / msi / AppImage 这类安装包，可以再引入 `cargo-dist`（`cargo dist init` 会生成配套工作流）。

## 已知限制

- 只支持 UTF-8 / GB18030。UTF-16 日志（Windows 上少见）会按 GB18030 解，显示异常，需要的话可以加 BOM 探测分支。
- 行偏移索引按 8 字节/行算，1GB、平均行宽 50 字节的日志约占 160MB 内存。极端场景可改用 `u32` + 分块。
- 搜索命中数上限 5 万（`MAX_SEARCH_HITS`），超出部分不统计。
- 切换大小写敏感后需要重新触发一次搜索（改选项后在搜索框回车即可）。

## 开发

```bash
cargo fmt --all -- --check    # 格式
cargo clippy --all-targets    # 静态检查，CI 里 -D warnings
cargo test                    # 测试
```

CI 在 ubuntu / macos / windows 三个平台跑上面三项，配置见 `.github/workflows/ci.yml`；
打 tag 出包见 `.github/workflows/release.yml`。

## 许可证

双许可，任选其一：

- MIT License（[LICENSE-MIT](LICENSE-MIT)）
- Apache License 2.0（[LICENSE-APACHE](LICENSE-APACHE)）

与 Rust 生态主流项目（egui、eframe 等）保持一致，便于使用者组合。
