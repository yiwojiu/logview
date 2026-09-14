#!/usr/bin/env bash
#
# 把裸可执行文件包装成 macOS 应用包（.app）。
#
# 为什么非做不可：Finder 遇到裸 Mach-O 二进制，会当作脚本交给 Terminal 执行，
# 双击的结果就是弹出一个终端窗口。只有 .app 这种目录结构（内含 Info.plist）
# 才会被 LaunchServices 识别为应用程序，双击才是正常启动。
# Windows 平台靠 PE 头部的子系统标记解决，macOS 没有对应机制，只能打包。
#
# 用法：bash assets/make-app.sh <二进制路径> <输出目录> <版本号>
set -euo pipefail

BIN="${1:?需要二进制路径}"
OUT_DIR="${2:?需要输出目录}"
VERSION="${3:?需要版本号}"

ASSETS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP="${OUT_DIR%/}/logview.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "$BIN" "$APP/Contents/MacOS/logview"
chmod 755 "$APP/Contents/MacOS/logview"

ICON_ENTRY=""
if [ -f "$ASSETS/icon.icns" ]; then
  cp "$ASSETS/icon.icns" "$APP/Contents/Resources/icon.icns"
  ICON_ENTRY="  <key>CFBundleIconFile</key><string>icon</string>"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>logview</string>
  <key>CFBundleDisplayName</key><string>logview</string>
  <key>CFBundleIdentifier</key><string>com.yiwojiu.logview</string>
  <key>CFBundleExecutable</key><string>logview</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>10.15</string>
  <key>NSHighResolutionCapable</key><true/>
${ICON_ENTRY}
</dict>
</plist>
PLIST

# 残留的扩展属性会让 Gatekeeper 在别人的机器上直接拒绝启动
xattr -cr "$APP" 2>/dev/null || true

# ad-hoc 签名：Apple Silicon 要求可执行文件至少带这么一层签名
if ! codesign --force --sign - "$APP" >/dev/null 2>&1; then
  echo "警告：ad-hoc 签名失败，应用仍可使用，但首次打开需右键选择「打开」"
fi

echo "已生成 ${APP}"
