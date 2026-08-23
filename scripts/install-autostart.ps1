# Register logon auto-start for the lore daemon and the embedding server.
#
# Fixes the reboot papercut: after a restart neither the daemon nor
# llama-server comes back, so the first `lore status` reports the daemon gone
# (or embeddings UNREACHABLE) until both are started by hand.
#
# Registers two current-user scheduled tasks (no admin required):
#   Lore Daemon      -> %USERPROFILE%\.cargo\bin\lore.exe daemon
#   Lore Embeddings  -> scripts\serve-embeddings-vllm.ps1 (vLLM FP8 in WSL2)
#                       or scripts\serve-embeddings.ps1 with -LlamaCpp
#
# Both run hidden at logon with no execution time limit. The daemon's
# single-owner handshake makes a double start harmless (the second instance
# refuses), so re-running the tasks manually is safe.
#
# Re-running this script replaces the existing tasks (idempotent).
# Remove both with: .\install-autostart.ps1 -Uninstall
param(
    [switch]$Uninstall,
    # Register the retired llama.cpp embedding stack instead of the vLLM one.
    [switch]$LlamaCpp
)
$ErrorActionPreference = 'Stop'

$daemonTask = 'Lore Daemon'
$embedTask = 'Lore Embeddings'

if ($Uninstall) {
    foreach ($name in @($daemonTask, $embedTask)) {
        try {
            Unregister-ScheduledTask -TaskName $name -Confirm:$false -ErrorAction Stop
            Write-Host "removed task: $name"
        } catch {
            Write-Host "not registered: $name"
        }
    }
    return
}

$loreExe = Join-Path $env:USERPROFILE '.cargo\bin\lore.exe'
if (-not (Test-Path $loreExe)) {
    throw "$loreExe not found - install it with: cargo install --path crates/lore"
}
# vLLM in WSL2 (FP8) replaced the llama.cpp Q8_0 stack on 2026-08-17: same
# model, ~2.1x throughput. Pass -LlamaCpp to register the old stack instead.
$serveScript = Join-Path $PSScriptRoot ($LlamaCpp ? 'serve-embeddings.ps1' : 'serve-embeddings-vllm.ps1')
if (-not (Test-Path $serveScript)) { throw "$serveScript not found" }
# Bare name, resolved by Task Scheduler at *run* time. `(Get-Command pwsh).Source`
# bakes in the version-stamped Store path
# (…\WindowsApps\Microsoft.PowerShell_7.6.4.0_x64__…\pwsh.exe), which the next
# PowerShell update invalidates — and a logon task whose Execute no longer
# exists fails with 0x80070002 into the task history and nowhere else, so the
# first symptom is `lore status` saying the daemon is not running weeks later.
$pwshExe = 'pwsh.exe'
if (-not (Get-Command $pwshExe -ErrorAction SilentlyContinue)) { throw 'pwsh.exe is not on PATH' }

# No -ExecutionTimeLimit of the 72h default: both processes are meant to run
# for the whole session.
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Seconds 0)
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"

# A hidden pwsh parent gives the console child a hidden console too; launching
# lore.exe directly would flash a visible console window at every logon.
$daemonAction = New-ScheduledTaskAction -Execute $pwshExe `
    -Argument "-NoProfile -WindowStyle Hidden -Command `"& '$loreExe' daemon`""
$embedAction = New-ScheduledTaskAction -Execute $pwshExe `
    -Argument "-NoProfile -WindowStyle Hidden -File `"$serveScript`""

Register-ScheduledTask -TaskName $daemonTask -Action $daemonAction `
    -Trigger $trigger -Settings $settings -Force | Out-Null
Write-Host "registered task: $daemonTask ($loreExe daemon)"
Register-ScheduledTask -TaskName $embedTask -Action $embedAction `
    -Trigger $trigger -Settings $settings -Force | Out-Null
Write-Host "registered task: $embedTask ($serveScript)"
Write-Host 'both start hidden at next logon; remove with -Uninstall'
