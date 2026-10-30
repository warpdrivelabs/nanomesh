# 应用内更新方案（Tauri Updater）

- 状态：待评审
- 适用：`clients/app`（nmspace 桌面端，Tauri 2）
- 目标版本：`v0.1.1`（首个带应用内更新的版本）
- 关联文档：`RELEASE_PLAN.md`

## 1. 背景与目标

nmspace 目前只能手动下载安装包覆盖安装。目标：

1. 客户端内可检查新版本、查看更新说明、下载并安装、自动重启。
2. 更新包必须签名，客户端只接受由本项目私钥签发的更新。
3. Windows / macOS（Apple Silicon + Intel）/ Linux（deb、rpm、AppImage）支持应用内更新；deb/rpm 安装时通过 pkexec 请求管理员授权。
4. 更新检查不依赖后端服务，读取静态 manifest；Gitee Release 只承载安装包。

不引入账号体系、不引入服务端数据库。

## 2. 现状盘点

- 桌面端：`clients/app`，Tauri `2.12.5` / `tauri-cli 2.12.4`；`src-tauri` 独立 crate。
- Tauri bundler `2.10+` 已把 **deb/rpm 识别为自包含更新产物**；updater 插件可通过 `pkexec` + 系统包管理器完成安装（TrueMate 的 `update_common.rs` 即按 `pkexec dpkg -i` 实现）。
- UI：`clients/app/ui` 为静态文件，`withGlobalTauri: true`；现有代码通过 `window.__TAURI__.core.invoke` 调用 Rust 命令，没有前端打包器。
- 版本：`0.1.0`，位于根 `Cargo.toml`、`clients/app/package.json`、`clients/app/src-tauri/Cargo.toml`、`clients/app/src-tauri/tauri.conf.json`。
- `tauri.conf.json`：`bundle.targets: "all"`；尚无 updater 插件、无 `createUpdaterArtifacts`、无更新公钥。
- 现有发布：Gitee Release 附件（见 `RELEASE_PLAN.md`）；尚无 `latest.json`。
- 本期范围：**只先打通 Windows 本地构建和验证**；macOS/Linux 等 Windows 验证通过后再做。
- 当前实现进度（2026-10-29）：已接入 `tauri-plugin-updater`、Rust 命令 `app_check_update` / `app_download_install`、设置 → 关于的“应用更新”入口；`cargo check` 通过，dev app 可启动。上游 +3 提交后已按 `installMode=perMachine` 重建：0.1.0 测试基线 `61c8c814…`、0.1.1 最终包 `0405ac35…`（`bundle/nsis` + `.sig`）；干净重建未再出现 `__TAURI_BUNDLE_TYPE` 警告。`updates/latest.json` 尚未发布，当前检查更新会因 404 失败。
- 运行环境：Windows 需要 FireDaemon OpenSSL `bin` 在 PATH；HTTPS 需要 `SSL_CERT_FILE`（Git CA bundle）。更新器自身的 HTTPS 依赖也受同类环境约束，需在打包/文档中覆盖。

> 安全事件：此前生成的一对 Tauri minisign 密钥，私钥与密码已在对话中出现。按用户 2026-10-29 的决定，**内部测试阶段先与 TrueMate 共用同一 keypair**；仅限内部/测试，不进入公开 stable。公开版前必须轮换并拆成 nanomesh 独立密钥。仓库、本文档、CI 日志均不得出现私钥或密码。

## 3. 总体设计

### 3.1 更新链路

```text
开发机/CI                         静态端点                        客户端
  |  生成签名密钥（一次性）           |                              |
  |  tauri build                     |                              |
  |  → 安装包 + .sig                 |                              |
  |  → latest.json                   |                              |
  |  → 上传 Release 附件              |                              |
  |  → 发布 latest.json（最后）  ---> |  HTTPS GET latest.json  <---  |
  |                                  |  比较版本                     |
  |                                  |  下载更新包                    |
  |                                  |  校验 .sig / 公钥              |
  |                                  |  安装 → 重启                  |
```

### 3.2 密钥管理

- 使用 Tauri 官方 minisign 签名密钥，**不是** Windows/macOS 代码签名证书。
- 生成：
  ```bash
  npm run tauri signer generate -- -w "$HOME/.tauri/nanomesh-updater.key"
  ```
  Windows PowerShell：
  ```powershell
  npm run tauri signer generate -- -w "$env:USERPROFILE\.tauri\nanomesh-updater.key"
  ```
- 本期内部测试：按用户决定与 TrueMate **共用同一 keypair**；`plugins.updater.pubkey` 使用 TrueMate 配置中的公钥。只限内部，不得用于公开 stable。
- 私钥文件只存在本机安全目录和 CI Secret；密码只存在密码管理器 / CI Secret。
- 公钥写入 `tauri.conf.json` 的 `plugins.updater.pubkey`；可以公开。
- 构建环境变量：
  - `TAURI_SIGNING_PRIVATE_KEY`：私钥文件路径或内容（CI 中放 Secret）
  - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`：私钥密码
- 轮换策略：公开版前生成 nanomesh **独立** keypair；已有客户端后更换公钥必须通过一次由旧 key 签名的迁移版本完成，必须单独评审。TrueMate 侧按已有客户端迁移策略处理。

### 3.3 Manifest 托管

`latest.json` 必须放在**固定 URL**，不能放在带版本号的 Release 附件路径里。

- 主端点：`https://cmx.pansoft.com/nanomesh-updates/latest.json?target={{target}}&current_version={{current_version}}`
  - 由 CMX 门户动态渲染（`nanomesh_updates` 独立模块，表 `nanomesh_update_version`），无已发布版本回 204。
  - 安装包上传走门户存储（`/api/storage/upload`，`object_type=nanomesh-updates`），下载由门户转发，不在 Gitee Release。
  - nginx 需将根路径 `/nanomesh-updates/` 反代到门户 8080（与 `/agent-updates/` 同款，关闭 gzip）。
  - 门户侧建议配置 `[assets] nanomesh_updates_public_base_url = "https://cmx.pansoft.com"`，避免多层反代改写 `X-Forwarded-Proto` 导致下载地址协议错误。
- 备用/后续端点：`https://relay.robot.link/updates/latest.json`
  - 运维确认能提供静态路由后再切为主端点；本期不依赖它。

安装包仍上传到 Gitee Release：

```text
https://gitee.com/warpdrivelabs/nanomesh/releases/download/<tag>/<file>
```

### 3.4 客户端集成方式

当前 UI 无打包器，直接引入 `@tauri-apps/plugin-updater` 会引入前端构建链。为保持现有静态 UI：

- Rust 侧引入 `tauri-plugin-updater`；重启使用 Tauri 内置 `AppHandle::restart()`，不需要额外 process 插件。
- 在 `src-tauri` 暴露两个命令：
  - `app_check_update`：返回 `{ available, version, notes }`
  - `app_download_install`：下载并安装，向前端发 `update-progress` 事件，完成后重启
- 前端仍用 `window.__TAURI__.core.invoke(...)` 调用，不新增 npm 包、不改 UI 构建方式。命令在 Rust 侧调用插件 API，不依赖前端 updater 权限。

### 3.5 平台行为

| 平台 | 更新包 | 自动安装 | 说明 |
|---|---|---|---|
| Windows x64 | NSIS `.exe` + `.sig` | ✅ | 首选 NSIS；`installMode=perMachine` 安装到 `C:\Program Files`，安装/更新需 UAC；MSI 同时发布供首次安装 |
| macOS Apple Silicon | `.app.tar.gz` + `.sig` | ✅（需签名） | DMG 供首次安装；更新用 tar.gz；必须 Developer ID 签名，建议公证 |
| macOS Intel | 同上，`x86_64` | ✅（需签名） | 两个架构分别构建 |
| Linux x64 AppImage | `.AppImage.tar.gz` + `.sig` | ✅ | 不需要 root |
| Linux x64 deb | `.deb` + `.sig` | ✅（需授权） | 通过 `pkexec dpkg -i` 安装；需要 polkit/桌面会话 |
| Linux x64 rpm | `.rpm` + `.sig` | ✅（需授权） | 通过 pkexec + rpm 安装；需要 polkit/桌面会话 |

> 本期（2026-10-29）只实现并验证 **Windows x64 NSIS**；上表 macOS/Linux 行是后续目标，不在本期验收范围。

不采用 delta 更新，首次实现一律下载完整包。

## 4. latest.json 规范

```json
{
  "version": "0.1.1",
  "notes": "应用内更新；修复注册中心 TLS/域名同步提示。",
  "pub_date": "2026-10-29T12:00:00Z",
  "platforms": {
    "windows-x86_64": {
      "signature": "<对应 .sig 文件内容>",
      "url": "https://gitee.com/warpdrivelabs/nanomesh/releases/download/v0.1.1/nmspace_0.1.1_x64-setup.exe"
    },
    "darwin-aarch64": {
      "signature": "<...>",
      "url": "https://gitee.com/.../nmspace_0.1.1_aarch64.app.tar.gz"
    },
    "darwin-x86_64": {
      "signature": "<...>",
      "url": "https://gitee.com/.../nmspace_0.1.1_x64.app.tar.gz"
    },
    "linux-x86_64-deb": {
      "signature": "<...>",
      "url": "https://gitee.com/.../nmspace_0.1.1_amd64.deb"
    },
    "linux-x86_64-rpm": {
      "signature": "<...>",
      "url": "https://gitee.com/.../nmspace_0.1.1_x86_64.rpm"
    },
    "linux-x86_64-appimage": {
      "signature": "<...>",
      "url": "https://gitee.com/.../nmspace_0.1.1_amd64.AppImage.tar.gz"
    }
  }
}
```

要求：

- `version` 与 tag、安装包版本、`tauri.conf.json` 版本一致。
- `signature` 为 Tauri 生成的 `.sig` 文件原文（不是代码签名）。
- `url` 必须是公开可匿名下载的 HTTPS 地址。
- Linux 使用安装格式后缀键（`linux-x86_64-deb` / `-rpm` / `-appimage`），updater 按当前安装格式选择；只登记 `linux-x86_64` 作为兜底。键名以实际生成的 `.sig` / TrueMate 页面的 `{os}-{arch}-{installer}` 约定为准。
- 只有出现在 manifest 的平台/安装格式才会被更新；不要为不发布的格式写条目。
- 先上传安装包，最后发布 manifest，避免客户端拿到 404/旧包。

## 5. 更新 UI / UX

- 启动后延迟 5 秒静默检查一次；24 小时内不重复自动检查。
- 设置页增加“检查更新”按钮，手动检查不受 24 小时限制。
- 发现新版本：
  - 展示版本号、发布日期、更新说明、包大小
  - `立即更新` / `稍后` / `跳过此版本`
  - 下载中显示进度和速度；允许取消
  - 安装完成后提示并重启
- 失败状态：网络失败、签名失败、无权限、磁盘不足分别给出可理解文案；不阻塞主界面。
- “跳过此版本”存 `localStorage`；新版本出现后自动失效。
- Linux deb/rpm：可以自动更新，但安装阶段会通过 pkexec 弹出管理员授权；无 polkit/无桌面会话时提示手动下载，不静默失败。AppImage 不需要 root。

## 6. 安全要求

1. HTTPS only；manifest 和包 URL 不允许 http。
2. 客户端内置公钥，所有更新包必须通过 `.sig` 验证；验证失败拒绝安装。
3. 私钥/密码不进入仓库、Release、日志、截图、文档。
4. manifest 不允许指向第三方域（白名单仅 Gitee Release 与自有静态域）。
5. 更新包文件名不可复用；同一版本不得覆盖旧文件。
6. 发布流程要求“签名 → 上传包 → 发布 manifest”顺序；禁止先发 manifest。
7. macOS 更新包必须签名；未签名包不得进入 stable manifest。
8. 日志只记录版本/URL/错误码，不记录私钥、token、完整下载 URL 中的敏感查询参数。
9. Linux deb/rpm 安装通过 pkexec 调系统包管理器，应用不保存管理员密码；无 polkit 时 fail-closed 并提示手动下载。

## 7. 边界（本期不做）

- 不做增量/差分更新。
- 不做 beta/stable 双通道（先单一 stable；后续可加 `beta.json`）。
- 不承诺无 polkit / 无桌面会话环境下的 deb/rpm 自动更新；不做 Flatpak/Snap 更新。
- 不做强制更新/最低版本拦截。
- 不做更新包 CDN/多源测速。
- 不做自动密钥轮换；轮换必须人工评审。
- 不做 Web 端（浏览器网关）更新，只有 Tauri 桌面端。

## 8. 实施清单

### 阶段 A：密钥与基础配置
- [ ] 内部测试沿用 TrueMate 共用 keypair（仅内部）；公开版前生成 nanomesh 独立 key
- [ ] 公钥写入 `plugins.updater.pubkey`，私钥进入本机安全目录/CI Secret
- [ ] `bundle.createUpdaterArtifacts: true`
- [ ] `plugins.updater.endpoints` 指向门户 `https://cmx.pansoft.com/nanomesh-updates/latest.json`
- [ ] `updates/latest.json` 纳入仓库发布流程
- [ ] `.gitignore` 覆盖 `*.key`、私钥相关文件

### 阶段 B：Rust 命令与 UI
- [ ] 接入 `tauri-plugin-updater`、`tauri-plugin-process`
- [ ] 实现 `app_check_update` / `app_download_install`
- [ ] 进度事件、错误映射、重启
- [ ] 设置页“检查更新”、启动静默检查、跳过版本
- [ ] Linux deb/rpm 安装路径的 pkexec/polkit 错误映射；AppImage 无 root 路径
- [ ] capability 最小权限

### 阶段 C：manifest 与发布脚本
- [ ] 生成 `latest.json` 的脚本（读取版本、notes、`.sig`、资产 URL）
- [ ] schema/字段校验（版本、平台键、签名非空、URL https）
- [ ] 发布顺序与校验步骤写入 `RELEASE_PLAN.md`

### 阶段 D：CI 与跨平台
- [ ] Windows/macOS/Linux 构建矩阵
- [ ] 签名密钥注入
- [ ] 产物收集、Gitee Release 上传、manifest 发布
- [ ] 失败重试与产物完整性校验

### 阶段 E：演练与发布
- [ ] 在测试 tag 上完成一次 0.1.0 → 0.1.1 的真实升级
- [ ] Ubuntu deb / Fedora rpm / AppImage 各验证一次自动升级
- [ ] 无 polkit/无桌面会话场景验证回退提示
- [ ] 篡改包验证拒绝
- [ ] 断网/弱网/跳过版本/重启验证
- [ ] 正式发布

## 9. 验收标准

> 本期只验收 Windows x64；macOS/Linux 在 Windows 通过后另行验收。

1. 已安装 `0.1.0` 的 Windows 客户端能检测到 `0.1.1` 并完成升级、重启后版本正确。
2. 篡改更新包或使用错误签名时，客户端拒绝安装并提示。
3. 断网时检查更新不崩溃、不弹错误轰炸；手动重试可用。
4. “跳过此版本”生效，下一版本自动恢复检查。
5. Linux deb/rpm 自动更新能弹出授权并完成；无 polkit 时给出明确手动下载提示。
6. macOS 未签名/未公证的包不会进入 stable manifest。
7. 仓库与 CI 日志中不存在私钥、密码或完整签名私钥内容。
8. `latest.json` 与 Release 资产一一对应，所有 URL 返回 200。

## 10. 风险与应对

| 风险 | 说明 | 应对 |
|---|---|---|
| 已泄露密钥 | 私钥/密码出现在对话；内部按用户决定与 TrueMate 共用 | 仅内部共用；公开版前作废并生成 nanomesh 独立 key；禁止日志输出 |
| Gitee raw 风控/缓存 | fallback 端点可能返回 HTML/旧内容 | 主端点用自有静态站点；发布后校验响应体 |
| macOS 未签名 | Gatekeeper 拦截、更新失败 | 购买/配置 Developer ID，公证后再进 stable |
| Windows SmartScreen | 未签名安装包提示 | 可选代码签名证书；至少提供 SHA256 |
| Linux 包格式 | deb/rpm 更新需要 pkexec/包管理器；依赖冲突或缺失 polkit 会失败 | 运行时检测 polkit；失败回退手动下载；发布前在 Ubuntu/Fedora 实测 |
| manifest 先于资产发布 | 客户端 404 | 固定发布顺序，脚本校验 URL 200 |
| 版本比较/回滚 | 降级不受支持 | 发布新版本修复；保留旧 Release 资产；文档说明回滚方式 |
| CI 密钥泄露 | 日志/缓存 | Secret 注入，禁止 echo，构建日志脱敏 |
| Gitee 100MB 限制 | 大包上传失败 | 拆分架构包；universal 包评估体积；超限转外部存储 |

## 11. 已定口径（2026-10-29）

- 内部测试与 TrueMate 共用 updater keypair；公开版前拆独立 key。
- `latest.json` 放 Gitee 仓库 `updates/latest.json`，主端点用 Gitee raw。
- 本期只发 Windows、本地构建验证；macOS/Linux 后续再做。
- 本地构建优先，CI（GitHub Actions / Gitee Go）后续再引入。

剩余待办（不阻塞本期 Windows 实施）：

- Windows 是否申请代码签名证书（可后置，先提供 SHA256）。
- macOS Developer ID / 公证（macOS 批次前必须确认）。
- relay 静态端点是否启用（本期用 Gitee raw，不依赖）。
