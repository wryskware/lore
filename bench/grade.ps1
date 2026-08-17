# Bench grading runner. Protocol and rationale:
# design/6_Evaluation/2026-08-17_grading-protocol.md. Criteria live in the
# round's task set; this script only applies them.
#
#   python pack.py --cells '20260817-*' --batch repo-task   # first, always
#   .\grade.ps1 -Round 20260817 -Pass A -Throttle 5         # score, blinded
#   .\grade.ps1 -Round 20260817 -Pass B                     # retrieval diagnosis
#   .\grade.ps1 -Round 20260817 -Pass A -DryRun             # assemble, launch nothing
#
# TWO PASSES, DIFFERENT INPUTS. Pass A scores against the key and is blind to
# the arm: it sees answers labelled A/B, never the tool trail (whose first
# lore_search line gives the arm away). Pass B reads the lore calls WITH their
# returned hits and diagnoses whether retrieval was relevant — and scores
# nothing, because the task set forbids grading an agent's process.
#
# A grader thread runs in an isolated directory holding only its brief, with
# the retrieval-OFF config, so it cannot go re-derive an answer it is supposed
# to be grading.
param(
    [Parameter(Mandatory)] [string]$Round,
    [ValidateSet('A', 'B')] [string]$Pass = 'A',
    [ValidateSet('luna', 'qwen')] [string]$Model = 'luna',
    # Grade one batch only, by name: 'lore-T3', or 'lore' for a pass-B repo.
    [string]$Batch,
    [ValidateRange(1, 16)] [int]$Throttle = 5,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
$benchRoot = $PSScriptRoot
$resultsRoot = Join-Path $benchRoot 'results'
$repoRoot = Split-Path $benchRoot -Parent

$modelMap = @{
    luna = @{ id = 'openai/gpt-5.6-luna'; variant = 'high' }
    qwen = @{ id = 'ollama/qwen3.8:latest'; variant = $null }
}
$m = $modelMap[$Model]

# The key document. Criteria are sliced out of this per task; if a round moves
# to a new task set, this is the one line that changes.
$taskSet = Join-Path $repoRoot 'design\6_Evaluation\2026-08-17_e2e-round-2-task-set.md'
if (-not (Test-Path -LiteralPath $taskSet)) { throw "task set not found: $taskSet" }
$taskSetLines = Get-Content -LiteralPath $taskSet

$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$gradesRoot = Join-Path $benchRoot "grades\$stamp-pass$Pass"
New-Item -ItemType Directory -Force $gradesRoot | Out-Null

# Paragraphs in a key section that are about the TASK'S DESIGN rather than its
# criteria: why it replaced a round-1 task, whether it is expected to
# discriminate, and the self-check arguing that each arm could plausibly
# succeed or fail. A grader has no use for any of it, and the self-check in
# particular predicts the result ("off arm can plainly succeed"), which is a
# thumb on the scale in a pass that is otherwise blind to the arm. Dropped
# paragraph-wise, so nothing in the middle of a criterion is lost.
$dropParagraph = @(
    '^\*\*Self-check\.\*\*',
    '^\*\*Why this replaces',
    '^\*\*On discrimination\.\*\*',
    '^\*\*What changed'
)

function Remove-DesignCommentary([string]$section) {
    $out = foreach ($para in ($section -split "`n`n")) {
        $first = ($para -split "`n")[0]
        $drop = $false
        foreach ($pattern in $dropParagraph) {
            if ($first -match $pattern) { $drop = $true; break }
        }
        if (-not $drop) { $para }
    }
    ($out -join "`n`n").Trim()
}

# Slice `### <repo> T<n> — …` out of the task set, up to the next heading of
# the same or higher level. Sliced rather than summarised on purpose: a grader
# paraphrasing a key is a grader writing its own key.
function Get-KeySection([string]$repo, [string]$task) {
    $start = -1
    for ($i = 0; $i -lt $taskSetLines.Count; $i++) {
        if ($taskSetLines[$i] -match "^### $repo $task\b") { $start = $i; break }
    }
    if ($start -lt 0) { throw "no key section '### $repo $task' in $taskSet" }
    $end = $taskSetLines.Count
    for ($i = $start + 1; $i -lt $taskSetLines.Count; $i++) {
        if ($taskSetLines[$i] -match '^(### |## |---$)') { $end = $i; break }
    }
    Remove-DesignCommentary (($taskSetLines[$start..($end - 1)]) -join "`n")
}

# Archetype preamble for the task type — T4's redesign block in particular
# defines what "a source of record" means, which its per-task keys assume.
function Get-ArchetypeSection([string]$task) {
    if ($task -ne 'T4') { return '' }
    $start = -1
    for ($i = 0; $i -lt $taskSetLines.Count; $i++) {
        if ($taskSetLines[$i] -match '^### T4, redesigned') { $start = $i; break }
    }
    if ($start -lt 0) { return '' }
    $end = $taskSetLines.Count
    for ($i = $start + 1; $i -lt $taskSetLines.Count; $i++) {
        if ($taskSetLines[$i] -match '^(### |## )') { $end = $i; break }
    }
    Remove-DesignCommentary (($taskSetLines[$start..($end - 1)]) -join "`n")
}

# Deterministic, arm-uncorrelated ordering for the blinded labels. Sorting by
# arm would put 'off' first every time and the grader would learn the pattern
# across threads; sorting by a digest of the cell name will not.
function Get-BlindOrder([object[]]$cells) {
    $md5 = [System.Security.Cryptography.MD5]::Create()
    $cells | Sort-Object {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($_.cell)
        [System.BitConverter]::ToString($md5.ComputeHash($bytes))
    }
}

$cellDirs = Get-ChildItem -Directory $resultsRoot |
    Where-Object { $_.Name -like "$Round*" -and (Test-Path (Join-Path $_.FullName 'metrics.json')) }
if (-not $cellDirs) { throw "no result cells matching '$Round*' in $resultsRoot" }

$cells = foreach ($d in $cellDirs) {
    $meta = Get-Content -Raw (Join-Path $d.FullName 'metrics.json') | ConvertFrom-Json
    [pscustomobject]@{
        cell = $meta.cell; repo = $meta.repo; task = $meta.task; arm = $meta.arm
        dir = $d.FullName; meta = $meta
    }
}

$SCHEMA_A = @'
Return ONE JSON array, nothing else, at path verdicts.json in your working
directory. One object per answer you were given:

{
  "label": "A",
  "score": 1,
  "criteria": [{"id": "<criterion from the key, its own words>",
                "met": true,
                "evidence": "<what in the answer satisfies it, quoted or located>"}],
  "missing": ["<required criterion the answer does not satisfy>"],
  "fabricated_citations": ["<file:line the answer cites that looks invented>"],
  "confidence": "high|medium|low",
  "notes": "<one sentence, only if something does not fit the schema>"
}

Rules:
- Score ONLY 0, 0.5 or 1, and only against the scale in the key. Do not invent
  criteria the key does not state, and do not grade style, length or tone.
- Enumerate EVERY required criterion in `criteria`, met or not. A bare score
  with no criterion list is not a usable verdict.
- The two answers are independent. Do not grade one relative to the other, do
  not normalise them, and do not assume they should differ.
- `confidence: low` is the correct output when the key depends on something you
  were not given (a suite result that has not been run, for instance). Say so
  in `notes` rather than guessing.
- You cannot open the repository. Judge only what is in this brief. If a
  citation looks invented, list it in `fabricated_citations` — someone else
  will check it against the source.
'@

$SCHEMA_B = @'
Return ONE JSON array, nothing else, at path verdicts.json in your working
directory. One object per cell:

{
  "cell": "<the cell id>",
  "per_call": [{"position": 1,
                "relevance": "on-target|partial|off-target|no-hits",
                "why": "<what the hits did or did not contain, in one line>"}],
  "verdict": "relevant-and-used|relevant-but-ignored|partially-relevant|irrelevant|no-hits",
  "diagnosis": "<one or two sentences>"
}

You are NOT scoring the answer. You are judging what the lore search RETURNED.

The question this pass exists to settle: when an agent made one search call and
then reverted to grep-and-read, was that because the search answered well
enough that one call sufficed, because it answered well and the agent ignored
it, or because the hits were useless? Those have opposite fixes, so choose.

- `relevant-and-used` — the hits contained what the task needed AND the agent
  built on them (high uptake, or the answer cites what was returned).
- `relevant-but-ignored` — the hits contained what the task needed and the
  agent went and re-derived it anyway. This is the steering finding.
- `irrelevant` — the hits did not contain what the task needed. The agent was
  right to move on. This is the ranking finding.
- Low call count is NOT evidence by itself. One sufficient call is a success.

`uptake` and `answer overlap` in each packet are computed by the harness, not
opinions: uptake counts returned paths the agent later opened, overlap counts
returned paths that survive into the final answer.
'@

function New-BriefA([string]$repo, [string]$task, [object[]]$group) {
    $ordered = Get-BlindOrder $group
    $labels = @('A', 'B', 'C', 'D')
    $prompt = ($group[0].meta.PSObject.Properties['cell'] | Out-Null)
    $promptsJson = Get-Content -Raw (Join-Path $benchRoot 'prompts.json') | ConvertFrom-Json
    $promptText = $promptsJson.$repo.$task

    $sb = [System.Text.StringBuilder]::new()
    [void]$sb.AppendLine("# Grading brief — $repo $task (pass A, task score)")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("You are grading answers produced by coding agents against a frozen key.")
    [void]$sb.AppendLine("You did not write the key and may not change it.")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("## The prompt each agent was given, verbatim")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("> $promptText")
    [void]$sb.AppendLine()
    $arch = Get-ArchetypeSection $task
    if ($arch) {
        [void]$sb.AppendLine("## Archetype")
        [void]$sb.AppendLine()
        [void]$sb.AppendLine($arch)
        [void]$sb.AppendLine()
    }
    [void]$sb.AppendLine("## The key")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine((Get-KeySection $repo $task))
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("## Answers")

    $map = @()
    for ($i = 0; $i -lt $ordered.Count; $i++) {
        $c = $ordered[$i]; $label = $labels[$i]
        $map += [pscustomobject]@{ label = $label; cell = $c.cell; arm = $c.arm }
        $answerPath = Join-Path $c.dir 'answer.md'
        $answer = if (Test-Path $answerPath) { Get-Content -Raw $answerPath } else { '(no answer captured)' }
        [void]$sb.AppendLine()
        [void]$sb.AppendLine("### Answer $label")
        [void]$sb.AppendLine()
        [void]$sb.AppendLine($answer.Trim())

        $diffPath = Join-Path $c.dir 'diff.patch'
        if ((Test-Path $diffPath) -and (Get-Item $diffPath).Length -gt 0) {
            [void]$sb.AppendLine()
            [void]$sb.AppendLine("#### Answer $label — diff")
            [void]$sb.AppendLine()
            [void]$sb.AppendLine('```diff')
            [void]$sb.AppendLine((Get-Content -Raw $diffPath).TrimEnd())
            [void]$sb.AppendLine('```')
            $suitePath = Join-Path $c.dir 'suite-result.txt'
            [void]$sb.AppendLine()
            [void]$sb.AppendLine("#### Answer $label — suite result")
            [void]$sb.AppendLine()
            if (Test-Path $suitePath) {
                [void]$sb.AppendLine((Get-Content -Raw $suitePath).Trim())
            }
            else {
                [void]$sb.AppendLine('NOT RUN. This cell predates the harness running suites itself.')
                [void]$sb.AppendLine('Any criterion that depends on it must come back `confidence: low`, not a guess.')
            }

            # The agent's own suite runs, lifted out of the packet. Weaker
            # evidence than a harness run — the agent picks the command, and a
            # green `--lib chunk::tests` says nothing about the workspace — but
            # it is evidence, and grading four cells as "suite unknown" while
            # the transcript shows a full green run is worse than weighing it.
            $packetPath = Join-Path $c.dir 'packet.md'
            if (Test-Path $packetPath) {
                $packet = Get-Content -Raw $packetPath
                $marker = '### suite runs the agent made itself (self-reported)'
                $at = $packet.IndexOf($marker)
                if ($at -ge 0) {
                    $section = $packet.Substring($at)
                    $nextHeading = [regex]::Match($section.Substring($marker.Length), '(?m)^### ')
                    if ($nextHeading.Success) {
                        $section = $section.Substring(0, $marker.Length + $nextHeading.Index)
                    }
                    [void]$sb.AppendLine()
                    [void]$sb.AppendLine("#### Answer $label — " + $section.Trim())
                }
            }
        }
    }
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("## Output")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine($SCHEMA_A)
    @{ brief = $sb.ToString(); map = $map }
}

function New-BriefB([string]$repo, [object[]]$group) {
    $sb = [System.Text.StringBuilder]::new()
    [void]$sb.AppendLine("# Grading brief — $repo (pass B, retrieval behaviour)")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("Each section below is one retrieval-ON cell: the prompt it answered, every")
    [void]$sb.AppendLine("lore call it made WITH the hits that call returned, the full tool trail, and")
    [void]$sb.AppendLine("the answer it produced.")
    [void]$sb.AppendLine()
    foreach ($c in ($group | Sort-Object task)) {
        $packet = Join-Path $c.dir 'packet.md'
        if (-not (Test-Path $packet)) { throw "no packet.md for $($c.cell) — run: python pack.py --cells '$Round*'" }
        [void]$sb.AppendLine((Get-Content -Raw $packet).TrimEnd())
        [void]$sb.AppendLine()
        [void]$sb.AppendLine('---')
        [void]$sb.AppendLine()
    }
    [void]$sb.AppendLine("## Output")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine($SCHEMA_B)
    $sb.ToString()
}

# Assemble every thread's working directory first, so a broken key slice fails
# before any model is paid to read a half-built brief.
$threads = @()
if ($Pass -eq 'A') {
    $groups = $cells | Group-Object { "$($_.repo)-$($_.task)" }
}
else {
    $groups = $cells | Where-Object { $_.arm -eq 'on' } | Group-Object repo
}
if ($Batch) { $groups = $groups | Where-Object { $_.Name -eq $Batch } }
if (-not $groups) { throw "no batches to grade (Round '$Round', pass $Pass, batch '$Batch')" }

foreach ($g in $groups) {
    $workDir = Join-Path $gradesRoot $g.Name
    New-Item -ItemType Directory -Force $workDir | Out-Null
    if ($Pass -eq 'A') {
        $repo = $g.Group[0].repo; $task = $g.Group[0].task
        $built = New-BriefA $repo $task $g.Group
        Set-Content -LiteralPath (Join-Path $workDir 'brief.md') -Value $built.brief -Encoding utf8
        # The label -> cell map lives OUTSIDE the grader's working directory.
        # Blinding that the grader can read is not blinding.
        $built.map | ConvertTo-Json -Depth 4 |
            Set-Content -LiteralPath (Join-Path $gradesRoot "$($g.Name).map.json") -Encoding utf8
    }
    else {
        Set-Content -LiteralPath (Join-Path $workDir 'brief.md') -Value (New-BriefB $g.Group[0].repo $g.Group) -Encoding utf8
    }
    $threads += [pscustomobject]@{ name = $g.Name; dir = $workDir; cells = $g.Group.Count }
}

Write-Host "[grade] pass $Pass, $($threads.Count) thread(s), $(($threads | Measure-Object cells -Sum).Sum) cell(s) -> $gradesRoot" -ForegroundColor Cyan
foreach ($t in $threads) {
    $kb = [math]::Round((Get-Item (Join-Path $t.dir 'brief.md')).Length / 1KB)
    Write-Host "  $($t.name): $($t.cells) cell(s), brief ${kb} KB"
}
if ($DryRun) {
    Write-Host "[grade] -DryRun: briefs assembled, nothing launched." -ForegroundColor Yellow
    return
}

$gradePrompt = 'Read brief.md in your working directory and follow it exactly. ' +
'Write your verdicts to verdicts.json in that same directory. ' +
'Output the JSON to the file, not to the chat.'

$procs = @()
foreach ($t in $threads) {
    while (@($procs | Where-Object { -not $_.p.HasExited }).Count -ge $Throttle) { Start-Sleep -Seconds 3 }
    Write-Host "[grade] launching $($t.name)" -ForegroundColor DarkCyan
    # Retrieval-OFF config for the grader, always: a grader with a search tool
    # can re-derive the answer it is grading, which is a different task.
    $ocArgs = @('run', '--dir', $t.dir, '-m', $m.id, '--format', 'json',
        '--title', "grade-$Pass-$($t.name)", '--auto')
    if ($m.variant) { $ocArgs += @('--variant', $m.variant) }
    $ocArgs += $gradePrompt
    $env:OPENCODE_CONFIG = Join-Path $benchRoot 'opencode-off.jsonc'
    $procs += [pscustomobject]@{
        t = $t
        p = Start-Process pwsh -PassThru -WindowStyle Hidden `
            -RedirectStandardOutput (Join-Path $t.dir 'events.jsonl') `
            -RedirectStandardError (Join-Path $t.dir 'stderr.log') `
            -ArgumentList @('-NoProfile', '-Command',
                "`$env:OPENCODE_CONFIG='$(Join-Path $benchRoot 'opencode-off.jsonc')'; " +
                "& opencode $(($ocArgs | ForEach-Object { "'" + ($_ -replace "'", "''") + "'" }) -join ' ')")
    }
}
$procs | ForEach-Object { $_.p.WaitForExit() }
Remove-Item Env:OPENCODE_CONFIG -ErrorAction SilentlyContinue

$ok = 0
foreach ($x in $procs) {
    $verdict = Join-Path $x.t.dir 'verdicts.json'
    if (Test-Path $verdict) {
        Copy-Item $verdict (Join-Path $gradesRoot "$($x.t.name).json") -Force
        $ok++
    }
    else {
        Write-Warning "[grade] $($x.t.name): no verdicts.json (exit $($x.p.ExitCode)) — see $($x.t.dir)"
    }
}
Write-Host "[grade] $ok/$($threads.Count) thread(s) returned verdicts -> $gradesRoot" -ForegroundColor Green
Write-Host "[grade] next: audit a sample against a stronger model before believing these." -ForegroundColor Yellow
Write-Host "        design/6_Evaluation/2026-08-17_grading-protocol.md, section 'Who grades'."
