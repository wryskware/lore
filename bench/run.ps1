# E2E bench runner. See bench/README.md,
# design/6_Evaluation/2026-08-15_e2e-round-1-answer-key.md (prompts/pins/
# protocol) and .../2026-08-17_e2e-round-1-key-addendum.md (grading Rev A).
#
#   .\run.ps1 -Model luna -Repo lore -Arm on -Task T4     # one cell
#   .\run.ps1 -Matrix                                     # everything
#   .\run.ps1 -Matrix -Models luna -Throttle 5            # luna only, 5-way parallel
#   .\run.ps1 -Matrix -Models luna -Arms on               # on-arm-only: 15 cells
#   .\run.ps1 -Matrix -Models luna -Repos lore,terrarium  # two arms, no Lexomancy
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
    [ValidateSet('luna', 'lunamax', 'qwen')] [string]$Model,
    [ValidateSet('lore', 'terrarium', 'lexomancy')] [string]$Repo,
    [ValidateSet('on', 'off')] [string]$Arm,
    [ValidateSet('T1', 'T2', 'T3', 'T4', 'T5')] [string]$Task,
    [switch]$Matrix,
    # Difficulty calibration for the two round-2 tasks whose difficulty is
    # argued rather than observed. Runs both arms, luna only. See the § Pilot
    # block below for why it is four cells and not fifteen.
    [switch]$Pilot,
    [ValidateSet('luna', 'lunamax', 'qwen')] [string[]]$Models = @('luna', 'qwen'),
    # Round 2 is scoped on-arm-only (15 cells): `-Matrix -Arms on`.
    [ValidateSet('on', 'off')] [string[]]$Arms = @('off', 'on'),
    # Matrix scope filters. A two-arm round runs lore+terrarium through the
    # matrix and Lexomancy by hand, because Lexomancy has exactly one cm
    # workspace (`Lexomancy-alt`) and its slot 'b' has never existed: see
    # README § Two trees per repo. `-Repos lore,terrarium` then
    # `-Repos lexomancy -Slot a -Tasks T1,T2,T3,T4` covers everything but the
    # two Lexomancy T5 cells, which are run one at a time.
    [ValidateSet('lore', 'terrarium', 'lexomancy')] [string[]]$Repos = @('lore', 'terrarium', 'lexomancy'),
    [ValidateSet('T1', 'T2', 'T3', 'T4', 'T5')] [string[]]$Tasks = @('T1', 'T2', 'T3', 'T4', 'T5'),
    # Overrides the arm -> slot mapping for this cell. `-Pilot` passes 'a' so
    # both arms read the round-1 tree and slot 'b' need not exist; the matrix
    # never sets it and keeps the fixed mapping. Not for general use.
    [ValidateSet('a', 'b')] [string]$Slot,
    [ValidateRange(1, 16)] [int]$Throttle = 5,
    # Seconds between child launches. See Start-Wave for why this is not zero.
    [ValidateRange(0, 60)] [int]$LaunchStaggerSeconds = 6
)

$ErrorActionPreference = 'Stop'
# Exit code of the cell this process ran, surfaced to the parent wave.
$script:cellExit = 0
# Cells that exited non-zero across every wave of a matrix run.
$script:waveFailures = 0
$benchRoot = $PSScriptRoot
$resultsRoot = Join-Path $benchRoot 'results'
New-Item -ItemType Directory -Force $resultsRoot | Out-Null

$modelMap = @{
    luna    = @{ id = 'openai/gpt-5.6-luna'; variant = 'high' }
    lunamax = @{ id = 'openai/gpt-5.6-luna'; variant = 'max' }
    qwen    = @{ id = 'ollama/qwen3.8:latest'; variant = $null }
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
    # Lexomancy retrieval targets the bench root's OWN project, one per slot.
    # This replaces the round-2 arrangement, where both slots retrieved from the
    # main `Lexomancy` root because the walker does not follow the junctions and
    # the bench roots indexed three loose files. Under D-0022 the junction
    # targets are declared as `[[sources]]` in the bench root's `.lore.toml`, so
    # `Lexomancy-bench` now indexes the corpus under test directly — including
    # `Lexomancy-alt`, the cm checkout the T5 cell actually edits, rather than
    # the live tree, which drifts. Result paths are mount-relative and therefore
    # openable from the bench tree through the junctions.
    lexomancy = @{
        vcs   = 'cm'
        cmPin = 'cs:134'
        slots = @{
            a = @{ dir = 'C:\Users\perag\Unity\Lexomancy-bench'; project = 'Lexomancy-bench'; cmDir = 'C:\Users\perag\Unity\Lexomancy-alt' }
            b = @{ dir = 'C:\Users\perag\Unity\Lexomancy-bench-b'; project = 'Lexomancy-bench-b'; cmDir = 'C:\Users\perag\Unity\Lexomancy-alt-b' }
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

# Suite per repo, run by the harness on a T5 cell while the diff is still
# applied. Commands are the ones the task set names as the grading suites.
# Lexomancy is absent on purpose: its suite is an EditMode run against an
# editor the grader keeps open, which the harness cannot drive, and inventing
# a headless approximation would be a different measurement.
$suiteCommand = @{
    lore      = @{ exe = 'cargo'; args = @('test', '--workspace'); workdir = '.'; display = 'cargo test --workspace' }
    terrarium = @{
        exe = 'uv'; args = @('run', '--extra', 'dev', '--extra', 'server', 'pytest', '-q')
        workdir = 'analysis'; display = 'uv run --extra dev --extra server pytest -q  (in analysis/)'
    }
}

function Get-CmChanged([string]$cmDir) {
    Push-Location $cmDir
    try { (cm status --short 2>$null) | Where-Object { $_ -match '\S' } }
    finally { Pop-Location }
}

function Invoke-Cell([string]$model, [string]$repo, [string]$arm, [string]$task) {
    $m = $modelMap[$model]; $r = $repoMap[$repo]
    # `-Slot` overrides the fixed arm -> slot mapping. Only `-Pilot` sets it,
    # and only for read-only tasks, where both arms sharing a tree is safe.
    $slot = if ($Slot) { $Slot } else { $armSlot[$arm] }
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
    # Pin the MCP server's scope to the project this cell claims to retrieve
    # from, instead of letting it resolve from the cwd. For lore and terrarium
    # the two agree; pinning makes the record true rather than incidental, and
    # keeps a cell from silently retrieving from a neighbouring registration if
    # cwd resolution ever changes again.
    # It matters most for Lexomancy, whose cwd is a junction tree: cwd
    # resolution would land on whichever registration owns that path, while the
    # cell means the bench project that declares the mounts as sources.
    # Harmless on the off arm, which has no MCP server at all.
    $env:LORE_PROJECT = $s.project
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
    Remove-Item Env:LORE_PROJECT -ErrorAction SilentlyContinue

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

    # T5: capture the diff, run the suite while the change is still applied,
    # then restore the working tree.
    #
    # The suite runs HERE and nowhere else. Grading needs "suite green at the
    # pin", the reset is seconds away, and once it lands the tree no longer
    # contains the change — so a grader asked for a suite result afterwards has
    # to re-apply the diff to a tree that has since served other cells. Round 2
    # graded four T5 cells at a provisional 0.5 for exactly this reason. It is
    # captured, never scored by the harness: `suite-result.txt` records the
    # command, the exit code and the tail of the output, and a human or a
    # grader reads it.
    if ($task -eq 'T5') {
        $suiteCmd = $suiteCommand[$repo]
        if ($suiteCmd) {
            Write-Host "[$cell] suite: $($suiteCmd.display)" -ForegroundColor DarkCyan
            $suiteOut = Join-Path $outDir 'suite-result.txt'
            $sw2 = [System.Diagnostics.Stopwatch]::StartNew()
            Push-Location (Join-Path $s.dir $suiteCmd.workdir)
            try {
                $output = & $suiteCmd.exe @($suiteCmd.args) 2>&1 | Out-String
                $suiteExit = $LASTEXITCODE
            }
            catch {
                $output = "harness failed to run the suite: $_"
                $suiteExit = -1
            }
            finally { Pop-Location }
            $sw2.Stop()
            @(
                "command: $($suiteCmd.display)",
                "cwd: $(Join-Path $s.dir $suiteCmd.workdir)",
                "exit: $suiteExit  ($([math]::Round($sw2.Elapsed.TotalSeconds))s)",
                "verdict: $(if ($suiteExit -eq 0) { 'GREEN' } else { 'RED' })",
                '',
                '--- output ---',
                $output.TrimEnd()
            ) | Set-Content -LiteralPath $suiteOut -Encoding utf8
            Write-Host "[$cell] suite $(if ($suiteExit -eq 0) { 'GREEN' } else { "RED (exit $suiteExit)" })" -ForegroundColor $(if ($suiteExit -eq 0) { 'Green' } else { 'Red' })
        }

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
    # A child process must exit non-zero when its cell did, or Start-Wave's
    # ExitCode check silently passes a wave in which cells died. Round-3
    # Lexomancy wave 1 lost three cells to an opencode lock and still reported
    # exit 0.
    $script:cellExit = $exit
    Write-Host ("[$cell] done in {0:n0}s  tokens in/out {1}/{2}  tools {3} (lore {4})" -f
        ($sw.ElapsedMilliseconds / 1000), $tokens.input, $tokens.output, $toolCalls, $loreCalls)
}

# Launches a set of cells as child processes, throttled, and waits for all of
# them. `$extraArgs` is splatted into every child's argument list — `-Pilot`
# uses it to pass `-Slot a`. Each element is passed as its own argument, so a
# value is never re-parsed by the child's command line.
function Start-Wave([object[]]$wave, [string]$label, [string[]]$extraArgs = @()) {
    if (-not $wave) { return }
    Write-Host "[wave] '$label': $($wave.Count) cell(s) @ throttle $Throttle" -ForegroundColor DarkCyan
    $procs = @()
    foreach ($c in $wave) {
        while (@($procs | Where-Object { -not $_.HasExited }).Count -ge $Throttle) {
            Start-Sleep -Seconds 3
        }
        $log = Join-Path $resultsRoot "launch-$($c.Model)-$($c.Repo)-$($c.Arm)-$($c.Task).log"
        Write-Host "[wave] launching $($c.Model)/$($c.Repo)/$($c.Arm)/$($c.Task)" -ForegroundColor DarkCyan
        $procs += Start-Process pwsh -PassThru -WindowStyle Hidden -RedirectStandardOutput $log `
            -ArgumentList (@('-NoProfile', '-File', $PSCommandPath,
                '-Model', $c.Model, '-Repo', $c.Repo, '-Arm', $c.Arm, '-Task', $c.Task) + $extraArgs)
        # Stagger cold starts. opencode opens a shared SQLite store on launch;
        # several processes racing for it in the same second lose the race with
        # `Error: Unexpected error / database is locked` and the cell dies at
        # ~1s having spent nothing. Three of eight cells went that way in
        # round-3 Lexomancy wave 1. The wait is per-launch, not per-cell, so a
        # throttled wave pays it only while it is filling slots.
        Start-Sleep -Seconds $LaunchStaggerSeconds
    }
    $procs | ForEach-Object { $_.WaitForExit() }
    $failed = @($procs | Where-Object { $_.ExitCode -ne 0 }).Count
    if ($failed) {
        $script:waveFailures += $failed
        Write-Warning "[wave] '$label': $failed cell(s) exited non-zero — check launch-*.log"
    }
}

# § Pilot — difficulty calibration before the round-2 keys freeze.
#
# Two round-2 tasks have difficulty that is argued rather than observed: lore
# T3 (the `design_status` consumer sweep, whose difficulty rests on the concept
# being spelled seven different ways so a literal grep gets about half of it)
# and terrarium T4 (the dropped Lenia substrate, whose difficulty rests on a
# lazy grep for reject|abandon|dropped returning nothing). The failure mode is a
# task that turns out trivial for BOTH arms, which measures nothing — and which
# you would otherwise discover only after running the whole round.
#
# So both arms run, even though round 2 proper is on-arm-only: the question
# being asked here is "could the off arm have done this easily", which cannot be
# answered without an off arm.
#
# Both arms use slot 'a'. Both pilot tasks are read-only, so sharing a tree is
# safe, and it means slot 'b' need not exist to run the pilot at all.
if ($Pilot) {
    $pilotSet = @(
        [pscustomobject]@{ Repo = 'lore'; Task = 'T3' }
        [pscustomobject]@{ Repo = 'terrarium'; Task = 'T4' }
    )
    # The shared-tree assumption above holds only while the set stays
    # read-only. Assert it rather than trusting whoever edits the list next.
    $writeCells = @($pilotSet | Where-Object { $_.Task -eq 'T5' })
    if ($writeCells) {
        throw "[pilot] pilot cells must be read-only — both arms share slot 'a', so a T5 write would land under the other arm: $($writeCells.Repo -join ', ')"
    }

    $total = [System.Diagnostics.Stopwatch]::StartNew()
    $cells = foreach ($p in $pilotSet) {
        foreach ($ar in 'off', 'on') {
            [pscustomobject]@{ Model = 'luna'; Repo = $p.Repo; Arm = $ar; Task = $p.Task }
        }
    }
    Start-Wave $cells 'pilot' @('-Slot', 'a')
    $total.Stop()
    Write-Host ("[pilot] {0} cells in {1:n1} min. Read the answers before freezing the keys: for each task, did the OFF arm struggle the way the key assumes?" -f
        $cells.Count, $total.Elapsed.TotalMinutes) -ForegroundColor Green
    return
}

if ($Matrix) {
    $total = [System.Diagnostics.Stopwatch]::StartNew()
    $cells = foreach ($mo in $Models) {
        foreach ($re in $Repos) {
            foreach ($ar in $Arms) {
                foreach ($ta in $Tasks) {
                    [pscustomobject]@{ Model = $mo; Repo = $re; Arm = $ar; Task = $ta }
                }
            }
        }
    }
    # Wave 1: luna read-only. Wave 2: luna T5 — safe to parallelise because
    # every T5 cell has a distinct (repo, arm) and therefore a distinct tree,
    # but held out of wave 1 so no T5 write lands under a reading cell in the
    # same tree. Wave 3: qwen, serial (GPU).
    # Which models must run one at a time. Local models contend for the one
    # GPU; hosted ones do not. Testing this by name ('luna') silently demoted
    # every hosted model added later -- `lunamax` ran its whole matrix serially
    # before anyone noticed it was four times slower than it should have been.
    $serialModels = @('qwen')
    $readOnly = @($cells | Where-Object { $_.Model -notin $serialModels -and $_.Task -ne 'T5' })
    $writes = @($cells | Where-Object { $_.Model -notin $serialModels -and $_.Task -eq 'T5' })
    $serial = @($cells | Where-Object { $_.Model -in $serialModels })

    # Assert the invariant the parallel waves rest on, rather than trusting it.
    foreach ($wave in @($readOnly, $writes)) {
        $dupes = $wave | Group-Object { "$($_.Repo)/$($_.Arm)/$($_.Task)" } |
            Where-Object Count -gt 1
        if ($dupes) { throw "[matrix] duplicate cells in a parallel wave: $($dupes.Name -join ', ')" }
    }
    # Group by the tree a cell actually resolves to, not by (repo, arm): with
    # `-Slot` forcing one slot for both arms, (repo, arm) is distinct while the
    # directory is shared, which is exactly the collision this guards.
    $treeClash = $writes | Group-Object {
        $sl = if ($Slot) { $Slot } else { $armSlot[$_.Arm] }
        $repoMap[$_.Repo].slots[$sl].dir
    } | Where-Object Count -gt 1
    if ($treeClash) { throw "[matrix] two T5 cells would share a tree: $($treeClash.Name -join ', '). Run them one at a time." }

    # `-Slot` must reach the child processes too, or a forced slot silently
    # reverts to the arm mapping inside the wave.
    $slotArgs = if ($Slot) { @('-Slot', $Slot) } else { @() }
    Start-Wave $readOnly 'luna T1-T4' $slotArgs
    Start-Wave $writes 'luna T5' $slotArgs

    foreach ($c in $serial) {
        Invoke-Cell $c.Model $c.Repo $c.Arm $c.Task
        if ($script:cellExit -ne 0) { $script:waveFailures++ }
    }
    $total.Stop()
    Write-Host ("[matrix] {0} cells in {1:n1} min ({2} read-only + {3} T5 parallel @ {4}, {5} serial)" -f
        $cells.Count, $total.Elapsed.TotalMinutes, $readOnly.Count, $writes.Count,
        $Throttle, $serial.Count) -ForegroundColor Green
    # A matrix that lost cells must not look like a clean run. Re-run the named
    # cells individually; a failed cell leaves a results directory with
    # exit_code non-zero and zero tokens, which must be quarantined (prefix
    # `x-`) before packing so it is not read as an empty answer.
    if ($script:waveFailures) {
        Write-Host ("[matrix] {0} cell(s) FAILED — this run is incomplete" -f $script:waveFailures) -ForegroundColor Red
        exit 1
    }
} else {
    if (-not ($Model -and $Repo -and $Arm -and $Task)) {
        throw 'Provide -Model -Repo -Arm -Task, or -Matrix.'
    }
    Invoke-Cell $Model $Repo $Arm $Task
    exit $script:cellExit
}
