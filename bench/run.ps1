# E2E round 1 runner. See bench/README.md and
# design/9_Scratch/2026-08-15_e2e-round-1-answer-key.md (the authority for
# prompts/pins/protocol).
#
#   .\run.ps1 -Model luna -Repo lore -Arm on -Task T4     # one cell
#   .\run.ps1 -Matrix                                     # everything
#   .\run.ps1 -Matrix -Models luna -Throttle 5            # luna only, 5-way parallel
#
# Matrix concurrency: read-only cells (T1-T4) run as parallel child pwsh
# processes, capped at -Throttle (each child owns its OPENCODE_CONFIG, so
# arms cannot cross-contaminate). T5 cells mutate working trees and qwen
# cells contend for the GPU — both always run serially, after the parallel
# wave.
#
# Results land in bench\results\<stamp>-<model>-<repo>-<arm>-<task>\.
param(
    [ValidateSet('luna', 'qwen')] [string]$Model,
    [ValidateSet('lore', 'terrarium', 'lexomancy')] [string]$Repo,
    [ValidateSet('on', 'off')] [string]$Arm,
    [ValidateSet('T1', 'T2', 'T3', 'T4', 'T5')] [string]$Task,
    [switch]$Matrix,
    [ValidateSet('luna', 'qwen')] [string[]]$Models = @('luna', 'qwen'),
    [ValidateRange(1, 16)] [int]$Throttle = 5
)

$ErrorActionPreference = 'Stop'
$benchRoot = $PSScriptRoot
$resultsRoot = Join-Path $benchRoot 'results'
New-Item -ItemType Directory -Force $resultsRoot | Out-Null

$modelMap = @{
    luna = @{ id = 'openai/gpt-5.6-luna'; variant = 'high' }
    qwen = @{ id = 'ollama/qwen3.8:latest'; variant = $null }
}
$repoMap = @{
    lore      = @{ dir = 'C:\Users\perag\bench-e2e\lore-bench'; vcs = 'git' }
    terrarium = @{ dir = 'C:\Users\perag\bench-e2e\terrarium-bench'; vcs = 'git' }
    lexomancy = @{ dir = 'C:\Users\perag\Unity\Lexomancy-bench'; vcs = 'cm'; cmDir = 'C:\Users\perag\Unity\Lexomancy-alt'; cmPin = 'cs:134' }
}
$prompts = Get-Content (Join-Path $benchRoot 'prompts.json') -Raw | ConvertFrom-Json

function Get-CmChanged([string]$cmDir) {
    Push-Location $cmDir
    try { (cm status --short 2>$null) | Where-Object { $_ -match '\S' } }
    finally { Pop-Location }
}

function Invoke-Cell([string]$model, [string]$repo, [string]$arm, [string]$task) {
    $m = $modelMap[$model]; $r = $repoMap[$repo]
    $prompt = $prompts.$repo.$task
    if (-not $prompt) { throw "no prompt for $repo/$task" }

    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $cell = "$stamp-$model-$repo-$arm-$task"
    $outDir = Join-Path $resultsRoot $cell
    New-Item -ItemType Directory -Force $outDir | Out-Null

    # Pre-state for T5 diff capture / reset.
    $preCm = if ($r.vcs -eq 'cm') { Get-CmChanged $r.cmDir } else { $null }

    $env:OPENCODE_CONFIG = Join-Path $benchRoot "opencode-$arm.jsonc"
    $args = @('run', '--dir', $r.dir, '-m', $m.id, '--format', 'json', '--title', $cell, '--auto')
    if ($m.variant) { $args += @('--variant', $m.variant) }
    $args += $prompt

    Write-Host "[$cell] running..." -ForegroundColor Cyan
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & opencode @args 1> (Join-Path $outDir 'events.jsonl') 2> (Join-Path $outDir 'stderr.log')
    $exit = $LASTEXITCODE
    $sw.Stop()
    Remove-Item Env:OPENCODE_CONFIG -ErrorAction SilentlyContinue

    # Parse the event stream: answer text, tokens, tool calls, session id.
    $tokens = @{ input = 0; output = 0; reasoning = 0; cache_read = 0; cache_write = 0 }
    $toolCalls = 0; $loreCalls = 0; $sessionId = $null
    $answer = [System.Text.StringBuilder]::new()
    foreach ($line in Get-Content (Join-Path $outDir 'events.jsonl') -ErrorAction SilentlyContinue) {
        $ev = try { $line | ConvertFrom-Json } catch { continue }
        if (-not $sessionId -and $ev.sessionID) { $sessionId = $ev.sessionID }
        switch -Wildcard ($ev.type) {
            'text' { [void]$answer.AppendLine($ev.part.text) }
            'step_finish' {
                $t = $ev.part.tokens
                if ($t) {
                    $tokens.input += $t.input; $tokens.output += $t.output
                    $tokens.reasoning += $t.reasoning
                    $tokens.cache_read += $t.cache.read; $tokens.cache_write += $t.cache.write
                }
            }
            'tool*' {
                $toolCalls++
                $name = if ($ev.part.tool) { $ev.part.tool } else { '' }
                if ($name -like 'lore*') { $loreCalls++ }
            }
        }
    }
    Set-Content (Join-Path $outDir 'answer.md') $answer.ToString()

    # T5: capture the diff, then restore the working tree.
    if ($task -eq 'T5') {
        if ($r.vcs -eq 'git') {
            git -C $r.dir add -N . 2>$null
            # git writes the file itself — piping through Set-Content rewrites
            # line endings and breaks `git apply`. Quoted as ONE argument:
            # bare `--output=(...)` splits at the paren in pwsh, git gets an
            # empty --output= and captures nothing (lost the 4 qwen T5 diffs
            # on 2026-08-16 before this fix).
            git -C $r.dir diff "--output=$(Join-Path $outDir 'diff.patch')"
            git -C $r.dir checkout -- . 2>$null
            git -C $r.dir clean -fd 2>$null
        } else {
            $postCm = Get-CmChanged $r.cmDir
            $newChanges = $postCm | Where-Object { $preCm -notcontains $_ }
            $newChanges | Set-Content (Join-Path $outDir 'cm-changed.txt')
            Push-Location $r.cmDir
            try {
                foreach ($line in $newChanges) {
                    # cm status --short lines end in the path; undo each new one.
                    $p = ($line -split '\s+')[-1]
                    if (-not $p) { continue }
                    # HARD RULE (Lexomancy CLAUDE.md / uvcs-hygiene): never
                    # `cm diff <path>` — it opens a blocking GUI window and cm
                    # has no textual hunk output. Pull the pinned revision and
                    # diff locally instead.
                    $base = Join-Path ([IO.Path]::GetTempPath()) ("t5base-" + [IO.Path]::GetFileName($p))
                    cm getfile "$p#$($r.cmPin)" --file="$base" 2>$null | Out-Null
                    if (-not (Test-Path $base)) { Set-Content $base '' }  # added file: empty base
                    $tmpDiff = Join-Path ([IO.Path]::GetTempPath()) 't5hunks.patch'
                    git diff --no-index --output=$tmpDiff -- $base $p 2>$null
                    if (Test-Path $tmpDiff) {
                        [IO.File]::AppendAllText((Join-Path $outDir 'diff.patch'), [IO.File]::ReadAllText($tmpDiff))
                        Remove-Item $tmpDiff
                    }
                    Remove-Item $base -ErrorAction SilentlyContinue
                    cm undo $p 2>$null | Out-Null
                }
            } finally { Pop-Location }
            if ($newChanges) { Write-Warning "[$cell] cm changes captured and undone: $($newChanges.Count) file(s). Verify with 'cm status'." }
        }
    }

    # Compaction time from opencode's session row (qwen 128k protocol).
    $compacting = $null
    if ($sessionId) {
        $compacting = python -c "import sqlite3;print(sqlite3.connect(r'C:/Users/perag/.local/share/opencode/opencode.db').execute('select time_compacting from session where id=?',('$sessionId',)).fetchone()[0])" 2>$null
    }

    $metrics = [ordered]@{
        cell = $cell; model = $m.id; repo = $repo; arm = $arm; task = $task
        wall_ms = $sw.ElapsedMilliseconds; exit_code = $exit
        tokens = $tokens; tool_calls = $toolCalls; lore_calls = $loreCalls
        session_id = $sessionId; time_compacting = $compacting
        score = $null  # graded by hand against the answer key
    }
    $metrics | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $outDir 'metrics.json')
    Write-Host ("[$cell] done in {0:n0}s  tokens in/out {1}/{2}  tools {3} (lore {4})" -f
        ($sw.ElapsedMilliseconds / 1000), $tokens.input, $tokens.output, $toolCalls, $loreCalls)
}

if ($Matrix) {
    $total = [System.Diagnostics.Stopwatch]::StartNew()
    $cells = foreach ($mo in $Models) {
        foreach ($re in 'lore', 'terrarium', 'lexomancy') {
            foreach ($ar in 'off', 'on') {
                foreach ($ta in 'T1', 'T2', 'T3', 'T4', 'T5') {
                    [pscustomobject]@{ Model = $mo; Repo = $re; Arm = $ar; Task = $ta }
                }
            }
        }
    }
    $parallel = @($cells | Where-Object { $_.Model -eq 'luna' -and $_.Task -ne 'T5' })
    $serial = @($cells | Where-Object { $_.Model -ne 'luna' -or $_.Task -eq 'T5' })

    $procs = @()
    foreach ($c in $parallel) {
        while (@($procs | Where-Object { -not $_.HasExited }).Count -ge $Throttle) {
            Start-Sleep -Seconds 3
        }
        $log = Join-Path $resultsRoot "launch-$($c.Model)-$($c.Repo)-$($c.Arm)-$($c.Task).log"
        Write-Host "[matrix] launching $($c.Model)/$($c.Repo)/$($c.Arm)/$($c.Task)" -ForegroundColor DarkCyan
        $procs += Start-Process pwsh -PassThru -WindowStyle Hidden -RedirectStandardOutput $log `
            -ArgumentList '-NoProfile', '-File', $PSCommandPath,
            '-Model', $c.Model, '-Repo', $c.Repo, '-Arm', $c.Arm, '-Task', $c.Task
    }
    $procs | ForEach-Object { $_.WaitForExit() }
    $failed = @($procs | Where-Object { $_.ExitCode -ne 0 }).Count
    if ($failed) { Write-Warning "[matrix] $failed parallel cell(s) exited non-zero — check launch-*.log" }

    foreach ($c in $serial) {
        Invoke-Cell $c.Model $c.Repo $c.Arm $c.Task
    }
    $total.Stop()
    Write-Host ("[matrix] {0} cells in {1:n1} min ({2} parallel @ {3}, {4} serial)" -f
        $cells.Count, $total.Elapsed.TotalMinutes, $parallel.Count, $Throttle, $serial.Count) -ForegroundColor Green
} else {
    if (-not ($Model -and $Repo -and $Arm -and $Task)) {
        throw 'Provide -Model -Repo -Arm -Task, or -Matrix.'
    }
    Invoke-Cell $Model $Repo $Arm $Task
}
