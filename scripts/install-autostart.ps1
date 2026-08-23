# Register logon auto-start for lore.
#
# Fixes the reboot papercut: after a restart nothing comes back, so the first
# `lore status` reports the daemon gone (or embeddings UNREACHABLE) until both
# are started by hand.
#
# Registers one current-user scheduled task (no admin required):
#   Lore  ->  %USERPROFILE%\.cargo\bin\lore.exe start
#
# One task rather than two, because `lore start` already does both in the right
# order: it launches the embedding server named by `[embeddings] start_command`
# in the daemon's config.toml, waits for it to answer, and only then starts the
# daemon — so the daemon's first probe finds a live endpoint instead of backing
# off for up to a minute. Two independent logon tasks raced by construction.
#
# Which embedding server that is now lives in config.toml, not in this script.
# The vLLM launcher is scripts\serve-embeddings-vllm.ps1 and the retired
# llama.cpp one is scripts\serve-embeddings.ps1; point `start_command` at
# whichever you want. See the README's "Semantic search" section.
#
# Runs hidden at logon with no execution time limit. `lore start` is
# idempotent and the daemon's single-owner handshake makes a double start
# harmless, so re-running the task by hand is safe.
#
# Re-running this script replaces the existing task (idempotent).
# Remove with: .\install-autostart.ps1 -Uninstall
param(
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'

$task = 'Lore'
# The two tasks this replaces, cleaned up on both paths so an older install
# does not leave a second daemon starter behind racing the new one.
$retired = @('Lore Daemon', 'Lore Embeddings')

if ($Uninstall) {
    foreach ($name in @($task) + $retired) {
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
# A *version-stable* path to pwsh, which is the whole point of this list.
# `(Get-Command pwsh).Source` resolves the Store install to its versioned
# package directory (…\WindowsApps\Microsoft.PowerShell_7.6.4.0_x64__…\pwsh.exe),
# and the next PowerShell update deletes that directory. A logon task whose
# Execute no longer exists fails with 0x80070002 into the task history and
# nowhere else, so the first symptom is `lore status` reporting the daemon gone,
# weeks later, for no visible reason. That is exactly what happened here.
#
# The bare name `pwsh.exe` is not the fix: Task Scheduler resolves Execute
# against the system PATH, which does not carry the per-user WindowsApps
# directory, and fails the same 0x80070002 way (verified 2026-08-22).
#
# In preference order: the MSI install, then the Store's *alias* path, which is
# a per-user execution alias that survives package updates and which Task
# Scheduler runs happily (also verified).
$pwshCandidates = @(
    (Join-Path $env:ProgramFiles 'PowerShell\7\pwsh.exe'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft\WindowsApps\pwsh.exe')
)
$pwshExe = $pwshCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $pwshExe) {
    # Last resort, and warned about, because this is the path that rots.
    $pwshExe = (Get-Command pwsh -ErrorAction SilentlyContinue).Source
    if (-not $pwshExe) { throw 'pwsh not found' }
    Write-Warning "no version-stable pwsh path found; registering $pwshExe, which a PowerShell update may invalidate - re-run this script if the task starts failing"
}

# Warned about rather than written: config.toml is hand-owned, and a script
# that edits it would be guessing at which embedding stack you meant.
$config = Join-Path $env:LOCALAPPDATA 'lore\config.toml'
if (-not ((Test-Path $config) -and (Select-String -Path $config -Pattern '^\s*start_command' -Quiet))) {
    Write-Warning "no [embeddings] start_command in $config - the daemon will start at logon but the embedding server will not, and search stays lexical-only until you start it by hand"
}

# No -ExecutionTimeLimit of the 72h default. `lore start` exits in seconds once
# the daemon answers, but it will wait several minutes for a cold embedding
# server first, and the daemon it leaves behind runs for the whole session.
$settings = New-ScheduledTaskSettingsSet `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -StartWhenAvailable -ExecutionTimeLimit (New-TimeSpan -Seconds 0)
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"

# A hidden pwsh parent gives the console child a hidden console too; launching
# lore.exe directly would flash a visible console window at every logon.
$action = New-ScheduledTaskAction -Execute $pwshExe `
    -Argument "-NoProfile -WindowStyle Hidden -Command `"& '$loreExe' start`""

foreach ($name in $retired) {
    if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $name -Confirm:$false
        Write-Host "removed superseded task: $name"
    }
}
Register-ScheduledTask -TaskName $task -Action $action `
    -Trigger $trigger -Settings $settings -Force | Out-Null
Write-Host "registered task: $task ($loreExe start)"
Write-Host 'starts hidden at next logon; remove with -Uninstall'
