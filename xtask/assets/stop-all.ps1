# stop-all.ps1 - 停止 NANO MESH 后端（先 nm-admind，再 nmd）。
$ErrorActionPreference = 'SilentlyContinue'
Set-Location $PSScriptRoot

function Stop-One($name, $pidFile) {
    if (-not (Test-Path $pidFile)) { Write-Host "[$name] 未运行（无 PID 文件）"; return }
    $procId = Get-Content $pidFile
    $p = Get-Process -Id $procId -ErrorAction SilentlyContinue
    if (-not $p) {
        Write-Host "[$name] 进程已退出，清理 PID 文件"
        Remove-Item $pidFile
        return
    }
    Write-Host "[$name] 停止中 (PID: $procId)…"
    Stop-Process -Id $procId -ErrorAction SilentlyContinue
    for ($i = 0; $i -lt 10; $i++) {
        if (-not (Get-Process -Id $procId -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Seconds 1
    }
    if (Get-Process -Id $procId -ErrorAction SilentlyContinue) {
        Write-Host "[$name] 超时，强制终止"
        Stop-Process -Id $procId -Force -ErrorAction SilentlyContinue
    }
    Remove-Item $pidFile -ErrorAction SilentlyContinue
    Write-Host "[$name] 已停止"
}

Stop-One 'nm-admind' 'run\nm-admind.pid'
Stop-One 'nmd' 'run\nmd.pid'
Write-Host '==> 全部已停止'
