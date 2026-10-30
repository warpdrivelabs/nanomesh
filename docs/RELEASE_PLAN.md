# 发版方案（Gitee Release · Windows 首批）

- 状态：待评审
- 当前批次：**仅 Windows x64，本地构建验证**；macOS/Linux 后续批次
- 目标版本：`v0.1.1`（首个带应用内更新的正式版本）
- 关联文档：`APP_UPDATER_DESIGN.md`

## 1. 目标与范围

当前批次需要完成：

1. Windows x64 安装包构建（NSIS 主安装/更新，MSI 备用）。
2. Tauri updater 签名产物与 `latest.json` 生成。
3. Gitee Release 创建、Windows 资产上传、直链校验。
4. `nanomesh/frontend/index.html` 的 Windows 下载链接更新。
5. 版本、tag、manifest、页面四处一致。

macOS/Linux 构建、GitHub Actions/Gitee Go、双架构/多包形态放到后续批次。

本次不包含：自动增量更新、beta 通道、Web 端发布、macOS/Linux 发布。

## 2. 版本策略

- 语义化版本：`MAJOR.MINOR.PATCH`；本次 `0.1.0 → 0.1.1`。
- Git tag：`v0.1.1`，与 Release tag、`latest.json.version`、安装包版本一致。
- 版本号落点（必须全部同步）：
  - 根 `Cargo.toml` → `[workspace.package] version`
  - `clients/app/package.json` → `version`
  - `clients/app/src-tauri/Cargo.toml` → `version`
  - `clients/app/src-tauri/tauri.conf.json` → `version`
- 预发布：Gitee Release 编辑页勾选“预览版本”；stable manifest 只指向正式版。

## 3. 产物矩阵

文件名以 Tauri 实际输出为准，下面为命名基线：

> 当前批次只构建/上传 Windows 两行；macOS/Linux 行是后续批次目标，不在本期验收范围。

| 平台 | 产物 | 用途 | 自动更新 |
|---|---|---|---|
| Windows x64 | `nmspace_0.1.1_x64-setup.exe`（NSIS） | 首选安装 + 更新 | ✅ |
| Windows x64 | `nmspace_0.1.1_x64_en-US.msi` | 离线/企业安装 | ❌（不承诺） |
| macOS Apple Silicon | `nmspace_0.1.1_aarch64.dmg` | 首次安装 | — |
| macOS Apple Silicon | `nmspace_0.1.1_aarch64.app.tar.gz` + `.sig` | 应用内更新 | ✅ |
| macOS Intel | `nmspace_0.1.1_x64.dmg` | 首次安装 | — |
| macOS Intel | `nmspace_0.1.1_x64.app.tar.gz` + `.sig` | 应用内更新 | ✅ |
| Linux x64 | `nmspace_0.1.1_amd64.deb` + `.sig` | Debian/Ubuntu 安装/更新 | ✅（pkexec） |
| Linux x64 | `nmspace_0.1.1_x86_64.rpm` + `.sig` | Fedora/RHEL 安装/更新 | ✅（pkexec） |
| Linux x64 | `nmspace_0.1.1_amd64.AppImage` | 便携运行 | — |
| Linux x64 | `nmspace_0.1.1_amd64.AppImage.tar.gz` + `.sig` | 应用内更新 | ✅（免 root） |
| 后端节点（可选） | `nanomesh-0.1.1-<平台>.zip` | CLI/节点分发 | 不适用 |
| 校验 | `SHA256SUMS` | 所有资产 | — |

说明：

- Windows 更新通道固定使用 **NSIS**；`installMode=perMachine`，默认安装到 `C:\Program Files`，安装/更新需要 UAC。MSI 仅作为手动安装选项，避免两类安装器混用。
- macOS 更新使用 `.app.tar.gz`，DMG 不参与自动更新。
- Linux deb/rpm/AppImage 均可应用内更新：deb/rpm 通过 pkexec 调系统包管理器（需要 polkit/桌面会话，安装时弹管理员授权）；AppImage 免 root 直接替换。
- Tauri bundler 2.10+ 支持把 deb/rpm 作为 updater artifact；本项目 Tauri 2.12.5 满足。
- 后端 zip 与桌面端安装包是两套产物，是否发布由本次范围决定。

## 4. 构建环境

### Windows

- Windows 10/11 x64，Rust 1.97.1，MSVC，Node 22，WebView2。
- 运行环境需要 FireDaemon OpenSSL `bin` 在 PATH；构建需要 `OPENSSL_DIR`。
- 命令：
  ```powershell
  cd clients/app
  npm install
  $env:TAURI_SIGNING_PRIVATE_KEY = "$env:USERPROFILE\.tauri\nanomesh-updater.key"
  $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = "<secret>"
  npm run tauri build
  ```
- 产物目录：`src-tauri/target/release/bundle/msi`、`bundle/nsis`。

### macOS

- macOS + Xcode Command Line Tools。
- 两个架构分别构建：
  ```bash
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  npm run tauri build -- --target aarch64-apple-darwin
  npm run tauri build -- --target x86_64-apple-darwin
  ```
- 如需 universal，可另建 universal 包，但本期 updater manifest 按两个架构分别发布，避免 universal 体积/签名复杂度。
- 正式分发必须 Developer ID 签名；建议公证。未签名包只能作为内部测试，不进 stable manifest。

### Linux

- Ubuntu 22.04/24.04（建议固定 22.04 作为 glibc 基线）或 Docker 容器。
- 依赖：
  ```bash
  sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
    libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf rpm
  ```
- 命令：
  ```bash
  cd clients/app
  npm install
  npm run tauri build
  ```
- 产物：`bundle/deb`、`bundle/rpm`、`bundle/appimage`；deb/rpm 的 `.sig` 也需收集。
- AppImage 构建不需要 FUSE；运行 AppImage 的用户环境才需要 FUSE。deb/rpm 自动更新依赖系统 `pkexec`/polkit 与包管理器。

## 5. 构建方式与 CI

**当前批次：本地 Windows 构建，不接入 CI。** 先验证 updater 链路和 Gitee Release 上传；稳定后再决定 CI。

后续批次可选：

### 后续首选：GitHub Actions 矩阵

原因：需要 macOS runner；公开仓库免费，私有仓库可用但 macOS 分钟数倍率较高。

- 将 Gitee 仓库镜像到 GitHub（私有仓库也可，注意分钟成本）。
- 触发：`push` tag `v*` 或手动 `workflow_dispatch`。
- 矩阵：
  - `windows-latest`
  - `macos-14`（Apple Silicon）
  - `macos-13`（Intel）
  - `ubuntu-22.04`
- 每个 job：
  1. checkout tag
  2. 安装 Node 22 / Rust 1.97.1 / 平台依赖
  3. 注入 updater 私钥与密码（Secrets）
  4. `npm ci && npm run tauri build`
  5. 收集安装包、`.sig`、SHA256
  6. 上传 workflow artifact
- Release job：
  1. 汇总产物
  2. 生成 `latest.json`
  3. 上传 Gitee Release 资产
  4. 发布 `latest.json` 到主端点
  5. 更新 `frontend/index.html` 链接（可由脚本或人工提交）
- 必要 Secrets：
  - `TAURI_SIGNING_PRIVATE_KEY`
  - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
  - `GITEE_TOKEN`
  - macOS：Developer ID 证书、公证 Apple ID / Team ID / App-specific password（或 API Key）
  - Windows 代码签名证书（可选）

### 后续备选：Gitee Go + 自建 Mac

- Gitee Go 承担 Linux/Windows 构建；macOS 接自建 Mac runner 或单独用 GitHub Actions。
- 优点：代码不离开 Gitee。
- 缺点：两套 CI、维护成本高；Gitee Go 的 macOS 托管 runner 不是标准能力。

## 6. 签名与安全

1. **Updater 签名密钥**
   - 内部测试阶段按用户决定与 TrueMate 共用同一 keypair；仅限内部，不进入公开 stable。
   - 公开版前生成 nanomesh 独立 keypair；私钥/密码只进本机安全目录和 CI Secret。仓库、Release、日志、文档不得出现私钥或密码。
2. **macOS 代码签名**
   - Developer ID Application 证书；`codesign` + `notarytool`；Tauri 构建时注入。
   - 未签名/未公证的 dmg/app.tar.gz 不进入 stable 更新。
3. **Windows 代码签名（可选）**
   - 有证书则签名，减少 SmartScreen 拦截；没有则提供 SHA256，并在 Release 说明中注明。
4. **Linux**
   - 无统一代码签名；发布 `SHA256SUMS`，页面/Release 提供校验值。
5. **发布顺序**
   - 先上传全部安装包 → 校验 URL 200 → 再发布 manifest。禁止反序。

## 7. 发版执行步骤

### 7.1 准备

- [ ] 确认本次版本号（`0.1.1`）与变更范围
- [ ] 更新四处版本号
- [ ] 运行 `cargo check --workspace`、`npm run tauri build` 干跑
- [ ] 确认 updater 签名 key 口径：本期内部共用 TrueMate key
- [ ] 确认 `latest.json` 已提交到 Gitee `updates/latest.json`
- [ ] 确认 Windows 签名/打包环境可用（本期不需要 macOS 签名）

### 7.2 构建

- [ ] Windows NSIS + MSI + updater `.sig`
- [ ] （后续批次）macOS aarch64 / x86_64 DMG + `.app.tar.gz` + `.sig`
- [ ] （后续批次）Linux deb/rpm/AppImage 安装包 + 各自 updater `.sig`
- [ ] 后端 zip（如包含）
- [ ] 生成 `SHA256SUMS`
- [ ] 在干净 Windows 虚拟机安装验证

### 7.3 打 tag

```bash
git add <版本文件>
git commit -m "chore(release): v0.1.1"
git tag -a v0.1.1 -m "NANOMESH v0.1.1"
git push origin main
git push origin v0.1.1
```

### 7.4 Gitee Release

- [ ] 仓库 → 发行版 → 创建发行版
- [ ] tag 选 `v0.1.1`，标题 `NANOMESH v0.1.1`
- [ ] 填写更新说明（见 §10 模板）
- [ ] 上传全部安装包、`.sig`、`SHA256SUMS`
- [ ] 逐个点击/`curl -I` 校验直链 200
- [ ] 本期若为预发布，勾选“预览版本”

注意 Gitee 限制：单附件 ≤100MB（GVP 200MB），仓库附件总量 ≤1GB（推荐项目 5GB、GVP 20GB）。

### 7.5 发布 latest.json

1. 用脚本读取版本、notes、`.sig`、Gitee 资产 URL。
2. 校验字段：`version`、`platforms`、`signature` 非空、URL 均为 https。
3. 先提交 `updates/latest.json` 到 Gitee `main`；本期不使用 relay 主端点。
4. 发布后 `curl` 校验 raw 内容，并用客户端实际检查更新。
4. 主端点响应头设置为不缓存或短缓存；发布后 `curl` 校验内容。
5. 校验客户端检查更新返回 `0.1.1`。

### 7.6 更新 index.html

- [ ] 版本 pill 改为 `v0.1.1`
- [ ] Windows 主按钮 → NSIS `.exe`；次链接 → MSI
- [ ] Windows → NSIS `.exe` 主按钮 + MSI 次链接
- [ ] （后续批次）macOS → Apple Silicon DMG / Intel DMG 两个链接
- [ ] （后续批次）Linux → deb / rpm / AppImage 三个链接；三种均支持应用内更新
- [ ] 保留“查看全部版本”指向 `/releases`
- [ ] 直链使用：
  ```text
  https://gitee.com/warpdrivelabs/nanomesh/releases/download/v0.1.1/<file>
  ```
- [ ] 页面实测下载（桌面 + 手机各一次）

### 7.7 公告与归档

- [ ] Release 说明包含版本、变更、已知问题、校验方式
- [ ] 记录本次产物清单与 SHA256
- [ ] 保留上一版本 Release 资产，不删除

## 8. index.html 双包展示建议

- Windows 卡片：主按钮 `下载 .exe`，下方小字 `MSI 安装包`
- macOS 卡片：两行 `Apple 芯片（.dmg）` / `Intel（.dmg）`
- Linux 卡片：`Ubuntu/Debian（.deb）` / `Fedora/RHEL（.rpm）` / `AppImage`；三种都支持应用内更新，deb/rpm 安装时会弹管理员授权
- 版本更新时只改链接和版本 pill；可后续把 URL 收敛为 JS 常量，减少漏改。

## 9. 验证清单

### 安装

- [ ] Windows NSIS 首次安装、启动、卸载正常
- [ ] Windows MSI 首次安装（不承诺自动更新）
- [ ] （后续批次）macOS ARM/Intel DMG 首次安装；Gatekeeper 行为符合预期
- [ ] （后续批次）Ubuntu deb 安装；Fedora rpm 安装；AppImage 直接运行
- [ ] 所有资产 SHA256 与 `SHA256SUMS` 一致

### 更新

- [ ] 0.1.0 → 0.1.1 应用内更新成功并重启
- [ ] macOS 两个架构均能更新（如本期发布）
- [ ] Linux deb/rpm/AppImage 均能应用内更新；deb/rpm 弹出 pkexec 授权；无 polkit 时回退手动下载
- [ ] 篡改包/错误签名被拒绝
- [ ] 断网、弱网、磁盘不足有可理解提示
- [ ] 跳过此版本后不再提示；下一版本恢复
- [ ] manifest 缺平台条目时该平台不误报更新

### 页面与发布

- [ ] index.html 所有下载按钮 200
- [ ] Release 资产与 manifest URL 一一对应
- [ ] 版本号四处一致
- [ ] 仓库无密钥泄露；CI 日志无敏感信息

## 10. Release Notes 模板

```markdown
# NANOMESH v0.1.1

## 新增
- 应用内更新（本期：Windows；后续：macOS / Linux deb / rpm / AppImage）
- ...

## 修复
- ...

## 安装包
- Windows: nmspace_0.1.1_x64-setup.exe / .msi
- （后续批次）macOS: aarch64.dmg / x64.dmg
- （后续批次）Linux: amd64.deb / x86_64.rpm / amd64.AppImage

## 已知问题
- Linux deb/rpm 自动更新需要管理员授权（pkexec）；无 polkit 环境请手动下载
- ...

## 校验
SHA256SUMS 见附件；macOS 包已签名/公证（如适用）。
```

## 11. 回滚

- 不删除旧 Release / 旧资产。
- 发布错误的 manifest：立即把 `latest.json` 指回上一个可用版本；注意 Tauri 通常不做版本降级，已升级用户需要安装修复版本。
- 严重问题：发布 `0.2.1` 修复版，manifest 指向新包；Release 说明标注问题版本。
- macOS 签名/公证失败的包不得进入 manifest；重新构建并替换同版本资产前，先移除旧 manifest。

## 12. 风险

| 风险 | 应对 |
|---|---|
| 私钥泄露 | 已泄露 key 作废；CI Secret；禁止日志输出 |
| Gitee raw 风控/缓存 | 主端点用自有静态站；发布后校验响应体 |
| macOS 签名/公证失败 | 提前演练；失败版本不进 stable |
| Linux glibc/依赖不兼容 | 固定 Ubuntu 22.04 构建；AppImage 作为便携兜底；deb/rpm 在对应发行版实测 |
| Linux deb/rpm 更新权限/依赖 | pkexec/polkit 缺失、依赖冲突会导致安装失败 | 检测 polkit；失败回退手动下载；Ubuntu/Fedora 实测 |
| 100MB 附件限制 | 分架构发布；避免 universal；超限转外部存储 |
| 版本/链接不一致 | 发布脚本统一读取版本；四处对照检查 |
| CI 分钟成本 | macOS 仅构建必要架构；缓存 Rust/npm |
| 回滚不可降级 | 用更高 patch 修复；保留旧资产与说明 |

## 13. 验收标准

1. `v0.1.1` 安装包、`.sig`、`SHA256SUMS` 全部上传且可匿名下载。
2. `latest.json` 通过 schema 校验，版本/tag/页面一致。
3. 本期 Windows 干净环境安装验证通过；Windows 更新链路通过。macOS/Linux 后续批次单独验收。
4. 页面所有下载链接实测可用，双包入口清晰。
5. 仓库与 CI 无密钥泄露。
6. Release 说明、已知问题、回滚路径完整。

## 14. 附录：常用命令

```powershell
# Windows
cd clients/app
npm install
npm run tauri build
Get-ChildItem src-tauri/target/release/bundle -Recurse -Include *.exe,*.msi

# 后端节点包
cd ../..
cargo xtask dist
```

```bash
# macOS
npm run tauri build -- --target aarch64-apple-darwin
npm run tauri build -- --target x86_64-apple-darwin

# Linux
npm run tauri build
ls src-tauri/target/release/bundle/{deb,rpm,appimage}
```

```bash
# 校验资产
sha256sum nmspace_* > SHA256SUMS
curl -I https://gitee.com/warpdrivelabs/nanomesh/releases/download/v0.1.1/<file>
```
