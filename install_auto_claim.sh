#!/bin/bash
# Qoder Switch — 每日自动领取积分（launchd，无需打开应用界面）
set -e
APP="/Applications/Qoder Switch.app"
if [ ! -d "$APP" ]; then
  APP="$(cd "$(dirname "$0")" && pwd)/src-tauri/target/release/bundle/macos/Qoder Switch.app"
fi
BIN="$APP/Contents/MacOS/qoder-switch"
if [ ! -x "$BIN" ]; then
  echo "未找到 Qoder Switch.app，请先构建或将 App 放入 /Applications"; exit 1
fi
PLIST="$HOME/Library/LaunchAgents/com.qoderswitch.claim.plist"
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.qoderswitch.claim</string>
  <key>ProgramArguments</key>
  <array>
    <string>$BIN</string>
    <string>--claim</string>
  </array>
  <key>StartCalendarInterval</key>
  <array>
    <dict><key>Hour</key><integer>10</integer><key>Minute</key><integer>5</integer></dict>
    <dict><key>Hour</key><integer>22</integer><key>Minute</key><integer>5</integer></dict>
  </array>
  <key>StandardOutPath</key><string>$HOME/.qoder-switch/auto-claim.log</string>
  <key>StandardErrorPath</key><string>$HOME/.qoder-switch/auto-claim.log</string>
</dict>
</plist>
EOF
launchctl unload "$PLIST" 2>/dev/null || true
launchctl load "$PLIST"
echo "已安装每日自动领取：每天 10:05 和 22:05 自动运行（无需打开应用）"
echo "日志：~/.qoder-switch/auto-claim.log"
