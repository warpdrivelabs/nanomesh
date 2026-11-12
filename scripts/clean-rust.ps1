# 全机 Rust 空间清理：Windows + 所有 WSL 发行版 一锅端（PowerShell 7 单入口）。
#
# 用法（pwsh）：
#   pwsh -File scripts\clean-rust.ps1              # dry-run：只统计占用，不删任何东西
#   pwsh -File scripts\clean-rust.ps1 -Go          # 实际清理：所有 Rust 工程的 target/ + registry 缓存与源码
#   pwsh -File scripts\clean-rust.ps1 -Go -Git     # 连 cargo git 依赖缓存一起清（下次构建要重新 fetch；gitee 直连有坑，慎用）
#   pwsh -File scripts\clean-rust.ps1 -Root D:\code  # 追加扫描根（可多个）
#   pwsh -File scripts\clean-rust.ps1 -Go -KeepEnv # 只清 Windows，不动 WSL
#   pwsh -File scripts\clean-rust.ps1 -Toolchains  # 顺带列出 rustup 工具链（只提示，从不自动删）
#
# 清理对象（-Go）：
#   1) 扫描到的每个 Rust 工程的 target\（最大头；删后该工程下次全量重编）
#   2) 各 WSL 发行版 ~/.cache 下散落的 target*（CARGO_TARGET_DIR 重定向产物）
#   3) CARGO_HOME 的 registry\cache 与 registry\src（.crate 包与解压源码，可再生成）
# 不动：CARGO_HOME\git（-Git 才清）、rustup 工具链、bin\（cargo install 的 tauri-cli 等）、.tauri 私钥
# 保护：某环境发现 cargo/rustc/makensis 进程在跑（正在构建），该环境跳过 --go（dry-run 不受影响）。
# 默认扫描根：E:\Pansoft 与 $HOME 下常见工程目录 + 各 WSL 发行版的 $HOME。工程在别处用 -Root 补。
# 注意：本文件必须带 UTF-8 BOM（Windows PowerShell 5.1 无 BOM 会把中文读成乱码导致语法错）。

param(
    [switch]$Go,
    [switch]$Git,
    [switch]$Toolchains,
    [switch]$KeepEnv,
    [string[]]$Root = @()
)

$ErrorActionPreference = "Stop"

# ---------- 工具函数 ----------

function Get-DirSizeMB([string]$path) {
    # .NET Framework 兼容写法（PS5.1 无 EnumerationOptions）；无权限子目录抛错时按已累计部分返回
    if (-not (Test-Path -LiteralPath $path)) { return -1 }
    $sum = [long]0
    try {
        foreach ($f in [IO.Directory]::EnumerateFiles($path, "*", [IO.SearchOption]::AllDirectories)) {
            try { $sum += ([IO.FileInfo]$f).Length } catch {}
        }
    } catch { }
    return [math]::Round($sum / 1MB, 1)
}

function Format-SizeMB($mb) {
    if ($mb -lt 0) { return "-" }
    if ($mb -ge 1024) { return ("{0:N2} GB" -f ($mb / 1024)) }
    return ("{0:N0} MB" -f $mb)
}

# 手写栈式遍历找 Cargo.toml（跳过 target/node_modules/.git 等大目录，避免全树扫描）
$PruneDirs = @("target", "node_modules", ".git", ".rustup", "registry", "checkouts", "dist", ".cache")
function Find-RustTargets([string[]]$roots) {
    $found = @()
    foreach ($root in $roots) {
        if (-not (Test-Path -LiteralPath $root)) { continue }
        $stack = New-Object System.Collections.Generic.Stack[string]
        $stack.Push((Resolve-Path -LiteralPath $root).Path)
        while ($stack.Count -gt 0) {
            $dir = $stack.Pop()
            try { $entries = [IO.Directory]::EnumerateFileSystemEntries($dir) } catch { continue }
            $isProject = $false
            foreach ($e in $entries) {
                $name = [IO.Path]::GetFileName($e)
                if ($name -eq "Cargo.toml") { $isProject = $true; continue }
                if ($PruneDirs -contains $name) { continue }
                if ([IO.Directory]::Exists($e)) { $stack.Push($e) }
            }
            if ($isProject -and (Test-Path -LiteralPath (Join-Path $dir "target"))) {
                $found += (Join-Path $dir "target")
            }
        }
    }
    return $found
}

$script:FreedMB = [double]0
function Remove-OrReport([string]$path, [string]$note, [string]$tag) {
    if (-not (Test-Path -LiteralPath $path)) { return }
    $mb = Get-DirSizeMB $path
    if ($Go) {
        Remove-Item -LiteralPath $path -Recurse -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $path) {
            Write-Host "  [$tag][失败] $path（占用中？稍后重试）" -ForegroundColor Red
        } else {
            Write-Host ("  [$tag][已删] {0}  ({1})  {2}" -f $path, (Format-SizeMB $mb), $note)
            $script:FreedMB += $mb
        }
    } else {
        Write-Host ("  [$tag][可清理] {0}  ({1})  {2}" -f $path, (Format-SizeMB $mb), $note)
        $script:FreedMB += $mb
    }
}

function Test-BuildBusy {
    $procs = Get-Process -Name cargo, rustc, rustdoc, makensis -ErrorAction SilentlyContinue
    return ($null -ne $procs -and $procs.Count -gt 0)
}

# ---------- 1) Windows 侧 ----------

Write-Host ""
Write-Host ("==== Windows 侧 | 模式: {0} ====" -f $(if ($Go) { "实际清理" } else { "dry-run（-Go 执行）" })) -ForegroundColor Cyan

# CARGO_HOME（Windows 侧约定在 D:\Packages\cargo）
$cargoHome = $env:CARGO_HOME
if (-not $cargoHome -or -not (Test-Path $cargoHome)) {
    foreach ($cand in @("D:\Packages\cargo", (Join-Path $HOME ".cargo"))) {
        if (Test-Path $cand) { $cargoHome = $cand; break }
    }
}

# 扫描根
$scanRoots = @("E:\Pansoft")
foreach ($d in @("$HOME\projects", "$HOME\code", "$HOME\src", "$HOME\work", "D:\src", "D:\projects", "D:\code")) {
    if (Test-Path $d) { $scanRoots += $d }
}
if ($Root) { $scanRoots += $Root }
Write-Host ("扫描根: {0}" -f ($scanRoots -join "  "))

if ($Go -and (Test-BuildBusy)) {
    Write-Host "  [跳过] 检测到 cargo/rustc/makensis 正在运行 —— 避免删到正在用的产物，Windows 侧本轮不清理！" -ForegroundColor Yellow
} else {
    Write-Host "-- Rust 工程 target --"
    $targets = Find-RustTargets $scanRoots
    if (-not $targets) { Write-Host "  （未发现）" }
    foreach ($t in $targets) { Remove-OrReport $t "" "target" }

    Write-Host "-- cargo 缓存（$cargoHome）--"
    if ($cargoHome -and (Test-Path $cargoHome)) {
        Remove-OrReport (Join-Path $cargoHome "registry\cache") ".crate 下载包，重新下载即可" "cache"
        Remove-OrReport (Join-Path $cargoHome "registry\src")   "解压源码，重新解压即可" "cache"
        if ($Git) {
            Remove-OrReport (Join-Path $cargoHome "git\db")        "git 依赖裸库（下次 fetch 重拉；gitee 有坑！）" "git"
            Remove-OrReport (Join-Path $cargoHome "git\checkouts") "git 依赖工作副本（cmx-agent UI 同步也用它）" "git"
        } else {
            Write-Host "  [保留] $(Join-Path $cargoHome 'git')（加 -Git 才清）"
        }
    } else {
        Write-Host "  （未找到 CARGO_HOME）"
    }
}

if ($Toolchains) {
    Write-Host "-- rustup 工具链（只提示，不自动删；手动: rustup toolchain uninstall <名>）--"
    $prevEap = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    try {
        if (Get-Command rustup -ErrorAction SilentlyContinue) { rustup toolchain list 2>$null | ForEach-Object { Write-Host "  $_" } }
    } finally { $ErrorActionPreference = $prevEap }
}

Write-Host ("Windows 侧小计: {0}" -f (Format-SizeMB $script:FreedMB)) -ForegroundColor Cyan

# ---------- 2) WSL 各发行版 ----------

if (-not $KeepEnv) {
    $prevEap = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    $distros = @()
    try {
        $raw = (wsl.exe --list --quiet 2>$null | Out-String)
        $distros = $raw -replace "`0", "" -split "`r?`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ -and $_ -notmatch "docker" }
    } finally { $ErrorActionPreference = $prevEap }

    # Linux 侧清理逻辑（写临时 .sh 再由 wsl.exe 跑 —— PS 各版本对含引号多行参数的 argv 传递都不可靠）
    # bash 侧输出用英文（PS 控制台对 wsl.exe 的 UTF-8 输出解码不定，中文易乱码）
    $linuxBody = @'
CLEAN_MODE="${1:-DRY}"; CLEAN_GIT="${2:-NOGIT}"
set -u
[ -n "${WSL_DISTRO_NAME:-}" ] || exit 0
echo "==== WSL distro: $WSL_DISTRO_NAME | mode: ${CLEAN_MODE} ===="
if [ "$CLEAN_MODE" = "GO" ] && pgrep -x cargo >/dev/null 2>&1; then
  echo "  [skip] cargo is running (build in progress?)"
  exit 0
fi
while IFS= read -r -d '' m; do
  t="$(dirname "$m")/target"
  if [ -d "$t" ]; then
    echo "  [target] $t ($(du -sh "$t" 2>/dev/null | cut -f1))"
    [ "$CLEAN_MODE" = "GO" ] && rm -rf "$t"
  fi
done < <(find "$HOME" -maxdepth 5 -name Cargo.toml -not -path '*/target/*' -not -path '*/node_modules/*' -not -path '*/.cargo/*' -not -path '*/.cache/*' -print0 2>/dev/null)
while IFS= read -r -d '' t; do
  echo "  [target] $t ($(du -sh "$t" 2>/dev/null | cut -f1))"
  [ "$CLEAN_MODE" = "GO" ] && rm -rf "$t"
done < <(find "$HOME/.cache" -maxdepth 2 -type d -name 'target*' -print0 2>/dev/null)
CH="${CARGO_HOME:-$HOME/.cargo}"
for sub in registry/cache registry/src; do
  p="$CH/$sub"
  if [ -d "$p" ]; then
    echo "  [cache] $p ($(du -sh "$p" 2>/dev/null | cut -f1))"
    [ "$CLEAN_MODE" = "GO" ] && rm -rf "$p"
  fi
done
if [ "$CLEAN_GIT" = "GIT" ]; then
  for sub in git/db git/checkouts; do
    p="$CH/$sub"
    if [ -d "$p" ]; then
      echo "  [git] $p ($(du -sh "$p" 2>/dev/null | cut -f1))"
      [ "$CLEAN_MODE" = "GO" ] && rm -rf "$p"
    fi
  done
else
  echo "  [keep] $CH/git (pass -Git to clean)"
fi
'@
    $linuxSh = Join-Path $env:TEMP "clean-rust-linux.sh"
    [IO.File]::WriteAllText($linuxSh, $linuxBody, [Text.UTF8Encoding]::new($false)) # 无 BOM：bash 不吃 BOM
    # 只小写盘符，其余保留原大小写
    $p2 = $linuxSh -replace '\\', '/'
    $linuxShWsl = if ($p2 -match '^([A-Za-z]):(.*)$') { ("/mnt/{0}{1}" -f $Matches[1].ToLower(), $Matches[2]) } else { $p2 }

    $mode = $(if ($Go) { "GO" } else { "DRY" })
    $gitFlag = $(if ($Git) { "GIT" } else { "NOGIT" })
    foreach ($d in $distros) {
        $prevEap2 = $ErrorActionPreference; $ErrorActionPreference = "Continue"
        try {
            wsl.exe -d $d bash $linuxShWsl $mode $gitFlag 2>&1 | ForEach-Object { "$_" }
        } finally { $ErrorActionPreference = $prevEap2 }
    }
}

# ---------- 汇总 ----------

Write-Host ""
Write-Host ("==== 完成 | {0} | 清单合计约 {1} ====" -f $(if ($Go) { "已执行清理" } else { "dry-run，未删除任何文件；确认后加 -Go" }), (Format-SizeMB $script:FreedMB)) -ForegroundColor Cyan
Write-Host "提醒：删 target 后相关工程下次构建为全量重编；registry 缓存删后首次构建需重新下载。"
