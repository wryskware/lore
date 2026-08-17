# E2E bench runner. See bench/README.md,
# design/6_Evaluation/2026-08-15_e2e-round-1-answer-key.md (prompts/pins/
# protocol) and .../2026-08-17_e2e-round-1-key-addendum.md (grading Rev A).
#
#   .\run.ps1 -Model luna -Repo lore -Arm on -Task T4     # one cell
#   .\run.ps1 -Matrix                                     # everything
#   .\run.ps1 -Matrix -Models luna -Throttle 5            # luna only, 5-way parallel
#   .\run.ps1 -Matrix -Models luna -Arms on               # round-2 shape: 15 on-arm cells
#
# WORKING TREES — one per (repo, arm). Each arm owns its own registered lore
# project, so a T5 cell that edits files can never be seen by the other arm.
# Slot 'a' is the round-1 tree (already registered); slot 'b' is the second one
# created by setup-worktrees.ps1. Arms map on->a, off->b, fixed, so a cell's
# tree is a pure function of (repo, arm) and two cells share a tree only if
# they share both.
#
# Matrix concurrency, three waves:
#   1. luna T1-T4  — parallel, capped at -Throttle. Read-only.
#   2. luna T5     — parallel, capped at -Throttle. Every T5 cell has a
#                    distinct (repo, arm) and therefore a distinct tree. Kept
#                    out of wave 1 because a T5 write would otherwise land
#                    under a T1-T4 cell reading the same tree.
#   3. everything else (qwen) — serial; qwen cells contend for the GPU.
# Each child pwsh owns its own OPENCODE_CONFIG, so arms cannot cross-
# contaminate.
#
# Results land in bench\results\<stamp>-<model>-<repo>-<arm>-<task>\.
param(
    [ValidateSet('luna', 'qwen')] [string]$Model,
    [ValidateSet('lore', 'terrarium', 'lexomancy')] [string]$Repo,
    [ValidateSet('on', 'off')] [string]$Arm,
    [ValidateSet('T1', 'T2', 'T3', 'T4', 'T5')] [string]$Task,
    [switch]$Matrix,
    [ValidateSet('luna', 'qwen')] [string[]]$Models = @('luna', 'qwen'),
    # Round 2 is scoped on-arm-only (15 cells): `-Matrix -Arms on`.
    [ValidateSet('on', 'off')] [string[]]$Arms = @('off', 'on'),
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

# One tree per (repo, slot). Slot 'a' is the round-1 tree, already registered
# with the daemon under the name in `project`; slot 'b' is the second one, so
# the two arms of a T5 cell never share files. Keep in sync with
# setup-worktrees.ps1 — it is what creates and registers slot 'b'.
$repoMap = @{
    lore      = @{
        vcs   = 'git'
        slots = @{
            a = @{ dir = 'C:\Users\perag\bench-e2e\lore-bench'; project = 'lore-bench' }
            b = @{ dir = 'C:\Users\perag\bench-e2e\lore-bench-b'; project = 'lore-bench-b' }
        }
    }
    terrarium = @{
        vcs   = 'git'
        slots = @{
            a = @{ dir = 'C:\Users\perag\bench-e2e\terrarium-bench'; project = 'terrarium-bench' }
            b = @{ dir = 'C:\Users\perag\bench-e2e\terrarium-bench-b'; project = 'terrarium-bench-b' }
        }
    }
    # Lexomancy retrieval targets the main `Lexomancy` root for BOTH slots (the
    # walker does not follow the junctions, so the bench roots index ~nothing).
    # The main root is frozen at the pin and read-only during runs, so sharing
    # it is safe. What must not be shared is the cm workspace the T5 cell edits
    # — hence a second one per slot.
    lexomancy = @{
        vcs   = 'cm'
        cmPin = 'cs:134'
        slots = @{
            a = @{ dir = 'C:\Users\perag\Unity\Lexomancy-bench'; project = 'Lexomancy'; cmDir = 'C:\Users\perag\Unity\Lexomancy-alt' }
            b = @{ dir = 'C:\Users\perag\Unity\Lexomancy-bench-b'; project = 'Lexomancy'; cmDir = 'C:\Users\perag\Unity\Lexomancy-alt-b' }
        }
    }
}

# Arm -> slot. Fixed, so a tree is a pure function of (repo, arm).
$armSlot = @{ on = 'a'; off = 'b' }

# The pinned lore-mcp binary. Copied out of a build by setup-worktrees.ps1 so a
# round is not silently re-pinned by whatever was last built in the live
# checkout. Keep in sync with opencode-{on,off}.jsonc.
$loreMcpExe = 'C:\Users\perag\bench-e2e\bin\lore-mcp.exe'

# Untracked files the daemon generates inside a registered root. They must be
# kept out of T5 diffs AND must survive the post-T5 reset: deleting .loreignore
# mid-round silently changes what the project indexes.
$daemonArtifacts = @('.lore.toml', '.loreignore')

# Corpus scrub. These paths exist at the pin and LEAK THE ANSWER KEY into the
# corpus under test: the round-1 plan doc carries the task list and the graded
# answers for all three repos. setup-worktrees.ps1 deletes them; this script
# refuses to run a cell while one is present, and the T5 reset below must not
# resurrect them (`git checkout -- .` restores a deleted tracked file), so they
# are excluded from the staged diff and from the restoring checkout the same way
# the daemon artifacts are. See design/6_Evaluation/2026-08-17_e2e-round-2-task-set.md
# § "Corpus scrub".
$scrubbed = @{
    lore      = @('design/9_Scratch/2026-08-15_e2e-round-1-plan.md')
    terrarium = @()
    lexomancy = @()
}

$promptsPath = Join-Path $benchRoot 'prompts.json'
$prompts = Get-Content $promptsPath -Raw | ConvertFrom-Json

function Get-CmChanged([string]$cmDir) {
    Push-Location $cmDir
    try { (cm status --short 2>$null) | Where-Object { $_ -match '\S' } }
    finally { Pop-Location }
}

function Invoke-Cell([string]$model, [string]$repo, [string]$arm, [string]$task) {
    $m = $modelMap[$model]; $r = $repoMap[$repo]
    $slot = $armSlot[$arm]
    $s = $r.slots[$slot]
    $prompt = $prompts.$repo.$task
    if (-not $prompt) { throw "no prompt for $repo/$task" }
    $scrub = @($scrubbed[$repo])

    # Preflight. A missing tree means setup-worktrees.ps1 has not been run (or
    # not for this slot); failing here beats running the cell against nothing.
    if (-not (Test-Path -LiteralPath $s.dir)) {
        throw "$repo/$arm expects slot '$slot' at $($s.dir), which does not exist. Run bench\setup-worktrees.ps1 first."
    }
    if ($r.vcs -eq 'cm' -and -not (Test-Path -LiteralPath $s.cmDir)) {
        throw "$repo/$arm expects the cm workspace $($s.cmDir), which does not exist. See bench\setup-worktrees.ps1."
    }
    if (-not (Test-Path -LiteralPath $loreMcpExe)) {
        throw "pinned lore-mcp binary missing: $loreMcpExe. Run bench\setup-worktrees.ps1 -PinBinary."
    }
    # Answer-key material must not be inside the corpus under test.
    foreach ($rel in $scrub) {
        if (Test-Path -LiteralPath (Join-Path $s.dir $rel)) {
            throw "$repo/$arm : '$rel' is present in $($s.dir) and leaks the answer key into the corpus. Run bench\setup-worktrees.ps1 -Apply -Scrub."
        }
    }

    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $cell = "$stamp-$model-$repo-$arm-$task"
    $outDir = Join-Path $resultsRoot $cell
    New-Item -ItemType Directory -Force $outDir | Out-Null

    # Pre-state for T5 diff capture / reset.
    $preCm = if ($r.vcs -eq 'cm') { Get-CmChanged $s.cmDir } else { $null }

    $env:OPENCODE_CONFIG = Join-Path $benchRoot "opencode-$arm.jsonc"
    # Not `$args`: that is an automatic variable, and shadowing it inside a
    # function is the kind of quiet weirdness this harness has already been
    # bitten by once.
    $ocArgs = @('run', '--dir', $s.dir, '-m', $m.id, '--format', 'json', '--title', $cell, '--auto')
    if ($m.variant) { $ocArgs += @('--variant', $m.variant) }
    $ocArgs += $prompt

    Write-Host "[$cell] running..." -ForegroundColor Cyan
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & opencode @ocArgs 1> (Join-Path $outDir 'events.jsonl') 2> (Join-Path $outDir 'stderr.log')
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
            # Daemon-generated files are not agent work. Excluded from the
            # staging/diff via pathspecs and from the clean via -e, so the
            # reset cannot delete the project's .loreignore out from under the
            # daemon mid-round.
            # Scrubbed paths join the daemon artifacts here for a different
            # reason: they are DELETED tracked files, so without the exclusion
            # the diff would carry a spurious deletion hunk AND the restoring
            # checkout below would put the answer key back into the corpus for
            # every later cell in this tree.
            $exclude = @(($daemonArtifacts + $scrub) | ForEach-Object { ":(exclude)$_" })
            git -C $s.dir add -N -- . @exclude 2>$null
            # git writes the file itself — piping through Set-Content rewrites
            # line endings and breaks `git apply`. Quoted as ONE argument:
            # bare `--output=(...)` splits at the paren in pwsh, git gets an
            # empty --output= and captures nothing (lost the 4 qwen T5 diffs
            # on 2026-08-16 before this fix).
            git -C $s.dir diff "--output=$(Join-Path $outDir 'diff.patch')" -- . @exclude
            git -C $s.dir checkout -- . @exclude 2>$null
            $keep = @($daemonArtifacts | ForEach-Object { '-e'; $_ })
            git -C $s.dir clean -fd @keep 2>$null
        } else {
            $postCm = Get-CmChanged $s.cmDir
            $newChanges = $postCm | Where-Object { $preCm -notcontains $_ }
            $newChanges | Set-Content (Join-Path $outDir 'cm-changed.txt')
            Push-Location $s.cmDir
            try {
                foreach ($line in $newChanges) {
                    # cm status --short lines end in the path; undo each new one.
                    $p = ($line -split '\s+')[-1]
                    if (-not $p) { continue }
                    # HARD RULE (Lexomancy CLAUDE.md / uvcs-hygiene): never
                    # `cm diff <path>` — it opens a blocking GUI window and cm
                    # has no textual hunk output. Pull the pinned revision and
                    # diff locally instead.
                    # Temp names carry the cell: two T5 arms run concurrently
                    # now, and a shared `t5hunks.patch` would have them
                    # overwrite each other's hunks.
                    $base = Join-Path ([IO.Path]::GetTempPath()) ("t5base-$cell-" + [IO.Path]::GetFileName($p))
                    cm getfile "$p#$($r.cmPin)" --file="$base" 2>$null | Out-Null
                    if (-not (Test-Path $base)) { Set-Content $base '' }  # added file: empty base
                    $tmpDiff = Join-Path ([IO.Path]::GetTempPath()) "t5hunks-$cell.patch"
                    # Same one-argument quoting as the git-repo path above; an
                    # unquoted --output=$var is what silently ate round 1's
                    # diffs when the expansion contained a paren.
                    git diff --no-index "--output=$tmpDiff" -- $base $p 2>$null
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

    # Record what the cell actually ran against: which tree, which registered
    # project, and which lore-mcp build. The hash is the real pin — the path
    # alone would not notice a rebuild between cells.
    $exeInfo = Get-Item -LiteralPath $loreMcpExe
    $metrics = [ordered]@{
        cell = $cell; model = $m.id; repo = $repo; arm = $arm; task = $task
        slot = $slot; dir = $s.dir; project = $s.project
        # Which task set this cell was asked. Prompts are no longer frozen
        # across rounds, so a results dir that cannot name its prompt cannot be
        # attributed to a key. The hash is the real identity; the id is for eyes.
        task_set = $prompts._task_set
        prompt_sha256 = (Get-FileHash -InputStream (
                [IO.MemoryStream]::new([Text.Encoding]::UTF8.GetBytes($prompt))
            ) -Algorithm SHA256).Hash
        lore_mcp = [ordered]@{
            path     = $loreMcpExe
            sha256   = (Get-FileHash -LiteralPath $loreMcpExe -Algorithm SHA256).Hash
            modified = $exeInfo.LastWriteTimeUtc.ToString('o')
        }
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
            foreach ($ar in $Arms) {
                foreach ($ta in 'T1', 'T2', 'T3', 'T4', 'T5') {
                    [pscustomobject]@{ Model = $mo; Repo = $re; Arm = $ar; Task = $ta }
                }
            }
        }
    }
    # Wave 1: luna read-only. Wave 2: luna T5 — safe to parallelise because
    # every T5 cell has a distinct (repo, arm) and therefore a distinct tree,
    # but held out of wave 1 so no T5 write lands under a reading cell in the
    # same tree. Wave 3: qwen, serial (GPU).
    $readOnly = @($cells | Where-Object { $_.Model -eq 'luna' -and $_.Task -ne 'T5' })
    $writes = @($cells | Where-Object { $_.Model -eq 'luna' -and $_.Task -eq 'T5' })
    $serial = @($cells | Where-Object { $_.Model -ne 'luna' })

    # Assert the invariant the parallel waves rest on, rather than trusting it.
    foreach ($wave in @($readOnly, $writes)) {
        $dupes = $wave | Group-Object { "$($_.Repo)/$($_.Arm)/$($_.Task)" } |
            Where-Object Count -gt 1
        if ($dupes) { throw "[matrix] duplicate cells in a parallel wave: $($dupes.Name -join ', ')" }
    }
    $treeClash = $writes | Group-Object { "$($_.Repo)/$($_.Arm)" } | Where-Object Count -gt 1
    if ($treeClash) { throw "[matrix] two T5 cells would share a tree: $($treeClash.Name -join ', ')" }

    function Start-Wave([object[]]$wave, [string]$label) {
        if (-not $wave) { return }
        Write-Host "[matrix] wave '$label': $($wave.Count) cell(s) @ throttle $Throttle" -ForegroundColor DarkCyan
        $procs = @()
        foreach ($c in $wave) {
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
        if ($failed) { Write-Warning "[matrix] wave '$label': $failed cell(s) exited non-zero — check launch-*.log" }
    }

    Start-Wave $readOnly 'luna T1-T4'
    Start-Wave $writes 'luna T5'

    foreach ($c in $serial) {
        Invoke-Cell $c.Model $c.Repo $c.Arm $c.Task
    }
    $total.Stop()
    Write-Host ("[matrix] {0} cells in {1:n1} min ({2} read-only + {3} T5 parallel @ {4}, {5} serial)" -f
        $cells.Count, $total.Elapsed.TotalMinutes, $readOnly.Count, $writes.Count,
        $Throttle, $serial.Count) -ForegroundColor Green
} else {
    if (-not ($Model -and $Repo -and $Arm -and $Task)) {
        throw 'Provide -Model -Repo -Arm -Task, or -Matrix.'
    }
    Invoke-Cell $Model $Repo $Arm $Task
}
