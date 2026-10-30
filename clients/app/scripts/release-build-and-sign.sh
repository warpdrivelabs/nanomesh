#!/usr/bin/env bash
#
# release-build-and-sign.sh —— nmspace 桌面端（clients/app）的
# 「编译 → 嵌入 LLM 二进制 → 签名 → 公证 → 装订 → 打 dmg + zip」一条龙。
# 与 clients/mobile/scripts 同款流程，多了 nm-llm-gatewayd / nm-llm-proxy 的编译与嵌入。
#
# 产物：
#   $TARGET_DIR/release/bundle/macos/nmspace.app                    （签名+公证+staple，内嵌两个 LLM 二进制）
#   $TARGET_DIR/release/dist/nmspace-app-<ver>-aarch64-macos.zip    （分发 zip）
#   $TARGET_DIR/release/dist/nmspace-app_<ver>_aarch64.dmg          （首装分发）
#   （dist 目录：桌面与移动端 productName 同为 nmspace，bundle 目录会互相覆盖，
#     最终产物统一挪到 dist/ 并带端别前缀，避免拿混。）
#
# 前置（一次性，本机已就绪；换机器需重做）：
#   1) Developer ID Application 证书 + 私钥已导入登录钥匙串
#   2) 公证凭据 keychain profile 可用（复用 cmx-agent-notary）：
#      xcrun notarytool history --keychain-profile cmx-agent-notary   # 不报错即可
#   3) 前端依赖已装：clients/app 下 npm install
#
# 用法：
#   ./release-build-and-sign.sh                # 全流程
#   ./release-build-and-sign.sh --no-build     # 跳过编译（不重编 LLM 二进制，用 target 里现有的）
#   ./release-build-and-sign.sh --no-notarize  # 只签名+打包，不公证（仅自用调试）
#   ./release-build-and-sign.sh --no-llm       # 跳过 LLM 二进制的编译与嵌入
#
set -euo pipefail

# ── 配置 ─────────────────────────────────────────────────────────
IDENTITY="Developer ID Application: Pansoft Company Limited (W8H2ZU6LLY)"
TEAM_ID="W8H2ZU6LLY"
NOTARY_PROFILE="${NOTARY_PROFILE:-cmx-agent-notary}" # 复用 cmx-agent 的公证凭据（同团队）
APP_NAME="nmspace"
ARCH="aarch64"
LLM_BINS=(nm-llm-gatewayd nm-llm-proxy)
# 自动更新签名（minisign）：私钥放仓库外 ~/.tauri/，密码存登录钥匙串，绝不进 git。
# 这对钥与 tauri.conf.json 烧录公钥（keyid E0BF7148449E4FE4）配对（20261030 验签确认）。
MINISIGN_KEY="${MINISIGN_KEY:-$HOME/.tauri/nmspace-release.key}"
KEYCHAIN_SERVICE="${KEYCHAIN_SERVICE:-nmspace-release-key}"
# 更新源基址（latest.json 里产物 url 的前缀；服务端就绪后传入，如 https://updates.x.com/files）
FEED_FILES_URL="${FEED_FILES_URL:-}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$APP_DIR/../.." && pwd)"

# ── 参数 ─────────────────────────────────────────────────────────
DO_BUILD=1
DO_NOTARIZE=1
DO_LLM=1
for arg in "$@"; do
  case "$arg" in
    --no-build)    DO_BUILD=0 ;;
    --no-notarize) DO_NOTARIZE=0 ;;
    --no-llm)      DO_LLM=0 ;;
    *) echo "未知参数: ${arg}（支持 --no-build / --no-notarize / --no-llm）" >&2; exit 2 ;;
  esac
done

log() { printf '\n\033[1;36m▶ %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# ── 1. 前置检查 ──────────────────────────────────────────────────
log "前置检查"
security find-identity -v -p codesigning | grep -q "$IDENTITY" \
  || die "钥匙串中找不到签名身份『${IDENTITY}』"
echo "签名身份: OK"
if [ "$DO_NOTARIZE" -eq 1 ]; then
  # notarytool profile 存在 iCloud 钥匙串，security find-generic-password 查不到，
  # 用 notarytool history 实际验证（cmx-agent 踩坑记录）。
  xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1 \
    || die "公证 profile『${NOTARY_PROFILE}』不可用（notarytool store-credentials）"
  echo "公证 profile: OK"
fi

# ── 1.5 导出 updater 签名密钥（minisign） ────────────────────────
# tauri.conf.json 开了 createUpdaterArtifacts，缺 TAURI_SIGNING_PRIVATE_KEY 时
# `tauri build` 直接失败——所以前置检查就取好 env。
# 注意：`tauri build` 打 updater 产物只认 TAURI_SIGNING_PRIVATE_KEY（密钥**内容**），
# 不认 _PATH 变体（signer sign 子命令才认 -f/env _PATH），两个都导出兼容。
log "导出 updater 签名密钥（minisign）"
[ -f "$MINISIGN_KEY" ] || die "minisign 私钥不存在：${MINISIGN_KEY}"
export TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PATH TAURI_SIGNING_PRIVATE_KEY_PASSWORD
TAURI_SIGNING_PRIVATE_KEY="$(cat "$MINISIGN_KEY")"
TAURI_SIGNING_PRIVATE_KEY_PATH="$MINISIGN_KEY"
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(security find-generic-password -s "$KEYCHAIN_SERVICE" -w)" \
  || die "钥匙串取不到 ${KEYCHAIN_SERVICE}（security add-generic-password -a nmspace -s $KEYCHAIN_SERVICE -w <密码> 补录）"
echo "minisign 私钥: OK"

# ── 同步 cmx-agent UI 函数（打包进 Resources，分发机无 cargo checkout） ──
# 选树逻辑与 cmx_host.rs 的运行期兜底一致：须含 index.html + embed/truemate-app.js。
# bash 函数无提升，必须先定义后调用（20261029 踩坑：调用写在定义前直接 command not found）。
sync_cmx_ui() {
  local src="${CMX_AGENT_UI_DIR:-}"
  if [ -z "$src" ]; then
    local best="" best_m=0 d m
    for d in "$HOME"/.cargo/git/checkouts/cmx-agent-*/*/; do
      [ -f "$d/crates/cmx-agent-web/ui/index.html" ] || continue
      [ -f "$d/crates/cmx-agent-web/ui/embed/truemate-app.js" ] || continue
      m="$(stat -f %m "$d" 2>/dev/null || echo 0)"
      if [ "$m" -ge "$best_m" ]; then best="$d"; best_m="$m"; fi
    done
    src="${best}crates/cmx-agent-web/ui"
  fi
  [ -f "$src/index.html" ] && [ -f "$src/embed/truemate-app.js" ] \
    || die "找不到合格的 cmx-agent UI（$src 缺 index.html 或 embed/truemate-app.js）"
  local dst="$APP_DIR/src-tauri/cmx-agent-ui"
  rm -rf "$dst"
  mkdir -p "$dst"
  ditto "$src" "$dst"
  echo "  UI 源: $src"
}

# ── 2. 编译 ──────────────────────────────────────────────────────
if [ "$DO_BUILD" -eq 1 ]; then
  if [ "$DO_LLM" -eq 1 ]; then
    log "编译 LLM 二进制（release）"
    ( cd "$REPO_ROOT" && cargo build --release -p nm-llm-gatewayd -p nm-llm-proxy )
  fi
  # 先同步 UI 再 tauri build：bundle.resources 构建期取 src-tauri/cmx-agent-ui，
  # 顺序反了包里只会带 README 占位（20261029 踩坑）。
  log "同步 cmx-agent UI 到 Resources"
  sync_cmx_ui
  log "tauri build（release，较久；vendored OpenSSL 首次会编译 OpenSSL 源码）"
  ( cd "$APP_DIR" && npx tauri build --bundles app \
      -c "{\"bundle\":{\"macOS\":{\"signingIdentity\":\"$IDENTITY\"}}}" )
fi

if [ "$DO_BUILD" -eq 0 ]; then
  # --no-build 模式：仍确保 Resources 资产与现有 .app 一致（不重打 Resources，
  # 仅当 src-tauri/cmx-agent-ui 为空时同步，供后续手动重打包用）。
  [ -f "$APP_DIR/src-tauri/cmx-agent-ui/index.html" ] || sync_cmx_ui
fi

# target 目录跟 cargo 全局配置走（本机 = ~/.cargo-shared-target，见 ~/.cargo/config.toml）。
TDIR="$( cd "$APP_DIR/src-tauri" \
  && cargo metadata --no-deps --format-version 1 2>/dev/null \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1 )"
[ -n "$TDIR" ] || TDIR="$APP_DIR/src-tauri/target"
BUNDLE_DIR="$TDIR/release/bundle/macos"
APP="$BUNDLE_DIR/$APP_NAME.app"
[ -d "$APP" ] || die "未找到产物: $APP"
VERSION="$( sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' "$APP_DIR/src-tauri/tauri.conf.json" | head -1 )"
DIST_DIR="$TDIR/release/dist"
ZIP="$DIST_DIR/$APP_NAME-app-$VERSION-$ARCH-macos.zip"
DMG="$DIST_DIR/$APP_NAME-app_${VERSION}_$ARCH.dmg"

# ── 3. 嵌入 LLM 二进制 ───────────────────────────────────────────
if [ "$DO_LLM" -eq 1 ]; then
  log "嵌入 LLM 二进制到 Contents/MacOS/（app 按 exe 同级目录解析）"
  for b in "${LLM_BINS[@]}"; do
    SRC="$TDIR/release/$b"
    [ -f "$SRC" ] || die "缺 ${SRC}（先编译，或用 --no-llm 跳过）"
    # 分发炸弹检查：任何 Homebrew 路径的动态库都过不了别人机器。
    if otool -L "$SRC" | grep -qE "/opt/homebrew|/usr/local/opt"; then
      die "$b 链接了 Homebrew 动态库（otool -L 查看），分发必挂——改 vendored 静态链接"
    fi
    cp "$SRC" "$APP/Contents/MacOS/$b"
  done
fi

# ── 4. 签名（hardened runtime + timestamp） ──────────────────────
log "codesign（先签内嵌二进制，再 deep 签 .app）"
for b in "${LLM_BINS[@]}"; do
  [ -f "$APP/Contents/MacOS/$b" ] && \
    codesign --force --options runtime --timestamp --sign "$IDENTITY" "$APP/Contents/MacOS/$b"
done
codesign --force --deep --options runtime --timestamp --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP" || die "codesign 验证失败"

# ── 5. 公证 + 装订 ───────────────────────────────────────────────
WORK="$(mktemp -d)"
STAGING="$(mktemp -d)"
trap 'rm -rf "${WORK:-}" "${STAGING:-}"' EXIT
if [ "$DO_NOTARIZE" -eq 1 ]; then
  log "打包 zip 并提交 Apple 公证（--wait，约几分钟）"
  NOTARY_ZIP="$WORK/$APP_NAME-app-notary.zip"
  ditto -c -k --keepParent "$APP" "$NOTARY_ZIP"
  xcrun notarytool submit "$NOTARY_ZIP" --keychain-profile "$NOTARY_PROFILE" --wait

  log "stapler staple（装订公证票据，离线可验）"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"

  log "spctl --assess（Gatekeeper 终检）"
  spctl --assess --verbose=4 "$APP" > /tmp/nmspace-app-spctl.out 2>&1 || true
  grep -q "accepted" /tmp/nmspace-app-spctl.out \
    || die "Gatekeeper 未 accepted（见 /tmp/nmspace-app-spctl.out）"
  echo "Gatekeeper: accepted"
fi

# ── 5.5 updater 产物（.app.tar.gz 重打包 + minisign 签名 + latest.json 半成品） ──
# 为什么重打包：tauri build 自产的 .app.tar.gz 打包自**未经本脚本重签/公证的 .app**，
# 直接分发会让更新后的应用被 Gatekeeper 拦。正确产物 = 用走完签名→公证→staple 的最终
# .app 重新打 tar.gz（staple 票据随 .app 走，离线可验），再用 minisign 签名
# （客户端 updater 验的就是这个 .sig，公钥在 tauri.conf.json）。
log "重打包 updater 产物 nmspace.app.tar.gz + minisign 签名"
TAR_GZ="$DIST_DIR/$APP_NAME-app-$VERSION-$ARCH.app.tar.gz"
mkdir -p "$DIST_DIR"
# COPYFILE_DISABLE=1：macOS bsdtar 默认把扩展属性存成 ./_ AppleDouble 条目，
# updater 解包时对「跳过首段后路径为空」的 ._ 条目直接 unpack 失败（cmx-agent 实测踩坑）。
( cd "$BUNDLE_DIR" && rm -f "$APP_NAME.app.tar.gz" "$APP_NAME.app.tar.gz.sig" \
    && COPYFILE_DISABLE=1 tar -czf "$TAR_GZ" "$APP_NAME.app" )
# env -u：上面为 tauri build 导出了内容型 TAURI_SIGNING_PRIVATE_KEY（映射 -k），
# 与这里的 -f 路径型互斥（clap 冲突报错），sign 前去掉，用 -f + 密码 env。
( cd "$APP_DIR" && env -u TAURI_SIGNING_PRIVATE_KEY npx tauri signer sign --app-version "$VERSION" -f "$MINISIGN_KEY" "$TAR_GZ" )
[ -f "$TAR_GZ.sig" ] || die "minisign 签名未产出: $TAR_GZ.sig"
# latest.json 半成品：signature 已填，url 按 FEED_FILES_URL 拼（未传则留占位），
# 供服务端登记时直接取用（服务端也可自行渲染 latest，此处仅为对接便利）。
LATEST_JSON="$DIST_DIR/latest-$VERSION-darwin-aarch64.json"
SIG_B64="$(tr -d '\n' < "$TAR_GZ.sig")"
python3 - "$LATEST_JSON" "$VERSION" "$SIG_B64" "$FEED_FILES_URL" "$(basename "$TAR_GZ")" <<'PYEOF'
import json, sys, datetime
path, version, sig, base, fname = sys.argv[1:6]
url = (base.rstrip("/") + "/" + fname) if base else ("REPLACE_WITH_FEED_URL/" + fname)
json.dump({
    "version": version,
    "notes": "",
    "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": {"darwin-aarch64": {"signature": sig, "url": url}},
}, open(path, "w"), ensure_ascii=False, indent=2)
PYEOF
echo "updater 产物: OK（stapled .app 重打包 + minisign）"

# ── 6. 分发产物 ──────────────────────────────────────────────────
log "打分发 zip + dmg（含 Applications 软链接）"
mkdir -p "$DIST_DIR"
rm -f "$ZIP" "$DMG"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$ZIP"
cp -Rp "$APP" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGING" -fs HFS+ -format UDBZ "$DMG" 2>/dev/null

# ── 7. 完成 ──────────────────────────────────────────────────────
log "完成 ✅"
# 注意：macOS 自带 bash 3.2 会把全角括号(（）)吞进 $VAR 的变量名里报 unbound，
# 变量后必须用 ASCII 括号/空格隔开（20261029 踩坑）。
echo "  app: $APP ($(du -sh "$APP" | awk '{print $1}'))"
echo "  zip: $ZIP ($(du -sh "$ZIP" | awk '{print $1}'))"
echo "  dmg: $DMG ($(du -sh "$DMG" | awk '{print $1}'))"
if [ "$DO_LLM" -eq 1 ]; then
  echo "  内嵌 LLM 二进制: ${LLM_BINS[*]}"
fi
echo "  updater: $TAR_GZ"
echo "           + .sig；latest.json 半成品: ${LATEST_JSON}（signature 已填，url 待服务端登记）"
if [ "$DO_NOTARIZE" -eq 0 ]; then
  echo "  ⚠ 未公证：仅本机自用，别的机器会被 Gatekeeper 拦"
fi
