# Installs the companion on a remote host over WinRM, using credentials passed in the
# environment (RDPAPP_HOST / RDPAPP_USER / RDPAPP_PASS / RDPAPP_EXE). Copies the binary
# in, then launches it as the logged-on user in their interactive session via a one-shot
# scheduled task (WinRM runs in session 0, which can't open the session's virtual
# channel). Prints RDPAPP_OK on success. Run with -NonInteractive.
$ErrorActionPreference = 'Stop'

$sec = ConvertTo-SecureString $env:RDPAPP_PASS -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential($env:RDPAPP_USER, $sec)
$opt = New-PSSessionOption -OpenTimeout 20000 -OperationTimeout 60000

$session = New-PSSession -ComputerName $env:RDPAPP_HOST -Credential $cred -Authentication Negotiate -SessionOption $opt
try {
  Invoke-Command -Session $session -ScriptBlock {
    New-Item -ItemType Directory -Force -Path 'C:\ProgramData\RdpApp' | Out-Null
  }
  Copy-Item -ToSession $session -Path $env:RDPAPP_EXE -Destination 'C:\ProgramData\RdpApp\rdp-companion.exe' -Force

  Invoke-Command -Session $session -ScriptBlock {
    $exe = 'C:\ProgramData\RdpApp\rdp-companion.exe'
    $me = whoami
    # A scheduled task owned by the logged-on user, run on demand, executes in that
    # user's interactive session — bridging out of WinRM's session 0.
    $action = New-ScheduledTaskAction -Execute $exe
    $principal = New-ScheduledTaskPrincipal -UserId $me -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    $task = New-ScheduledTask -Action $action -Principal $principal -Settings $settings
    Register-ScheduledTask -TaskName 'RdpAppCompanionBootstrap' -InputObject $task -Force | Out-Null
    Start-ScheduledTask -TaskName 'RdpAppCompanionBootstrap'
    Start-Sleep -Seconds 2
    Unregister-ScheduledTask -TaskName 'RdpAppCompanionBootstrap' -Confirm:$false
  }
  Write-Output 'RDPAPP_OK'
}
finally {
  Remove-PSSession $session
}
