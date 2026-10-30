# cmx-agent-ui（打包资源占位目录）

此目录是 `tauri.conf.json` 的 `bundle.resources` 条目，Tauri 构建时要求它在磁盘上存在。

- **开发态**：本目录只有这个 README；`cmx_host.rs::resolve_ui_dir` 找不到这里的 `index.html`，
  会自动回落到本地 cmx-agent checkout 的源码路径（`crates/cmx-agent-web/ui`）。
- **发布态**：发布脚本把选定的 cmx-agent checkout 的 `ui/`（含 `embed/`）同步进本目录，
  随 bundle 带到 `<bundle>/Contents/Resources/cmx-agent-ui`（macOS）/ exe 同级（Linux）。

`.gitignore` 忽略本目录全部内容、仅保留此 README（`cmx-agent-ui/*` + `!cmx-agent-ui/README.md`），
因此开发机不会把同步进来的 UI 资产误提交。
