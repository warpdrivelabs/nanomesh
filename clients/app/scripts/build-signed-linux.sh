#!/usr/bin/env bash
# nmspace Linux 签名构建（deb/rpm 更新产物 + .sig）。
#
# 用法（clients/app/ 下）：
#   ./scripts/build-signed-linux.sh                 # 交互输入密码
#   ./scripts/build-signed-linux.sh <密码>          # 脚本传入
#   TAURI_SIGNING_PRIVATE_KEY_PASSWORD=... ./scripts/build-signed-linux.sh
#
# 私钥位置（仓库外，gitignore 已挡 *.key）：~/.tauri/cmx-agent.key
#   与 cmx-agent 同一把更新签名钥（tauri.conf.json 烧录的 pubkey 即该钥对的公钥，
#   换钥必须同步换 tauri.conf.json）。可用 MINISIGN_KEY=<路径> 覆盖。
# 产物：$target_dir/release/bundle/deb/*.deb（rpm 同目录旁 bundle/rpm/）+ 同名 .sig
#       （更新产物 + .sig 一对，一起登记发布）。$target_dir 见下方 CARGO_TARGET_DIR 说明。
#
# 与 cmx-agent 版的差异：
#   - 去掉 cargo clean：本项目 UI（../ui 与 cmx-agent-ui 资源）都是打包期拷贝，
#     不编译进二进制，增量编译不影响 UI 新鲜度。
#   - 打包前自动同步 cmx-agent UI 到 src-tauri/cmx-agent-ui（bundle.resources
#     构建期取该目录；入库时只是 README 占位，不同步打出的包运行期加载不到 UI）。
#   - 不走 npx/node：node_modules 是 Windows 侧 npm 装的（.bin shim 找 node.exe、
#     @tauri-apps/cli 只带 win32 二进制），WSL 共用必挂。Linux 侧用 cargo install
#     的 tauri-cli（版本与 package-lock.json 的 @tauri-apps/cli 对齐，npm 与
#     crates.io 同号发布），node 在本构建链里本就只承担跑 CLI 包装这一件事。
#
# Linux 侧一次性准备：
#   cargo install tauri-cli --locked --version <package-lock 里的 @tauri-apps/cli 版本>
#   protoc（nm-proto 的 prost-build 构建期要；~/.local/protoc*/bin/protoc 会被自动识别）

set -euo pipefail

app_dir="$(cd "$(dirname "$0")/.." && pwd)"
src_tauri="$app_dir/src-tauri"
key_path="${MINISIGN_KEY:-$HOME/.tauri/cmx-agent.key}"

# ---- 工具链预检（放密码输入前，缺东西别浪费一次输密码）----

# 非登录 shell（CI / wsl.exe 直调）PATH 不含 rustup 的 ~/.cargo/bin，兜底加进来
[[ -x "$HOME/.cargo/bin/cargo" ]] && export PATH="$HOME/.cargo/bin:$PATH"

# tauri CLI：默认 cargo tauri（TAURI_CLI 可整体覆盖，如 TAURI_CLI="npx tauri"）。
# 版本必须与 package-lock 固定的 @tauri-apps/cli 同号（npm 包与 crates.io 的 tauri-cli 同 tag 发布）。
lock_cli_ver="$(grep -A3 -F '"node_modules/@tauri-apps/cli": {' "$app_dir/package-lock.json" \
    | sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' | head -1)"
if [[ -z "${TAURI_CLI:-}" ]]; then
    if ! command -v cargo-tauri >/dev/null 2>&1; then
        echo "缺 tauri-cli（Linux 构建不走 npx：node_modules 是 Windows 侧装的）——先装：" >&2
        echo "  cargo install tauri-cli --locked --version ${lock_cli_ver:-<查 package-lock 的 @tauri-apps/cli 版本>}" >&2
        exit 1
    fi
    got_cli_ver="$(cargo tauri --version | awk '{print $NF}')"
    if [[ -n "$lock_cli_ver" && "$got_cli_ver" != "$lock_cli_ver" ]]; then
        echo "tauri-cli 版本不一致：已装 $got_cli_ver，package-lock 固定 $lock_cli_ver，先对齐：" >&2
        echo "  cargo install tauri-cli --locked --version $lock_cli_ver" >&2
        exit 1
    fi
    TAURI_CLI="cargo tauri"
fi
read -r -a tauri_cmd <<<"$TAURI_CLI"

# gitee 的 smart HTTP 响应 Content-Type 不标准（application/octet-stream），
# cargo 内置 git 客户端严格校验会拒收（spurious network error ×3 后失败）；
# 改走系统 git 命令行拉取即可（git 不校验该项，gitee 国内直连通，无需代理）。
export CARGO_NET_GIT_FETCH_WITH_CLI=true

# 客户端子树 .cargo/config.toml 把 OPENSSL_NO_VENDOR 压回 "0"（Windows/macOS 没有可用的
# 系统 OpenSSL，必须 vendor）。进程环境优先于非 force 的 [env]，这里显式保持 Linux 打包
# 走系统 openssl —— 与改动前行为一致，省去源码编译 OpenSSL，也不依赖本机 perl 模块齐全。
export OPENSSL_NO_VENDOR=1

# nm-proto 走 prost-build，构建期要 protoc（Windows 侧靠用户级 PROTOC 环境变量，WSL 不继承）。
find_protoc() {
    if [[ -n "${PROTOC:-}" ]] && command -v "$PROTOC" >/dev/null 2>&1; then printf '%s' "$PROTOC"; return 0; fi
    command -v protoc 2>/dev/null && return 0
    local p
    for p in "$HOME"/.local/protoc*/bin/protoc "$HOME"/packages/protoc*/bin/protoc; do
        [[ -x "$p" ]] && { printf '%s' "$p"; return 0; }
    done
    return 1
}
PROTOC="$(find_protoc || true)"
if [[ -z "$PROTOC" ]]; then
    echo "缺 protoc（nm-proto 的 prost-build 构建期需要）——WSL 用户级安装（无需 sudo）：" >&2
    echo "  mkdir -p ~/.local/protoc-36.2" >&2
    echo "  curl -fsSL https://github.com/protocolbuffers/protobuf/releases/download/v36.2/protoc-36.2-linux-x86_64.zip -o /tmp/protoc.zip" >&2
    echo "  python3 -m zipfile -e /tmp/protoc.zip ~/.local/protoc-36.2/   # 脚本自动识别 ~/.local/protoc*/bin/protoc" >&2
    echo "  chmod +x ~/.local/protoc-36.2/bin/protoc   # python3 zipfile 解压不带可执行位" >&2
    exit 1
fi
export PROTOC

# WSL /mnt 盘（DrvFs）上 cargo 增删文件极慢，且避免与 Windows 构建混一个 target 目录：
# 仓库在 /mnt 下时 Linux 产物默认重定向到 WSL 本地盘。NANOMESH_KEEP_TARGET_DIR=1 留回仓库内，
# 或直接设 CARGO_TARGET_DIR 自定位置。
if [[ -z "${CARGO_TARGET_DIR:-}" && -z "${NANOMESH_KEEP_TARGET_DIR:-}" && "$app_dir" == /mnt/* ]]; then
    export CARGO_TARGET_DIR="$HOME/.cache/nanomesh/target-linux"
    echo "仓库在 /mnt 盘：CARGO_TARGET_DIR 重定向到 $CARGO_TARGET_DIR（NANOMESH_KEEP_TARGET_DIR=1 可关闭）"
fi
target_dir="${CARGO_TARGET_DIR:-$src_tauri/target}"

if [[ ! -f "$key_path" ]]; then
    echo "找不到更新签名私钥：$key_path —— 把 cmx-agent 的同一把私钥放到该位置（或 MINISIGN_KEY=<路径> 指向别处）后重试" >&2
    exit 1
fi

key_password="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
if [[ -z "$key_password" ]]; then
    if [[ $# -ge 1 ]]; then
        key_password="$1"
    else
        read -rsp "输入私钥密码: " key_password
        echo
    fi
fi

# 打包前同步 cmx-agent UI（选树逻辑与 cmx_host.rs 的运行期兜底一致：
# 须含 index.html + embed/truemate-app.js）。bash 函数无提升，先定义后调用。
find_cmx_ui() {
    local roots=() root d m best="" best_m=0
    [[ -n "${CARGO_HOME:-}" ]] && roots+=("$CARGO_HOME")
    roots+=("$HOME/.cargo")
    if grep -qi microsoft /proc/version 2>/dev/null; then
        # WSL：加扫 Windows 侧 cargo home（本机 CARGO_HOME=D:\Packages\cargo）
        roots+=("/mnt/c/Users/"*"/.cargo" "/mnt/d/Packages/cargo")
    fi
    for root in "${roots[@]}"; do
        for d in "$root"/git/checkouts/cmx-agent-*/*/; do
            [[ -d "$d" ]] || continue
            if [[ -f "$d/crates/cmx-agent-web/ui/index.html" \
                && -f "$d/crates/cmx-agent-web/ui/embed/truemate-app.js" ]]; then
                m="$(stat -c %Y "$d" 2>/dev/null || echo 0)"
                if (( m >= best_m )); then best="$d"; best_m=$m; fi
            fi
        done
    done
    [[ -n "$best" ]] || return 1
    printf '%s' "${best}crates/cmx-agent-web/ui"
}

sync_cmx_ui() {
    local src="${CMX_AGENT_UI_DIR:-}"
    if [[ -z "$src" ]]; then
        src="$(find_cmx_ui || true)"
        if [[ -z "$src" ]]; then
            # 首建机器/全新 WSL 还没有 checkout：先 fetch 把 cmx-agent git 依赖拉下来再找
            #（sync 先于 build，否则首次构建必失败在 sync）。
            echo "未发现 cmx-agent checkout，先 cargo fetch 拉取 git 依赖…"
            (cd "$src_tauri" && cargo fetch)
            src="$(find_cmx_ui || true)"
        fi
    fi
    [[ -n "$src" && -f "$src/index.html" && -f "$src/embed/truemate-app.js" ]] || {
        echo "找不到合格的 cmx-agent UI（$src 缺 index.html 或 embed/truemate-app.js）" >&2
        echo "已扫描 CARGO_HOME 与 ~/.cargo 的 git/checkouts（WSL 下另扫 /mnt 侧 Windows cargo home）" >&2
        echo "可设 CMX_AGENT_UI_DIR=<cmx-agent checkout>/crates/cmx-agent-web/ui 后重跑" >&2
        exit 1
    }
    # git 只跟踪本目录的 README 占位（.gitignore 放行它），rm -rf 会连它一起删掉；
    # 先备份再补回，否则每打一次包工作区就多一条「删除 README」的待提交改动。
    local readme="$src_tauri/cmx-agent-ui/README.md" readme_bak=""
    if [[ -f "$readme" ]]; then readme_bak="$(mktemp)"; cp "$readme" "$readme_bak"; fi
    rm -rf "$src_tauri/cmx-agent-ui"
    mkdir -p "$src_tauri/cmx-agent-ui"
    cp -a "$src/." "$src_tauri/cmx-agent-ui/"
    if [[ -n "$readme_bak" ]]; then cp "$readme_bak" "$readme"; rm -f "$readme_bak"; fi
    echo "cmx-agent UI 已同步: $src"
}
sync_cmx_ui

cd "$app_dir" # tauri CLI 从 clients/app 定位 src-tauri/tauri.conf.json

# 内容型 env 全版本通吃（CLI 只认内容型）；PATH 型一并设置兼容新版。
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$key_path")"
export TAURI_SIGNING_PRIVATE_KEY_PATH="$key_path"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$key_password"

"${tauri_cmd[@]}" build

# 防漏签自检：更新产物旁必须产出同名 .sig（缺了说明签名步没跑，勿分发）。
sigs=$(find "$target_dir/release/bundle/deb" "$target_dir/release/bundle/rpm" \
    -maxdepth 1 -name "*.sig" 2>/dev/null || true)
if [[ -z "$sigs" ]]; then
    echo "未找到 .sig —— 更新产物没有签名，禁止分发；检查私钥/密码后重跑" >&2
    exit 1
fi

echo
echo "构建完成（deb/rpm + .sig 成对，一起登记发布）："
echo "$sigs" | sed 's/^/  /'
find "$target_dir/release/bundle/deb" "$target_dir/release/bundle/rpm" \
    -maxdepth 1 \( -name "*.deb" -o -name "*.rpm" \) 2>/dev/null | sed 's/^/  /'
