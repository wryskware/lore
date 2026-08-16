# Scores saved run.ps1 results against the answer keys and writes a summary.
# Pure post-processing: touches nothing but bench/embed/results/.
param(
    [string]$RunSet,   # score only this run set (yyyyMMdd-HHmmss); default: latest run per model
    [switch]$All       # score every run dir found
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
foreach ($c in (Get-Content (Join-Path $Root 'corpora.json') | ConvertFrom-Json)) {
    $f = Join-Path $Root $c.queries
    if (Test-Path $f) { $keys[$c.name] = Get-Content $f -Raw | ConvertFrom-Json }
}

$rows = @()
foreach ($r in $runs) {
    foreach ($corpus in $keys.Keys) {
        $sf = Join-Path $r.dir "searches\$corpus.json"
        if (-not (Test-Path $sf)) { continue }
        $searches = Get-Content $sf -Raw | ConvertFrom-Json
        $key = @{}
        foreach ($q in $keys[$corpus].queries) { $key[$q.id] = @($q.relevant | ForEach-Object { Norm $_.path }) }

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
            [pscustomobject]@{
                id = $s.id; kind = $s.kind
                hit5 = [int](($paths[0..([math]::Min($paths.Count,5)-1)] | Where-Object { $_ -in $rel }).Count -gt 0)
                hit10 = [int]($firstHit -gt 0)
                rr = if ($firstHit) { 1.0 / $firstHit } else { 0.0 }
                ndcg = if ($ideal) { $dcg / $ideal } else { 0.0 }
                latency = $s.latency_ms
            }
        }
        if (-not $per) { continue }
        $rows += [pscustomobject]@{
            model = $r.model; corpus = $corpus; n = @($per).Count
            'hit@5' = [math]::Round((@($per) | Measure-Object hit5 -Average).Average, 3)
            'hit@10' = [math]::Round((@($per) | Measure-Object hit10 -Average).Average, 3)
            'MRR@10' = [math]::Round((@($per) | Measure-Object rr -Average).Average, 3)
            'nDCG@10' = [math]::Round((@($per) | Measure-Object ndcg -Average).Average, 3)
            'lat_ms' = [math]::Round((@($per) | Measure-Object latency -Average).Average, 0)
            per = $per
        }
    }
}

$md = [System.Collections.Generic.List[string]]::new()
$md.Add("# Embedding bench summary`n")
foreach ($corpusGroup in ($rows | Group-Object corpus)) {
    $md.Add("## $($corpusGroup.Name)`n")
    $md.Add('| model | n | hit@5 | hit@10 | MRR@10 | nDCG@10 | avg lat ms |')
    $md.Add('|---|---|---|---|---|---|---|')
    foreach ($row in ($corpusGroup.Group | Sort-Object 'nDCG@10' -Descending)) {
        $md.Add("| $($row.model) | $($row.n) | $($row.'hit@5') | $($row.'hit@10') | $($row.'MRR@10') | $($row.'nDCG@10') | $($row.lat_ms) |")
    }
    $md.Add('')
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
$rows | Select-Object model, corpus, n, 'hit@5', 'hit@10', 'MRR@10', 'nDCG@10', lat_ms | Format-Table -AutoSize
Write-Host "written: $out"
