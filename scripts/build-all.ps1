# nanomesh 双平台一键签名打包：Windows(NSIS) + deepin WSL(deb/rpm)，产物集中到 Windows 下载目录。
#
# 用法（pwsh）：
#   pwsh -File scripts\build-all.ps1                          # 交互输密码，先 Windows 后 deepin
#   pwsh -File scripts\build-all.ps1 -KeyPassword <密码>
#   pwsh -File scripts\build-all.ps1 -WindowsOnly | -LinuxOnly   # 只打一侧
#
# 产物：$env:USERPROFILE\Downloads\nanomesh\
#   nmspace_x.y.z_x64-setup.exe + .sig            （Windows NSIS）
#   nmspace_x.y.z_amd64.deb / .rpm (+AppImage) + .sig （deepin；deepin 的 target 在 WSL 本地盘
#                                                    ~/.cache/nanomesh/target-linux，由构建脚本重定向）
# 说明：两侧串行执行——两个构建脚本都会重建共享的 src-tauri\cmx-agent-ui，并行会在打包读资源时互踩；
#       某侧失败会继续另一侧，结尾汇总成败与产物清单。
# 依赖：Windows = Strawberry Perl + NSIS 缓存（build-signed-windows.ps1 自检）；
#       deepin = tauri-cli 2.12.1 + protoc（build-signed-linux.sh 自检，2026-11-02 已装齐）。

param(
    [string]$KeyPassword,
    [switch]$WindowsOnly,
    [switch]$LinuxOnly
)

$ErrorActionPreference = "Stop"
# wsl.exe 的输出是 UTF-8：把控制台解码也设成 UTF-8，否则 bash/构建脚本里的中文全成乱码
try { [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new() } catch { }

$repo = Split-Path -Parent $PSScriptRoot   # scripts\ 的上级 = 仓库根
$distDir = Join-Path $env:USERPROFILE "Downloads\nanomesh"
New-Item -ItemType Directory -Force -Path $distDir | Out-Null
# Windows 路径 -> WSL /mnt 路径（只小写盘符，其余保留原大小写）
function To-WslPath([string]$p) {
    $p2 = $p -replace '\\', '/'
    if ($p2 -match '^([A-Za-z]):(.*)$') { return ("/mnt/{0}{1}" -f $Matches[1].ToLower(), $Matches[2]) }
    return $p2
}

if (-not $KeyPassword) { $KeyPassword = Read-Host "输入私钥密码" }

# Windows 侧 protoc 兜底（nm-proto 构建需要；通常用户级环境变量已设）
if (-not $env:PROTOC -and (Test-Path "D:\Packages\protoc\bin\protoc.exe")) {
    $env:PROTOC = "D:\Packages\protoc\bin\protoc.exe"
}

$report = @()   # "平台 | 成败 | 说明"
$artifacts = @()

# ---------- [1/2] Windows (NSIS) ----------
if (-not $LinuxOnly) {
    Write-Host "`n========== [1/2] Windows (NSIS) ==========" -ForegroundColor Cyan
    $prevEap = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    try {
        & pwsh -NoProfile -File (Join-Path $repo "clients\app\scripts\build-signed-windows.ps1") -KeyPassword $KeyPassword
        $code = $LASTEXITCODE
    } finally { $ErrorActionPreference = $prevEap }
    if ($code -eq 0) {
        $nsis = Join-Path $repo "clients\app\src-tauri\target\release\bundle\nsis"
        $got = @(Get-ChildItem "$nsis\*.exe", "$nsis\*.sig" -ErrorAction SilentlyContinue)
        if ($got.Count -gt 0) {
            $got | Copy-Item -Destination $distDir -Force
            $artifacts += $got.Name
            $report += "Windows | OK   | $($got.Count) 个文件"
        } else {
            $report += "Windows | 失败 | 构建退出码 0 但 nsis 目录没找到 exe/.sig"
        }
    } else {
        $report += "Windows | 失败 | build-signed-windows.ps1 退出码 $code（看上方日志）"
    }
}

# ---------- [2/2] deepin (deb/rpm) ----------
if (-not $WindowsOnly) {
    Write-Host "`n========== [2/2] deepin WSL (deb/rpm) ==========" -ForegroundColor Cyan
    $linuxSh = To-WslPath (Join-Path $repo "clients\app\scripts\build-signed-linux.sh")
    # 撞锁预警：deepin 里有 cargo 在跑就提示（cargo 自己会等锁，但不提醒会以为卡死）
    $prevEap = $ErrorActionPreference; $ErrorActionPreference = "Continue"
    $busy = ""
    try { $busy = wsl.exe -d deepin bash -c 'pgrep -x cargo >/dev/null && echo BUSY || echo IDLE' 2>$null } finally { $ErrorActionPreference = $prevEap }
    if ("$busy" -match "BUSY") {
        Write-Host "[提示] deepin 里有 cargo 进程在跑（可能在构建/下载依赖）——本脚本会与它抢锁排队，介意就先等它结束。" -ForegroundColor Yellow
    }
    try {
        wsl.exe -d deepin bash $linuxSh $KeyPassword
        $code = $LASTEXITCODE
    } catch { $code = 1 }
    finally { $ErrorActionPreference = $prevEap }

    if ($code -eq 0) {
        # 从 WSL 本地盘 target 收产物到 Windows 下载目录（deb/rpm/AppImage + 各自 .sig；只收文件跳过解包目录）
        $distWsl = To-WslPath $distDir
        $gather = @'
for d in deb rpm appimage; do
  p="$HOME/.cache/nanomesh/target-linux/release/bundle/$d"
  if [ -d "$p" ]; then
    for f in "$p"/*; do
      [ -f "$f" ] && cp -v "$f" "__DIST__"/
    done
  fi
done
'@ -replace '__DIST__', $distWsl
        $prevEap = $ErrorActionPreference; $ErrorActionPreference = "Continue"
        try { wsl.exe -d deepin bash -c $gather 2>&1 | ForEach-Object { "$_" } | Out-Null } finally { $ErrorActionPreference = $prevEap }
        $got = @(Get-ChildItem -LiteralPath $distDir -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '\.(deb|deb\.sig|rpm|rpm\.sig|AppImage|AppImage\.sig)$' })
        $report += if ($got.Count -gt 0) { "deepin  | OK   | $($got.Count) 个文件" } else { "deepin  | 失败 | 退出码 0 但没收到 deb/rpm 产物" }
    } else {
        $report += "deepin  | 失败 | build-signed-linux.sh 退出码 $code（看上方日志）"
    }
}

# ---------- 汇总 ----------
Write-Host ""
Write-Host "==================== 打包汇总 ====================" -ForegroundColor Cyan
$report | ForEach-Object { Write-Host "  $_" }
Write-Host ("  产物目录: {0}" -f $distDir) -ForegroundColor Green
Get-ChildItem $distDir -ErrorAction SilentlyContinue | Sort-Object LastWriteTime -Descending |
    Select-Object -First 12 | ForEach-Object { Write-Host ("    {0}  {1}" -f $_.Name, $_.LastWriteTime.ToString("MM-dd HH:mm")) }
Write-Host "  （更新产物 + 同名 .sig 成对登记发布）"
