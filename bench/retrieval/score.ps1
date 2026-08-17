# Scores saved query results and writes a summary.
# Pure post-processing: touches nothing but bench/retrieval/results/.
#
# Two independent sources of truth, deliberately kept apart:
#   queries/<corpus>.json    hand-verified answer keys -> recall (hit@k, MRR)
#   judgments/<corpus>.json  labelled result pool (judge.ps1) -> precision,
#                            graded nDCG, and the honest noise rate
# Everything judgment-derived degrades to '-' when a corpus has no labels, so
# an unjudged run still scores exactly as it did before judging existed.
param(
    [string]$RunSet,   # score only this run set (yyyyMMdd-HHmmss); default: latest run per model
    [switch]$All,      # score every run dir found
    [string]$Detail    # per-query breakdown for this model; default: the top-nDCG one
)
$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot

function Norm([string]$p) { $p.Replace('\', '/').ToLowerInvariant() }

$runs = Get-ChildItem (Join-Path $Root 'results') -Directory -ErrorAction SilentlyContinue |
    Where-Object { Test-Path (Join-Path $_.FullName 'run.json') } |
    ForEach-Object {
        $r = Get-Content (Join-Path $_.FullName 'run.json') | ConvertFrom-Json
        [pscustomobject]@{ dir = $_.FullName; model = $r.model; run_set = $r.run_set; run = $r }
    }
if ($RunSet) { $runs = @($runs | Where-Object run_set -eq $RunSet) }
elseif (-not $All) {
    $runs = @($runs | Group-Object model | ForEach-Object { $_.Group | Sort-Object run_set | Select-Object -Last 1 })
}
if (-not $runs) { throw 'no scored runs found under results/' }

$keys = @{}
$labels = @{}     # corpus -> @{ "<query id>|<path>" = label }
foreach ($c in (Get-Content (Join-Path $Root 'corpora.json') | ConvertFrom-Json)) {
    $f = Join-Path $Root $c.queries
    if (Test-Path $f) { $keys[$c.name] = Get-Content $f -Raw | ConvertFrom-Json }
    $j = Join-Path $Root "judgments\$($c.name).json"
    if (Test-Path $j) {
        $map = @{}
        # Judgments are keyed by content hash so a re-pinned corpus can hold
        # several labels for one path; the most recent one is the live label.
        foreach ($e in ((Get-Content $j -Raw | ConvertFrom-Json).judgments | Sort-Object at)) {
            $map["$($e.query_id)|$(Norm $e.path)"] = [int]$e.label
        }
        $labels[$c.name] = $map
    }
}

$rows = @()
foreach ($r in $runs) {
    foreach ($corpus in $keys.Keys) {
        $sf = Join-Path $r.dir "searches\$corpus.json"
        if (-not (Test-Path $sf)) { continue }
        $searches = Get-Content $sf -Raw | ConvertFrom-Json
        $key = @{}
        foreach ($q in $keys[$corpus].queries) { $key[$q.id] = @($q.relevant | ForEach-Object { Norm $_.path }) }

        $lab = $labels[$corpus]
        $per = foreach ($s in $searches) {
            $rel = $key[$s.id]; if (-not $rel) { continue }
            # Results are chunks; several chunks of one file must count as one
            # ranked entry or DCG exceeds its ideal. Collapse to unique paths,
            # first occurrence wins.
            $seen = @{}
            $paths = @($s.results | ForEach-Object { Norm $_.path } | Where-Object {
                if ($seen.ContainsKey($_)) { $false } else { $seen[$_] = 1; $true } })
            $firstHit = 0
            for ($i = 0; $i -lt [math]::Min($paths.Count, 10); $i++) {
                if ($paths[$i] -in $rel) { $firstHit = $i + 1; break }
            }
            $dcg = 0.0; $hits = 0
            for ($i = 0; $i -lt [math]::Min($paths.Count, 10); $i++) {
                if ($paths[$i] -in $rel) { $dcg += 1 / [math]::Log2($i + 2); $hits++ }
            }
            $ideal = 0.0
            for ($i = 0; $i -lt [math]::Min($rel.Count, 10); $i++) { $ideal += 1 / [math]::Log2($i + 2) }
            # -- judged-pool metrics. Precision is computed over the results
            # that actually carry a label, with coverage reported beside it:
            # counting an unjudged result as noise would understate precision
            # exactly as counting it as relevant would inflate it.
            $p5 = $null; $p10 = $null; $p5strict = $null; $cov = $null
            $gndcg = $null; $firstRel = 0
            if ($lab) {
                $labelled = @(); $graded = @()
                for ($i = 0; $i -lt [math]::Min($paths.Count, 10); $i++) {
                    $v = $lab["$($s.id)|$($paths[$i])"]
                    $graded += [int]$v          # unjudged contributes 0 gain
                    $labelled += , @($i, $v)
                    if (-not $firstRel -and $null -ne $v -and $v -ge 1) { $firstRel = $i + 1 }
                }
                $judgedAt = { param($n) @($labelled | Where-Object { $_[0] -lt $n -and $null -ne $_[1] }) }
                $j5 = & $judgedAt 5; $j10 = & $judgedAt 10
                if ($j5.Count) {
                    $p5 = @($j5 | Where-Object { $_[1] -ge 1 }).Count / $j5.Count
                    $p5strict = @($j5 | Where-Object { $_[1] -eq 2 }).Count / $j5.Count
                }
                if ($j10.Count) { $p10 = @($j10 | Where-Object { $_[1] -ge 1 }).Count / $j10.Count }
                $depth = [math]::Min($paths.Count, 10)
                if ($depth) { $cov = $j10.Count / $depth }
                # Graded nDCG against the best ordering of this query's own
                # labelled pool — pool-limited by construction, like every
                # judged-pool ideal.
                $gd = 0.0
                for ($i = 0; $i -lt $graded.Count; $i++) { $gd += $graded[$i] / [math]::Log2($i + 2) }
                $poolLabels = @($lab.Keys | Where-Object { $_ -like "$($s.id)|*" } | ForEach-Object { $lab[$_] } |
                    Sort-Object -Descending | Select-Object -First 10)
                $gi = 0.0
                for ($i = 0; $i -lt $poolLabels.Count; $i++) { $gi += $poolLabels[$i] / [math]::Log2($i + 2) }
                $gndcg = if ($gi) { $gd / $gi } else { $null }
            }
            [pscustomobject]@{
                id = $s.id; kind = $s.kind
                hit5 = [int](($paths[0..([math]::Min($paths.Count,5)-1)] | Where-Object { $_ -in $rel }).Count -gt 0)
                hit10 = [int]($firstHit -gt 0)
                rr = if ($firstHit) { 1.0 / $firstHit } else { 0.0 }
                ndcg = if ($ideal) { $dcg / $ideal } else { 0.0 }
                latency = $s.latency_ms
                # Rank is the diagnostic the averages hide: 0 = the key target
                # never appeared in the top 10.
                rank = $firstHit
                rankRel = $firstRel
                keyRanks = @($rel | ForEach-Object { $t = $_; $ix = [array]::IndexOf($paths, $t); if ($ix -ge 0) { $ix + 1 } else { 0 } })
                p5 = $p5; p10 = $p10; p5strict = $p5strict; coverage = $cov; gndcg = $gndcg
                query = $s.query
            }
        }
        if (-not $per) { continue }
        function Avg($set, $prop) {
            $v = @($set | Where-Object { $null -ne $_.$prop })
            if ($v.Count) { [math]::Round(($v | Measure-Object $prop -Average).Average, 3) } else { $null }
        }
        $rows += [pscustomobject]@{
            model = $r.model; corpus = $corpus; n = @($per).Count
            'hit@5' = [math]::Round((@($per) | Measure-Object hit5 -Average).Average, 3)
            'hit@10' = [math]::Round((@($per) | Measure-Object hit10 -Average).Average, 3)
            'MRR@10' = [math]::Round((@($per) | Measure-Object rr -Average).Average, 3)
            'nDCG@10' = [math]::Round((@($per) | Measure-Object ndcg -Average).Average, 3)
            'P@5' = Avg $per 'p5'
            'P@10' = Avg $per 'p10'
            'P@5 strict' = Avg $per 'p5strict'
            'gnDCG@10' = Avg $per 'gndcg'
            'judged' = Avg $per 'coverage'
            'lat_ms' = [math]::Round((@($per) | Measure-Object latency -Average).Average, 0)
            per = $per
        }
    }
}

function Cell($v, [int]$dp = 3) { if ($null -eq $v) { '-' } else { [math]::Round($v, $dp) } }

$md = [System.Collections.Generic.List[string]]::new()
$md.Add("# Retrieval bench summary`n")
$md.Add('`hit@k` / `MRR` / `nDCG` are **recall** against the hand-verified keys: did the known answer come back, and how high. `P@k` is **precision** against the judged pool: of the files that came back, how many were worth reading (label >= 1; `strict` counts only label 2). `judged` is the share of returned files carrying a label — precision from a low-coverage corpus is a sample, not a measurement.')
$md.Add('')
foreach ($corpusGroup in ($rows | Group-Object corpus)) {
    $md.Add("## $($corpusGroup.Name)`n")
    $md.Add('| model | n | hit@5 | hit@10 | MRR@10 | nDCG@10 | P@5 | P@10 | P@5 strict | gnDCG@10 | judged | avg lat ms |')
    $md.Add('|---|---|---|---|---|---|---|---|---|---|---|---|')
    foreach ($row in ($corpusGroup.Group | Sort-Object 'nDCG@10' -Descending)) {
        $md.Add("| $($row.model) | $($row.n) | $($row.'hit@5') | $($row.'hit@10') | $($row.'MRR@10') | $($row.'nDCG@10') | $(Cell $row.'P@5') | $(Cell $row.'P@10') | $(Cell $row.'P@5 strict') | $(Cell $row.'gnDCG@10') | $(Cell $row.judged 2) | $($row.lat_ms) |")
    }
    $md.Add('')
    if ($null -ne ($corpusGroup.Group | Where-Object { $null -ne $_.'P@10' } | Select-Object -First 1)) {
        $best = @($corpusGroup.Group | Sort-Object 'P@10' -Descending)[0]
        $md.Add("Noise rate (1 - P@10) for **$($best.model)**: **$([math]::Round(1 - $best.'P@10', 3))** — that share of what it returned in the top 10 was labelled noise.")
        $md.Add('')
    }
    $md.Add('Per query kind (hit@10):')
    $md.Add('')
    $kinds = $corpusGroup.Group.per.kind | Sort-Object -Unique
    $md.Add('| model | ' + ($kinds -join ' | ') + ' |')
    $md.Add('|---' * ($kinds.Count + 1) + '|')
    foreach ($row in ($corpusGroup.Group | Sort-Object 'nDCG@10' -Descending)) {
        $cells = foreach ($k in $kinds) {
            $sub = @($row.per | Where-Object kind -eq $k)
            if ($sub) { [math]::Round(($sub | Measure-Object hit10 -Average).Average, 2) } else { '-' }
        }
        $md.Add("| $($row.model) | " + ($cells -join ' | ') + ' |')
    }
    $md.Add('')

    # -- per-query detail. One model, because ranks are for reading, not for
    # comparing: the averages above already do the comparing.
    $d = if ($Detail) { @($corpusGroup.Group | Where-Object model -eq $Detail)[0] }
         else { @($corpusGroup.Group | Sort-Object 'nDCG@10' -Descending)[0] }
    if ($d) {
        $md.Add("### Per query — $($d.model)`n")
        $md.Add('`rank` is where the key target landed (0 = absent from the top 10); `first rel` is the first result the judge called relevant at all.')
        $md.Add('')
        $md.Add('| id | kind | rank | key ranks | first rel | P@5 | judged | query |')
        $md.Add('|---|---|---|---|---|---|---|---|')
        foreach ($q in ($d.per | Sort-Object { if ($_.rank) { $_.rank } else { 99 } } -Descending)) {
            # @(0) is falsy in PowerShell, so a single missed key target must
            # be counted, not truth-tested, or it renders as "no key at all".
            $kr = if (@($q.keyRanks).Count) { (@($q.keyRanks) | ForEach-Object { if ($_) { $_ } else { 'miss' } }) -join ', ' } else { '-' }
            $qt = "$($q.query)"; if ($qt.Length -gt 70) { $qt = $qt.Substring(0, 70) + '...' }
            $md.Add("| $($q.id) | $($q.kind) | $(if ($q.rank) { $q.rank } else { 'miss' }) | $kr | $(if ($q.rankRel) { $q.rankRel } else { '-' }) | $(Cell $q.p5 2) | $(Cell $q.coverage 2) | $qt |")
        }
        $md.Add('')

        # -- worst offenders: the queries worth opening, not a number.
        $bad = @($d.per | Where-Object { -not $_.rank -or $_.rank -gt 5 -or ($null -ne $_.p5 -and $_.p5 -lt 0.4) })
        if ($bad) {
            $md.Add("**Worst offenders ($($bad.Count)/$($d.n))** — key target missing or below rank 5, or under 40% of the top 5 judged useful:")
            $md.Add('')
            foreach ($q in ($bad | Sort-Object { if ($_.rank) { $_.rank } else { 99 } } -Descending)) {
                $md.Add("- **$($q.id)** ($($q.kind)) rank $(if ($q.rank) { $q.rank } else { 'miss' }), P@5 $(Cell $q.p5 2) — $($q.query)")
            }
            $md.Add('')
        }
    }

    # -- judge calibration. The keys are known 2s; a judge that disagrees with
    # them is the instrument failing, and every precision number above inherits
    # that failure. Reported per corpus, always, when labels exist.
    if ($labels[$corpusGroup.Name] -and $keys[$corpusGroup.Name]) {
        $lm = $labels[$corpusGroup.Name]
        $agree = 0; $seen = 0; $soft = 0
        foreach ($q in $keys[$corpusGroup.Name].queries) {
            foreach ($p in $q.relevant) {
                $v = $lm["$($q.id)|$(Norm $p.path)"]
                if ($null -eq $v) { continue }
                $seen++
                if ($v -eq 2) { $agree++ }
                if ($v -ge 1) { $soft++ }
            }
        }
        if ($seen) {
            $md.Add("**Judge calibration:** of $seen hand-verified key entries the judge also saw, it graded $agree as 2 ($([math]::Round(100 * $agree / $seen))%) and $soft as at least 1 ($([math]::Round(100 * $soft / $seen))%). Low agreement invalidates the precision columns above, not the retriever.")
            $md.Add('')
        } else {
            $md.Add('**Judge calibration:** no key entry appears in the judged pool yet — precision above is uncalibrated.')
            $md.Add('')
        }
    }
}
$md.Add("## Cost (indexing drain)`n")
$md.Add('| model | chunks | drain s | chunks/s | prompt tokens | tok/s | VRAM (llama) MiB | VRAM (GPU max) MiB | load s |')
$md.Add('|---|---|---|---|---|---|---|---|---|')
foreach ($r in ($runs | Sort-Object model)) {
    $d = $r.run.drain
    $md.Add("| $($r.run.model) | $($d.chunks) | $($d.seconds) | $($d.chunks_per_sec) | $($d.prompt_tokens) | $($d.tokens_per_sec) | $($d.vram_llama_max_mib) | $($d.vram_gpu_max_mib) | $($r.run.model_load_seconds) |")
}
$md.Add("`n## Daemon-side latency (rolling-window percentiles, ms)`n")
$md.Add('`search_embed` is the embed-query wait inside search — the per-query cost of the model itself; `search` is the whole handler.')
$md.Add('')
$md.Add('| model | search p50/p95/p99 | search_embed p50/p95/p99 | samples (search) |')
$md.Add('|---|---|---|---|')
foreach ($r in ($runs | Sort-Object model)) {
    $g = $r.run.daemon_latency.global
    if (-not $g) { continue }
    $s = $g | Where-Object endpoint -eq 'search'
    $e = $g | Where-Object endpoint -eq 'search_embed'
    $sCell = if ($s) { "$($s.p50_ms) / $($s.p95_ms) / $($s.p99_ms)" } else { '-' }
    $eCell = if ($e) { "$($e.p50_ms) / $($e.p95_ms) / $($e.p99_ms)" } else { '-' }
    $md.Add("| $($r.run.model) | $sCell | $eCell | $(if ($s) { $s.samples } else { '-' }) |")
}
$md.Add('')
$md.Add('Per-corpus store scan (`search_store:<project>`, p50/p95 ms):')
$md.Add('')
$corpusNames = @($runs | ForEach-Object { $_.run.daemon_latency.PSObject.Properties.Name } | Where-Object { $_ -ne 'global' } | Sort-Object -Unique)
if ($corpusNames) {
    $md.Add('| model | ' + ($corpusNames -join ' | ') + ' |')
    $md.Add('|---' * ($corpusNames.Count + 1) + '|')
    foreach ($r in ($runs | Sort-Object model)) {
        $cells = foreach ($cn in $corpusNames) {
            $e = $r.run.daemon_latency.$cn
            if ($e) { "$($e.p50_ms) / $($e.p95_ms)" } else { '-' }
        }
        $md.Add("| $($r.run.model) | " + ($cells -join ' | ') + ' |')
    }
    $md.Add('')
}

$warn = @($runs | ForEach-Object { $_.run.warnings } | Where-Object { $_ })
if ($warn) { $md.Add("`n## Warnings`n"); $warn | ForEach-Object { $md.Add("- $_") } }

$out = Join-Path $Root 'results\summary.md'
$md | Set-Content $out
$rows | Select-Object model, corpus, n, 'hit@5', 'hit@10', 'MRR@10', 'nDCG@10', 'P@5', 'P@10', 'judged', lat_ms | Format-Table -AutoSize
Write-Host "written: $out"
