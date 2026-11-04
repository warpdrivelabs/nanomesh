# nmspace 桌面端自动登录方案（方案 A：keychain 存账号口令）

> 20261103 · javier · 待评审

## 一、目标

应用启动后，对上次登录的账号（`nmspace-account-last`）自动完成登录，直接进入主界面，无需用户重输口令。登录失败时静默回退到现有手输登录页，用户体验与现状完全兼容。

## 二、现状与关键事实

应用有两层凭据：

| 层 | 现状 |
|---|---|
| 设备保险库（本地 vault） | **已自动化**：启动时 `routeStart()`（auth.js:292）静默 `ensure_device`，随机口令存 keychain |
| 账号登录（家节点） | **每次手输**：`account_login`（account.rs:158）真连家节点做 `name.login` 口令比对，本机不落盘 |

自动登录只解决第二层。

### 关键代码事实

- keychain 封装：`registry/vault.rs` 模块私有 `mod devkey`（get/set，service=`io.nmspace.app`，account=`device-vault:<dir>`），未导出，无 delete
- 登录命令：`account_login`（registry/account.rs:157-223），流程 = `open_device_vault` → `resolve_home_node` → 探测拨号比对口令 → 公钥归属校验 → `connect()`；登录即完整初始化（inbox/事件/roam driver），无"仅首次"逻辑，重复登录安全（connect 前 `clear_conn` 防互踢）
- 前端成功路径：account.js:377-388 → `saveKey` + `remember` → `enter(r, nickname)`（account.js:330-342，设 Identity/CURRENT_SVC → `showMainView` → `imStart`）
- 断开/切换：`disconnect` 命令 + `imDisconnect()`（im.js:337-351）；**无清除缓存口令的路径，需新增**
- 改密：`account_passwd`（account.rs:227）/ `account_reset`（:253）成功后必须同步更新 keychain，否则下次自动登录 `bad_password`
- 启动时序无互斥：单实例插件防重复启动；`roam_reconnect_driver` 只在已有连接时活动；并发由 `reconnect_lock` 串行
- 设计前提需修订：account.rs:100 与 account.js:1 注明「本机不保存账号口令」，落地后需改文案/文档

## 三、实现设计

### 3.1 后端（Rust）

**① 扩展 keychain 封装（vault.rs）**

- `mod devkey` 改 `pub(crate)`，新增 `remove(dir_key)` 函数
- 账号口令条目：service 不变，account = `account-pw:<local>@<domain>`

**② 新增三个命令（registry/account.rs + runtime.rs:165-171 注册）**

```
account_credentials_save(local, domain, password)
  - 登录成功后由前端调用；keychain set，失败仅 WARN 不阻断
    （Android 上 devkey 返回 Unsupported，自动登录天然不生效，见 3.3）

account_credentials_remove(local, domain)
  - 登出时清凭据；条目不存在也返回 ok（幂等）
```

（不实现 `account_credentials_get`：口令读取只在 `account_auto_login` 内部进行，
不暴露给前端，口令不过前端更干净。）

**③ 新增自动登录命令 `account_auto_login`**（keychain 读取放后端，口令不回流前端）

```
输入：local, domain, user, nickname, registry, relay_urls, pkarr_url, dns_origin
流程：
  1. devkey::get("account-pw:<local>@<domain>") → None 则返回 { ok:false, reason:"no_credentials" }
  2. 复用 account_login 现有流程（建议把 158-223 主体抽成内部函数 fn do_login(...)，
     account_login 与 account_auto_login 共用）
  3. reason 分两类返回，前端按类别处理（见 3.2①）：
     - 凭据类：bad_password / no_such_user → keychain remove 该条目后返回
       not_key_owner → **不删凭据**：该错误含「本机没有该名字私钥」的情况
       （导入备份/换机/webview 数据被清），口令本身是对的，删了凭据用户下次
       重登还要重新勾选记住。只返回错误码回退登录页。
     - 网络类：resolve_home_node 失败 / probe 拨号超时 / 连接不可达，
       统一归一为 reason:"network"（registry_phrase 现无网络类错误码，
       do_login 内需按错误形态归类）
  4. vault_orphaned：透传错误码，走既有 ensureDevice() 确认流程
成功返回与 account_login 相同：{ user, name, node }
```

**④ 改密同步**：`account_passwd` / `account_reset` 成功后，用新口令重写 keychain 条目。

### 3.2 前端（ui/js）

**① 自动登录入口（auth.js `routeStart()`）**

```
ensure_device 成功后、showLoginView() 之前：
  1. last = lastAccount()；无记录 → 直接 showLoginView()（现状）
  2. 调 NM.inv("account_auto_login", {...infra(), local, domain, user, nickname})
  3. ok=true  → saveKey + remember + await enter(r, nickname)
  4. ok=false → showLoginView()，密码框预填 local/domain（现状样式），
     reason=bad_password 时 toast「口令已变更，请重新登录」
  5. 网络/超时类失败（超时 8s、家节点不可达）→ 不立即放弃：
     延迟 5s 重试一次；再失败才 showLoginView()，toast「自动登录失败，
     请手动登录」（与凭据失效提示区分，避免用户误以为记住密码坏了）
     —— 开机自启场景网络常晚于 app 就绪，凭据明明有效不该一次超时就被踢回登录页
  6. 任何其他异常 → showLoginView()，绝不阻塞启动
```

- 加全局节流标记，防止 routeStart 被 UI 回退逻辑二次触发重复登录
- `ensure_device` 报 `vault_orphaned` 时跳过自动登录（现有 catch 吞错行为保留，用户在登录页走 ensureDevice 确认流程）

**② 登出/切换账号清除凭据（im.js `imDisconnect`、usermenu.js `switchUser`）**

- 「退出登录」按钮改为调用 `account_credentials_remove`（新增，keychain remove）+ 现有 disconnect 流程
- `switchUser` 只断开不删凭据（保留该账号的自动登录能力）

**③ 登录页加勾选项「记住密码，下次自动登录」**（**默认不勾选**）

- 默认不勾选：keychain 条目无生物识别保护、同会话任意进程可读，账号口令常是
  用户在别处复用的口令，泄露面大于随机设备口令——勾选应是用户的明确授权动作
- 勾选时 submit() 成功后调 `account_credentials_save`；不勾选则 remove
- 勾选状态记忆：凭据类失败（bad_password 等）删凭据后，用户重登容易忘记再勾，
  导致记住密码静默失效。登录页用 localStorage 记住「上次勾选状态」做预填，
  且凭据类失败 toast 补一句「可重新勾选记住密码」

### 3.3 Android 方案

devkey 在 Android 返回 `Unsupported`（keyring crate 无 Secret Service），
`account_credentials_save` 失败仅 WARN → **Android 上自动登录不生效**，登录页
不显示勾选项（devkey Unsupported 可由 get 探测，或直接按 user agent/编译 target 判断）。

演进选项（本期不做，登记待办）：
- **方案甲**：Android 用 Keystore 生成设备密钥（硬件级，未 root 不可导出）加密口令后落盘，
  等价于 keychain 语义。需要新增一个小依赖或手写 android.keystore 桥接，工作量约 1-2 天。
- **方案乙**：沿用 device vault 模式——随机口令存 0600 文件，账号口令再用它加密一层落盘。
  安全水位低于 Keystore（root 可读），但与现有 Android device vault 回退同水位，改动最小。
- 建议：桌面端先上，Android 走方案乙，等有安全侧反馈再升甲。

### 3.4 不改的东西

- `account_login` 命令签名与行为不变（手动登录路径原样）
- vault_orphaned 五流程预检不变
- `roam_reconnect_driver` / 待机唤醒重连逻辑不变（依赖 `last_connect`，自动登录后同样生效）

## 四、错误场景矩阵

| 场景 | 行为 |
|---|---|
| 无缓存凭据 | 静默回登录页（现状） |
| 离线 / 家节点不可达 | 后端归一 reason:"network" → 前端延迟 5s 重试一次 → 仍失败回登录页 toast「自动登录失败，请手动登录」 |
| 口令已被改（bad_password） | 删缓存凭据 → 回登录页 toast「口令已变更，请重新登录」+「可重新勾选记住密码」 |
| 账号不存在（no_such_user） | 同上 |
| keychain 拒绝访问（Denied） | 视为无凭据，回登录页 |
| vault_orphaned | 跳过自动登录，登录页走既有确认重置流程 |
| 自动登录与手动操作并发 | `reconnect_lock` 在 `connect()` 入口（connection.rs:352）与重连单飞共用：自动登录与手点登录都走 connect，**串行执行、后到者 clear_conn 踢掉先到者**（非防重复登录）。routeStart 节流标记负责挡住前端重复触发；后端串行保证不双 bind |
| 退出登录 | 删凭据 + disconnect，下次不自动登录 |
| 改密成功（keychain 同步失败） | `account_passwd` 成功但 keychain 写失败（Unsupported/Denied）→ 下次自动登录 bad_password → 凭据被删，链路自愈但用户视角是「记住密码悄悄失效」；可接受，接受度待用户反馈 |
| 改密成功（keychain 同步成功） | keychain 同步更新，自动登录继续可用 |

## 五、安全评估

- 账号口令进 keychain，安全上限 = 本机系统账户（与现有随机设备口令存 keychain 同水位，不降低整体安全）
- 口令不回流前端（`account_auto_login` 内部读取），前端 `account_credentials_save` 只上行
- 家节点侧零改动：仍是标准 `name.login` 口令认证
- 勾选是用户对口令落 keychain 的明确授权，不做静默收集；勾选状态 localStorage 预填（见 3.2③）
- 已知折衷：离线时无法自动登录（口令比对必须连家节点）；若未来要求"口令不出本机"，演进方向是方案 B（设备密钥级认证，需服务端配合）

## 六、工作量与验证

**改动文件**：vault.rs（+remove、导出）、account.rs（抽 do_login + 3 个新命令，含 reason 网络类归一）、runtime.rs（注册）、account.js（勾选+save+勾选状态记忆）、auth.js（routeStart 自动登录+网络类重试）、im.js / usermenu.js（登出清凭据）、文案注释 2 处

**验证清单**：
1. 登录勾选记住 → 重启 app → 直达主界面
2. 取消勾选 → 重启 → 回登录页
3. 改密后重启 → 自动登录成功（keychain 已同步）
4. 家节点侧改密后重启 → bad_password → 凭据清除 → 回登录页
5. 退出登录 → 重启 → 不自动登录；切换账号 → 各账号可自动登录
6. vault_orphaned 场景：归档确认 → 重置 → 手动登录 → 重新记住
7. 开机自启（HIDDEN_ARG）下自动登录正常
8. 断网启动 → 延迟重试一次 → 仍失败回登录页，toast 区分文案，不卡启动
