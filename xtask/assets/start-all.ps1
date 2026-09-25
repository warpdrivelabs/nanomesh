# start-all.ps1 - NANO MESH 后端一键启动（Windows 发布包版）
# 首次运行自动从 nmd.toml.example 生成 nmd.toml（写入随机 admin token）。
# 可用环境变量：ADMIND_LISTEN（管理台监听地址，默认 0.0.0.0:9610）
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
New-Item -ItemType Directory -Force -Path run, logs, data | Out-Null
$Listen = if ($env:ADMIND_LISTEN) { $env:ADMIND_LISTEN } else { '0.0.0.0:9610' }

# 首次运行：生成 nmd.toml + 随机 token
if (-not (Test-Path nmd.toml)) {
    $bytes = New-Object byte[] 16
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    $token = ($bytes | ForEach-Object { $_.ToString('x2') }) -join ''
    (Get-Content nmd.toml.example -Raw) -replace '__ADMIN_TOKEN__', $token |
        Set-Content nmd.toml -Encoding UTF8
    Write-Host '[init] 已生成 nmd.toml（含随机 admin token）'
}

# 从 nmd.toml 读取 [admin] 配置
$conf = Get-Content nmd.toml -Raw
$ApiAddr  = [regex]::Match($conf, 'api_addr\s*=\s*"([^"]+)"').Groups[1].Value
$ApiToken = [regex]::Match($conf, 'api_token\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $ApiAddr) { $ApiAddr = '127.0.0.1:9611' }

function Test-Running($pidFile) {
    if (-not (Test-Path $pidFile)) { return $false }
    $procId = Get-Content $pidFile
    return [bool](Get-Process -Id $procId -ErrorAction SilentlyContinue)
}

function Start-One($name, $pidFile, $exe, $args_) {
    if (Test-Running $pidFile) {
        Write-Host "[$name] 已在运行 (PID: $(Get-Content $pidFile))，跳过"
        return
    }
    Write-Host "[$name] 启动中…"
    $p = Start-Process -FilePath $exe -ArgumentList $args_ -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput "logs\$name.log" -RedirectStandardError "logs\$name.err.log"
    $p.Id | Out-File $pidFile -Encoding ascii
    Start-Sleep -Seconds 2
    if (Test-Running $pidFile) {
        Write-Host "[$name] 启动成功 (PID: $($p.Id))  日志: logs\$name.log"
    } else {
        Write-Host "[$name] 启动失败，请查看 logs\$name.err.log"
        Remove-Item $pidFile -ErrorAction SilentlyContinue
        exit 1
    }
}

# 1) nmd（去中心网格节点）  2) nm-admind（后端管理台）
Start-One 'nmd' 'run\nmd.pid' "$PSScriptRoot\bin\nmd.exe" @('--config', 'nmd.toml')
Start-One 'nm-admind' 'run\nm-admind.pid' "$PSScriptRoot\bin\nm-admind.exe" @(
    '--listen', $Listen, '--nmd-api', "http://$ApiAddr",
    '--nmd-token', $ApiToken, '--state', 'admind.state.json')

Write-Host ''
Write-Host '==> 全部启动完成'
Write-Host "    管理台   http://$($Listen -replace '0\.0\.0\.0','127.0.0.1')/   （首次登录 admin / nmspace-admin，请立即改密）"
Write-Host '    节点日志 logs\nmd.log   管理台日志 logs\nm-admind.log'
Write-Host '    停止：stop-all.bat    重启：restart-all.bat'
