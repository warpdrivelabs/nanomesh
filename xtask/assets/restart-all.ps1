# restart-all.ps1 - 先停后启。
Set-Location $PSScriptRoot
& "$PSScriptRoot\stop-all.ps1"
Start-Sleep -Seconds 2
& "$PSScriptRoot\start-all.ps1"
