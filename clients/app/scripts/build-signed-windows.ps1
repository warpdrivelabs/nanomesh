# nmspace Windows 签名构建（NSIS 更新产物 + .sig）。
#
# 用法（clients/app/ 下）：
#   powershell -ExecutionPolicy Bypass -File scripts\build-signed-windows.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\build-signed-windows.ps1 -KeyPassword <密码>
#   $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD="..."; powershell -ExecutionPolicy Bypass -File scripts\build-signed-windows.ps1
#
# 私钥位置（仓库外，gitignore 已挡 *.key）：~\.tauri\cmx-agent.key
#   与 cmx-agent 同一把更新签名钥（tauri.conf.json 烧录的 pubkey 即该钥对的公钥，
#   换钥必须同步换 tauri.conf.json）。可用 -MinisignKey 或 $env:MINISIGN_KEY 覆盖。
# 产物：src-tauri\target\release\bundle\nsis\nmspace_x.y.z_x64-setup.exe + 同名 .sig
#       （exe + .sig 一对，一起登记发布）
#
# 与 cmx-agent 版的差异：
#   - 去掉 cargo clean：本项目 UI（..\ui 与 cmx-agent-ui 资源）都是打包期拷贝，
#     不编译进二进制，增量编译不影响 UI 新鲜度。
#   - 打包前自动同步 cmx-agent UI 到 src-tauri\cmx-agent-ui（bundle.resources
#     构建期取该目录；入库时只是 README 占位，不同步打出的包运行期加载不到 UI）。

param(
    [string]$KeyPassword = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD,
    [string]$MinisignKey = $env:MINISIGN_KEY,
    [string]$CmxUiDir = $env:CMX_AGENT_UI_DIR
)

$ErrorActionPreference = "Stop"

$appDir = Split-Path -Parent $PSScriptRoot
$srcTauri = Join-Path $appDir "src-tauri"
if (-not $MinisignKey) {
    $MinisignKey = Join-Path $HOME ".tauri\cmx-agent.key"
}

if (-not (Test-Path $MinisignKey)) {
    Write-Error "找不到更新签名私钥：$MinisignKey —— 把 cmx-agent 的同一把私钥放到该位置（或 -MinisignKey <路径> 指向别处）后重试"
}
if (-not $KeyPassword) {
    $KeyPassword = Read-Host "输入私钥密码"
}

# 打包前同步 cmx-agent UI（选树逻辑与 cmx_host.rs 的运行期兜底一致：
# 须含 index.html + embed/truemate-app.js），从最新的 cargo git checkout 取。
function Find-CmxUi {
    $roots = @()
    if ($env:CARGO_HOME) { $roots += $env:CARGO_HOME }
    $roots += (Join-Path $HOME ".cargo")
    $best = $null
    $bestM = [datetime]::MinValue
    foreach ($root in $roots) {
        $co = Join-Path $root "git\checkouts"
        Get-ChildItem -Directory -Path $co -Filter "cmx-agent-*" -ErrorAction SilentlyContinue |
            ForEach-Object {
                Get-ChildItem -Directory $_.FullName | ForEach-Object {
                    $ui = Join-Path $_.FullName "crates\cmx-agent-web\ui"
                    if ((Test-Path (Join-Path $ui "index.html")) -and (Test-Path (Join-Path $ui "embed\truemate-app.js"))) {
                        $m = (Get-Item $_.FullName).LastWriteTimeUtc
                        if ($m -ge $bestM) { $best = $ui; $bestM = $m }
                    }
                }
            }
    }
    return $best
}

function Sync-CmxUi {
    $src = $CmxUiDir
    if (-not $src) {
        $src = Find-CmxUi
        if (-not $src) {
            # 首建机器还没有 checkout：先 fetch 把 cmx-agent git 依赖拉下来再找
            #（sync 先于 build，否则首次构建必失败在 sync）。
            Write-Host "未发现 cmx-agent checkout，先 cargo fetch 拉取 git 依赖…"
            Push-Location $srcTauri
            try {
                cargo fetch
                if ($LASTEXITCODE -ne 0) { Write-Error "cargo fetch 失败（exit $LASTEXITCODE）" }
            } finally { Pop-Location }
            $src = Find-CmxUi
        }
    }
    if (-not $src -or -not (Test-Path (Join-Path $src "index.html")) -or -not (Test-Path (Join-Path $src "embed\truemate-app.js"))) {
        Write-Error "找不到合格的 cmx-agent UI —— 可设 -CmxUiDir 或 `$env:CMX_AGENT_UI_DIR=<cmx-agent checkout>\crates\cmx-agent-web\ui 后重跑"
    }
    $dst = Join-Path $srcTauri "cmx-agent-ui"
    # git 只跟踪本目录的 README 占位（.gitignore 放行它），Remove-Item 会连它一起删掉；
    # 先备份再补回，否则每打一次包工作区就多一条「删除 README」的待提交改动。
    $readme = Join-Path $dst "README.md"
    $readmeBak = $null
    if (Test-Path $readme) { $readmeBak = [IO.Path]::GetTempFileName(); Copy-Item $readme $readmeBak -Force }
    if (Test-Path $dst) { Remove-Item $dst -Recurse -Force }
    New-Item -ItemType Directory -Path $dst | Out-Null
    Copy-Item (Join-Path $src "*") $dst -Recurse -Force
    if ($readmeBak) { Copy-Item $readmeBak $readme -Force; Remove-Item $readmeBak -Force }
    Write-Host "cmx-agent UI 已同步: $src"
}
Sync-CmxUi

Set-Location $appDir # 从 clients/app 跑 npx，用 node_modules 里的固定版本 CLI（需先 npm install）

# vendored OpenSSL（openssl-src）构建需要 perl，且必须含 Locale::Maketext::Simple 模块——
# Perl 5.41+ 从核心移除了它，Git Bash 自带的新版 MSYS perl 因此不合格（Configure 会 exit 2）。
# 候选顺序：PATH 里的 → Strawberry 标准安装位 → 从 git.exe 推导 Git 自带的；逐个模块自检后采用。
$perlCandidates = @()
$cmd = Get-Command perl.exe -ErrorAction SilentlyContinue
if ($cmd) { $perlCandidates += $cmd.Source }
foreach ($fixed in @("C:\Strawberry\perl\bin\perl.exe", "D:\Strawberry\perl\bin\perl.exe")) {
    if (Test-Path $fixed) { $perlCandidates += $fixed }
}
$git = Get-Command git.exe -ErrorAction SilentlyContinue
if ($git) {
    $dir = Split-Path $git.Source
    for ($i = 0; $i -lt 3 -and $dir; $i++) {
        $p = Join-Path $dir "usr\bin\perl.exe"
        if ((Test-Path $p) -and ($perlCandidates -notcontains $p)) { $perlCandidates += $p }
        $dir = Split-Path $dir
    }
}
$perlOk = $null
# PS 5.1 坑：EAP=Stop 时原生程序 stderr + 2>$null 会包装成 NativeCommandError 杀掉脚本，
# 自检（缺模块的候选必然打 stderr）永远走不到下一个候选 —— 这里临时放宽。
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
foreach ($cand in $perlCandidates) {
    & $cand -MLocale::Maketext::Simple -e "1" 2>$null
    if ($LASTEXITCODE -eq 0) { $perlOk = $cand; break }
}
$ErrorActionPreference = $prevEap
if (-not $perlOk) {
    Write-Error "没有合格的 perl（需含 Locale::Maketext::Simple，OpenSSL Configure 必需）—— 安装 Strawberry Perl：winget install StrawberryPerl.StrawberryPerl"
}
$env:PATH = "$(Split-Path $perlOk);$env:PATH"
# 同时显式指定给 openssl-src（优先读 OPENSSL_SRC_PERL，其次 PERL，最后 PATH 上的 perl）：
# 避免选中 PATH 里排在后面的 Git MSYS perl —— 它缺 Locale::Maketext::Simple，Configure 会挂。
$env:OPENSSL_SRC_PERL = $perlOk
Write-Host "构建用 perl: $perlOk"

# 内容型 env 全版本通吃（CLI 只认内容型）；PATH 型一并设置兼容新版。
$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content $MinisignKey -Raw
$env:TAURI_SIGNING_PRIVATE_KEY_PATH = $MinisignKey
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $KeyPassword

# 本机 Windows 构建走 vendored OpenSSL（openssl-src 源码编译，即上面自检的 perl 的用途）。
# 仓库根 .cargo/config.toml 的 [env] 为 Fedora 设了 OPENSSL_NO_VENDOR=1（走系统 openssl），
# 本机 Windows 没装系统 OpenSSL 会被误伤。cargo 规则：进程 env 优先于 config 的非 force [env]，
# 而 openssl-sys 认 "0" = 仍走 vendor —— 这里显式压回 vendor 路线。
$env:OPENSSL_NO_VENDOR = "0"
# gitee 的 smart HTTP 响应 Content-Type 不标准，cargo 内置 git 客户端会拒收；
# 走系统 git 命令行拉取可绕开（cmx-agent 依赖 bump 后重新 fetch 时会用到）。
$env:CARGO_NET_GIT_FETCH_WITH_CLI = "true"

npx tauri build
if ($LASTEXITCODE -ne 0) {
    Write-Error "tauri build 失败（exit $LASTEXITCODE）"
}

# 防漏签自检：NSIS 产物旁必须产出同名 .sig（缺了说明签名步没跑，勿分发）。
$sigs = Get-ChildItem "$srcTauri\target\release\bundle\nsis\*.sig" -ErrorAction SilentlyContinue
if (-not $sigs) {
    Write-Error "未找到 .sig —— 更新产物没有签名，禁止分发；检查私钥/密码后重跑"
}

Write-Host ""
Write-Host "构建完成（exe + .sig 成对，一起登记发布）：" -ForegroundColor Green
Get-ChildItem "$srcTauri\target\release\bundle\nsis\*" | Where-Object { $_.Extension -in ".exe", ".sig" } |
    ForEach-Object { Write-Host ("  " + $_.FullName) }
