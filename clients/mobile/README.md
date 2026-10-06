# nmspace 移动端（Android 先 · iOS 后 · 手机 + 平板）

独立于桌面工程 `clients/app`（后者**保持冻结、零改动**）。本工程以桌面版为**基线拷贝**，复用工作区共享后端 crate（`crates/*`，path 依赖），在此之上做移动化（响应式/触摸/安全区/后台推送）。两端各自独立构建与打包，互不影响。

> 方案与路线：见 [`../../docs/MOBILE_PLAN.md`](../../docs/MOBILE_PLAN.md)。基线分析：[`../../docs/CURRENT_STATE_ANALYSIS.md`](../../docs/CURRENT_STATE_ANALYSIS.md)。

## 目录
```
clients/mobile/
├── src-tauri/
│   ├── Cargo.toml        # package nmspace-mobile / lib nmspace_mobile_lib；path 依赖 ../../../crates/*
│   ├── src/              # lib.rs/main.rs/auth/devices/pair/pairing/tray（桌面专属段 #[cfg(desktop)] 自动跳过）
│   ├── tauri.conf.json   # identifier io.nmspace.mobile（与桌面 io.nmspace.app 区分，可并存安装）
│   ├── capabilities/     # 权限（移动端 P1 再精简窗口相关项）
│   └── icons/ Info.plist build.rs
└── ui/                   # 移动 UI 基线（拷自桌面 ui，P1 起做响应式/触摸改造）
```

工程**不在**根 workspace 内（根 `Cargo.toml` 已 `exclude = ["clients"]`），用 `tauri` / `cargo` 独立构建。

## 工具链准备

本机现状（2026-10-04 核查）：
- ✅ Rust Android targets 已装：`aarch64-linux-android` `armv7-linux-androideabi` `x86_64-linux-android` `i686-linux-android`
- ✅ Homebrew 可用
- ❌ **tauri CLI 未装** → `cargo install tauri-cli --version '^2'`（或 `npm i -g @tauri-apps/cli@^2`）
- ❌ **JDK 未装**（Gradle/AGP 需要）→ `brew install --cask temurin@17`，并 `export JAVA_HOME="$(/usr/libexec/java_home -v 17)"`
- ❌ **Android SDK/NDK 未装** → `brew install --cask android-studio`，在其 SDK Manager 里装 **Platform (API 34)** + **NDK** + **Build-Tools**；随后：
  ```sh
  export ANDROID_HOME="$HOME/Library/Android/sdk"
  export NDK_HOME="$ANDROID_HOME/ndk/<version>"
  ```

## 初始化与运行（Android）
```sh
cd clients/mobile
cargo tauri android init          # 生成 src-tauri/gen/android（本仓首次执行）
cargo tauri android dev           # 模拟器/真机热重载
cargo tauri android build         # 产出 APK/AAB
```
iOS（P4，需 macOS + Xcode + Apple Developer）：`cargo tauri ios init` / `ios build`。

## 桌面开发态快速校验（无需 Android 工具链）
```sh
cd clients/mobile/src-tauri && cargo check     # 验证后端复用与 Tauri 胶水编译通过
```

## 与桌面的关系
- **不改桌面**：本工程所有改动都在 `clients/mobile/`。
- **后端复用**：iroh/消息/群/频道/命名/多设备/保险库全在 `crates/*`，双端共享、零分叉。
- 日后可把两端共享的命令/SQLite 逻辑抽成 `crates/nm-app-core`（另开任务，届时仍不回改桌面）。
