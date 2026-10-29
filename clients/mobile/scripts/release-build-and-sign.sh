#!/usr/bin/env bash
#
# release-build-and-sign.sh —— nmspace 移动壳（桌面预览形态）的
# 「编译 → 签名 → 公证 → 装订 → 打 dmg + zip」一条龙。
# 参照 cmx-agent 同名脚本；移动壳无 updater / vendor dylib，流程更短。
#
# 产物：
#   $TARGET_DIR/release/bundle/macos/nmspace.app               （Developer ID 签名 + 公证 + staple）
#   $TARGET_DIR/release/bundle/macos/nmspace-<ver>-aarch64.zip （stapled .app 的分发 zip）
#   $TARGET_DIR/release/bundle/dmg/nmspace_<ver>_aarch64.dmg   （app + Applications 软链接，首装分发）
#
# 前置（一次性，本机已就绪；换机器需重做）：
#   1) Developer ID Application 证书 + 私钥已导入登录钥匙串
#   2) 公证凭据 keychain profile 可用（复用同团队已配好的 cmx-agent-notary）：
#      xcrun notarytool history --keychain-profile cmx-agent-notary   # 不报错即可
#      （首次设置：xcrun notarytool store-credentials "cmx-agent-notary"
#        --apple-id <AppleID> --team-id W8H2ZU6LLY --password <专用密码>）
#   3) 前端依赖已装：clients/mobile 下 npm install（tauri CLI 走 npx）
#
# 用法：
#   ./release-build-and-sign.sh                # 全流程
#   ./release-build-and-sign.sh --no-build     # 跳过编译，对已有 .app 签名+公证+打包
#   ./release-build-and-sign.sh --no-notarize  # 只签名+打包，不公证（仅自用调试，别人机器会被 Gatekeeper 拦）
#
set -euo pipefail

# ── 配置 ─────────────────────────────────────────────────────────
IDENTITY="Developer ID Application: Pansoft Company Limited (W8H2ZU6LLY)"
TEAM_ID="W8H2ZU6LLY"
NOTARY_PROFILE="${NOTARY_PROFILE:-cmx-agent-notary}" # 复用 cmx-agent 的公证凭据（同团队）
APP_NAME="nmspace"
ARCH="aarch64" # 移动壳目前只出 Apple 芯片包；Intel 交叉编译卡 x86_64 OpenSSL，未解锁

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
MOBILE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# ── 参数 ─────────────────────────────────────────────────────────
DO_BUILD=1
DO_NOTARIZE=1
for arg in "$@"; do
  case "$arg" in
    --no-build)    DO_BUILD=0 ;;
    --no-notarize) DO_NOTARIZE=0 ;;
    *) echo "未知参数: $arg（支持 --no-build / --no-notarize）" >&2; exit 2 ;;
  esac
done

log() { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# ── 1. 前置检查 ──────────────────────────────────────────────────
log "前置检查"
security find-identity -v -p codesigning | grep -q "$IDENTITY" \
  || die "钥匙串中找不到签名身份『$IDENTITY』"
echo "签名身份: OK"
if [ "$DO_NOTARIZE" -eq 1 ]; then
  # notarytool profile 存在 iCloud 钥匙串，security find-generic-password 查不到，
  # 用 notarytool history 实际验证（cmx-agent 踩坑记录）。
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1 \
    || die "公证 profile『$NOTARY_PROFILE』不可用（notarytool store-credentials）"
  echo "公证 profile: OK"
fi

# ── 2. 编译（bundler 阶段即用 Developer ID 签名） ─────────────────
if [ "$DO_BUILD" -eq 1 ]; then
  log "tauri build（release，约 2 分钟）"
  ( cd "$MOBILE_DIR" && npx tauri build --bundles app \
      -c "{\"bundle\":{\"macOS\":{\"signingIdentity\":\"$IDENTITY\"}}}" )
fi

# target 目录跟 cargo 全局配置走（本机 = ~/.cargo-shared-target，见 ~/.cargo/config.toml）。
TDIR="$( cd "$MOBILE_DIR/src-tauri" \
  && cargo metadata --no-deps --format-version 1 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1 )"
[ -n "$TDIR" ] || TDIR="$MOBILE_DIR/src-tauri/target"
BUNDLE_DIR="$TDIR/release/bundle/macos"
APP="$BUNDLE_DIR/$APP_NAME.app"
[ -d "$APP" ] || die "未找到产物: $APP"
VERSION="$( sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$MOBILE_DIR/src-tauri/tauri.conf.json" | head -1 )"
ZIP="$BUNDLE_DIR/$APP_NAME-$VERSION-$ARCH-macos.zip"
DMG_DIR="$TDIR/release/bundle/dmg"
DMG="$DMG_DIR/${APP_NAME}_${VERSION}_${ARCH}.dmg"

# ── 3. 签名（deep + hardened runtime + timestamp，兜底重签） ──────
log "codesign（deep + hardened runtime + timestamp）"
codesign --force --deep --options runtime --timestamp --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP" || die "codesign 验证失败"

# ── 4. 公证 + 装订 ───────────────────────────────────────────────
if [ "$DO_NOTARIZE" -eq 1 ]; then
  log "打包 zip 并提交 Apple 公证（--wait，约几分钟）"
  WORK="$(mktemp -d)"
  trap 'rm -rf "$WORK"' EXIT
  NOTARY_ZIP="$WORK/$APP_NAME-notary.zip"
  ditto -c -k --keepParent "$APP" "$NOTARY_ZIP"
  xcrun notarytool submit "$NOTARY_ZIP" --keychain-profile "$NOTARY_PROFILE" --wait

  log "stapler staple（装订公证票据，离线可验）"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"

  log "spctl --assess（Gatekeeper 终检）"
  spctl --assess --verbose=4 "$APP" > /tmp/nmspace-spctl.out 2>&1 || true
  grep -q "accepted" /tmp/nmspace-spctl.out \
    || die "Gatekeeper 未 accepted（见 /tmp/nmspace-spctl.out）"
  echo "Gatekeeper: accepted"
fi

# ── 5. 分发产物（用走完全流程的最终 .app 打包） ────────────────────
log "打分发 zip + dmg（含 Applications 软链接）"
rm -f "$ZIP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
STAGING="$(mktemp -d)"
trap 'rm -rf "$WORK" "$STAGING"' EXIT
cp -Rp "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
mkdir -p "$DMG_DIR"
rm -f "$DMG"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGING" -fs HFS+ -format UDBZ "$DMG" 2>/dev/null

# ── 6. 完成 ──────────────────────────────────────────────────────
log "完成 ✅"
echo "  app: $APP（$(du -sh "$APP" | awk '{print $1}')）"
echo "  zip: $ZIP（$(du -sh "$ZIP" | awk '{print $1}')）"
echo "  dmg: $DMG（$(du -sh "$DMG" | awk '{print $1}')）"
[ "$DO_NOTARIZE" -eq 0 ] && echo "  ⚠ 未公证：仅本机自用，别的机器会被 Gatekeeper 拦"
