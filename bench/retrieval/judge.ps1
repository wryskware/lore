# Labels the pooled search results so precision becomes computable.
#
# The answer keys name *the* right file; they do not name every acceptable one,
# so nothing in them can say whether the other nine results were useful or
# noise. This walks the union of paths any recorded run actually returned and
# gives each one a grade against the query that produced it:
#
#   2 = answers the query on its own
#   1 = useful supporting context
#   0 = noise
#
# Judgments are cached in judgments/<corpus>.json keyed by
# (query id, path, file sha256), so re-judging a new arm costs only the paths
# nobody has returned before, and moving a corpus off its pin invalidates only
# the files whose content actually changed.
#
#   .\judge.ps1 -DryRun            # what it would cost, judging nothing
#   .\judge.ps1                    # judge everything unjudged
#   .\judge.ps1 -Corpora lore-bench -MaxItems 60
#
# The judge is an LLM (luna free tier through opencode, batched). It never sees
# the answer key: score.ps1 compares its labels against the hand-verified key
# entries afterwards and reports the agreement rate, which is the only thing
# standing between "measured" and "asserted".
param(
    [string[]]$Corpora,                              # names from corpora.json; default: all
    [string]$RunSet,                                 # pool from this run set only; default: every recorded run
    [int]$JudgeK = 10,                               # rank depth to label (precision@10 needs 10)
    [int]$BatchSize = 12,                            # candidates per model call
    [int]$MaxItems = 0,                              # stop after N new labels (0 = no cap)
    [int]$SnippetMaxChars = 4000,
    [string]$Model = 'openai/gpt-5.6-luna',
    [string]$Variant,                                # provider reasoning variant; default: model default
    [switch]$DryRun,                                 # report the pool, call nothing
    [switch]$Force                                   # judge even if a corpus is off its pin
)
$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot
$judgeDir = Join-Path $Root 'judgments'
New-Item -ItemType Directory -Force $judgeDir | Out-Null

function Norm([string]$p) { $p.Replace('\', '/') }
function Sha256File([string]$p) {
    try { (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash.ToLowerInvariant() } catch { $null }
}

$allCorpora = Get-Content (Join-Path $Root 'corpora.json') | ConvertFrom-Json
if ($Corpora) { $allCorpora = @($allCorpora | Where-Object { $_.name -in $Corpora }) }
if (-not $allCorpora) { throw 'no corpora selected' }

$runDirs = @(Get-ChildItem (Join-Path $Root 'results') -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName 'searches') })
if ($RunSet) { $runDirs = @($runDirs | Where-Object { $_.Name -like "$RunSet*" }) }
if (-not $runDirs) { throw 'no run directories with searches/ found under results/' }

$prompted = 0
foreach ($c in $allCorpora) {
    $keyFile = Join-Path $Root $c.queries
    if (-not (Test-Path $keyFile)) { Write-Warning "no query file for $($c.name) — skipping"; continue }
    $key = Get-Content $keyFile -Raw | ConvertFrom-Json
    $queryText = @{}
    foreach ($q in $key.queries) { $queryText[$q.id] = $q.query }

    # -- pin check -----------------------------------------------------------
    # Judgments are only reusable while the corpus sits where the key says it
    # does. Git pins are checked; a cm pin (`cs:NNN`) is reported and trusted,
    # because interrogating cm from a script is not worth a blocked GUI.
    # A git pin is a 40-hex sha. Anything else (`cs:134`) is a cm changeset,
    # and Lexomancy carries a .git directory as well as a cm workspace, so
    # presence of one is not evidence of which pin the key means.
    if ($key.frozen_at -match '^[0-9a-f]{40}$' -and (Test-Path (Join-Path $c.root '.git'))) {
        $head = (git -C $c.root rev-parse HEAD 2>$null)
        if ($LASTEXITCODE -eq 0 -and $head -ne $key.frozen_at) {
            $msg = "$($c.name) is at $head, key says $($key.frozen_at)"
            if (-not $Force) { throw "$msg — judgments would not be reusable. Re-pin, or pass -Force." }
            Write-Warning "$msg (-Force)"
        }
    } elseif ($key.frozen_at) {
        Write-Host "  $($c.name): pin $($key.frozen_at) not machine-checkable (non-git corpus) — trusting it" -ForegroundColor DarkYellow
    }

    # -- pool ----------------------------------------------------------------
    # One entry per (query, path): the file is what gets labelled, and the span
    # shown to the judge is the best-ranked chunk of it, which is what a caller
    # would have read first.
    $pool = [ordered]@{}
    foreach ($d in $runDirs) {
        $sf = Join-Path $d.FullName "searches\$($c.name).json"
        if (-not (Test-Path $sf)) { continue }
        foreach ($s in (Get-Content $sf -Raw | ConvertFrom-Json)) {
            $seen = @{}; $rank = 0
            foreach ($r in $s.results) {
                $p = Norm $r.path
                if ($seen.ContainsKey($p)) { continue }
                $seen[$p] = 1; $rank++
                if ($rank -gt $JudgeK) { break }
                $k = "$($s.id)|$p"
                if (-not $pool.Contains($k)) {
                    $pool[$k] = [pscustomobject]@{
                        qid = $s.id; path = $p; line_start = $r.line_start; line_end = $r.line_end
                    }
                }
            }
        }
    }

    # -- what is already labelled -------------------------------------------
    $store = Join-Path $judgeDir "$($c.name).json"
    $judged = if (Test-Path $store) { Get-Content $store -Raw | ConvertFrom-Json } else { $null }
    $have = @{}
    if ($judged) { foreach ($j in $judged.judgments) { $have["$($j.query_id)|$($j.path)|$($j.sha256)"] = $j } }

    $todo = @()
    $missing = @()
    foreach ($e in $pool.Values) {
        $full = Join-Path $c.root ($e.path -replace '/', '\')
        $sha = Sha256File $full
        if (-not $sha) { $missing += $e.path; continue }
        if ($have.ContainsKey("$($e.qid)|$($e.path)|$sha")) { continue }
        $todo += [pscustomobject]@{ qid = $e.qid; path = $e.path; sha = $sha; full = $full
                                    line_start = $e.line_start; line_end = $e.line_end }
    }
    $missing = @($missing | Sort-Object -Unique)
    Write-Host "$($c.name): pool $($pool.Count), labelled $($have.Count), to judge $($todo.Count)" -ForegroundColor Cyan
    if ($missing) { Write-Warning "$($c.name): $($missing.Count) pooled path(s) not on disk at the pin (e.g. $($missing[0]))" }
    if ($DryRun -or -not $todo) { continue }
    if ($MaxItems -gt 0 -and $todo.Count -gt ($MaxItems - $prompted)) {
        $todo = @($todo | Select-Object -First ([math]::Max(0, $MaxItems - $prompted)))
        Write-Host "  capped to $($todo.Count) by -MaxItems" -ForegroundColor DarkYellow
    }
    if (-not $todo) { continue }

    # -- judge ---------------------------------------------------------------
    $new = @()
    $batches = [math]::Ceiling($todo.Count / $BatchSize)
    for ($b = 0; $b -lt $batches; $b++) {
        $batch = @($todo | Select-Object -Skip ($b * $BatchSize) -First $BatchSize)
        $items = foreach ($i in 0..($batch.Count - 1)) {
            $e = $batch[$i]
            $lines = Get-Content -LiteralPath $e.full -ErrorAction SilentlyContinue
            $from = [math]::Max(1, [int]$e.line_start); $to = [math]::Min($lines.Count, [int]$e.line_end)
            $span = if ($lines -and $to -ge $from) { ($lines[($from - 1)..($to - 1)] -join "`n") } else { '' }
            if ($span.Length -gt $SnippetMaxChars) { $span = $span.Substring(0, $SnippetMaxChars) + "`n...[truncated]" }
            @"
### ITEM $i
QUERY: $($queryText[$e.qid])
PATH: $($e.path)  (lines $($e.line_start)-$($e.line_end))
RETRIEVED TEXT:
``````
$span
``````
"@
        }
        $prompt = @"
You are grading a code-search result set. For each ITEM below, decide how well
the retrieved file answers the QUERY, judging only from the text shown and the
path. Do not use any tools. Do not read files. Do not search.

Grades:
2 = answers the query on its own; a developer who read this would be done.
1 = useful supporting context; genuinely worth having read, but not the answer.
0 = noise; being in the result list wastes the reader's attention.

Judge the file on its merits for this query. Do not guess at what else the
repository might contain, and do not reward a file for being important in
general if it does not speak to the query.

Reply with ONLY a JSON array, one object per ITEM, no prose and no code fence:
[{"i":0,"label":2,"why":"one short sentence"}, ...]

$($items -join "`n")
"@
        $ocArgs = @('run', '--dir', $Root, '-m', $Model, '--format', 'json', '--title', "judge-$($c.name)-$b")
        if ($Variant) { $ocArgs += @('--variant', $Variant) }
        $ocArgs += $prompt

        $tmp = Join-Path ([IO.Path]::GetTempPath()) "lore-judge-$([guid]::NewGuid().ToString('n')).jsonl"
        $env:OPENCODE_CONFIG = Join-Path $Root 'opencode-judge.jsonc'
        try { & opencode @ocArgs 1> $tmp 2> $null } finally { Remove-Item Env:OPENCODE_CONFIG -ErrorAction SilentlyContinue }

        $text = [System.Text.StringBuilder]::new()
        foreach ($line in (Get-Content $tmp -ErrorAction SilentlyContinue)) {
            $ev = try { $line | ConvertFrom-Json } catch { continue }
            if ($ev.type -eq 'text') { [void]$text.AppendLine($ev.part.text) }
        }
        Remove-Item $tmp -ErrorAction SilentlyContinue
        $raw = $text.ToString()
        $m = [regex]::Match($raw, '\[[\s\S]*\]')
        $parsed = if ($m.Success) { try { $m.Value | ConvertFrom-Json } catch { $null } } else { $null }
        if (-not $parsed) {
            Write-Warning "batch $($b + 1)/$batches returned no parseable JSON — leaving it unjudged"
            continue
        }
        foreach ($p in $parsed) {
            $i = [int]$p.i
            if ($i -lt 0 -or $i -ge $batch.Count) { continue }
            $lab = [int]$p.label
            if ($lab -lt 0 -or $lab -gt 2) { continue }
            $e = $batch[$i]
            $new += [ordered]@{
                query_id = $e.qid; path = $e.path; sha256 = $e.sha
                label = $lab; why = "$($p.why)"; model = $Model; at = (Get-Date -Format o)
            }
        }
        $prompted += $batch.Count
        Write-Host "  batch $($b + 1)/$batches -> $($new.Count) labels so far"
    }

    # -- persist -------------------------------------------------------------
    if (-not $new) { continue }
    $all = @()
    if ($judged) { $all += $judged.judgments }
    $all += $new
    $all = @($all | Sort-Object query_id, path, sha256)
    [ordered]@{
        corpus = $c.name
        frozen_at = $key.frozen_at
        scale = '2 = answers the query, 1 = useful context, 0 = noise'
        judgments = $all
    } | ConvertTo-Json -Depth 5 | Set-Content $store
    Write-Host "  wrote $($all.Count) judgments -> judgments\$($c.name).json" -ForegroundColor Green
}
