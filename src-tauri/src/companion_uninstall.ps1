# Removes the companion from a host over WinRM (RDPAPP_HOST / RDPAPP_USER / RDPAPP_PASS):
# stops the process, removes its logon autostart entry, and deletes its files.
$ErrorActionPreference = 'Stop'

$sec = ConvertTo-SecureString $env:RDPAPP_PASS -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential($env:RDPAPP_USER, $sec)
$session = New-PSSession -ComputerName $env:RDPAPP_HOST -Credential $cred -Authentication Negotiate
try {
  Invoke-Command -Session $session -ScriptBlock {
    Get-Process rdp-companion -ErrorAction SilentlyContinue | Stop-Process -Force
    Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'RdpAppCompanion' -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force 'C:\ProgramData\RdpApp' -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force (Join-Path $env:LOCALAPPDATA 'RdpApp') -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName 'RdpAppCompanionBootstrap' -Confirm:$false -ErrorAction SilentlyContinue
  }
}
finally {
  Remove-PSSession $session
}
