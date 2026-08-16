# Embedding-model retrieval bench for lore.
#
# For each candidate model this script runs a FULLY ISOLATED stack:
#   llama-server (standalone, CUDA)  <-  bench-only lore daemon (own
#   LORE_DATA_DIR, own port via daemon.json handshake)  <-  search queries.
# The dogfooding daemon in %LOCALAPPDATA%\lore is never touched.
#
# Per model it records: index drain wall time, chunks/s, exact prompt tokens
# (llama-server /metrics), VRAM (per-process + whole GPU, sampled during the
# drain), per-query latency, and the raw top-K results for scoring (score.ps1).
#
# Prereqs: setup.ps1 has run; the e2e bench matrix is NOT running (this needs
# the GPU to itself for clean numbers).
param(
    [string[]]$Models,          # ids from models.json; default: lexical + every model with a downloaded gguf
    [string[]]$Corpora,         # names from corpora.json; default: all
    [int]$TopK = 20,
    [int]$DrainTimeoutMin = 120,
    [int]$LlamaPort = 8091,
    [string]$LoreExe = (Join-Path $PSScriptRoot '..\..\target\release\lore.exe'),
    [switch]$Force              # skip the "is another bench running" guard
)
$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot
$PollSec = 3
$StablePollsNeeded = 3

# ---------------------------------------------------------------- guards ----
$mainDataDir = Join-Path $env:LOCALAPPDATA 'lore'
if (-not (Test-Path $LoreExe)) { throw "lore.exe not found at $LoreExe (build with: cargo build --release)" }
if (-not $Force) {
    $busy = Get-Process opencode -ErrorAction SilentlyContinue
    if ($busy) { throw 'opencode is running — an e2e bench may be in flight. Re-run with -Force if you are sure the GPU is free.' }
}
$llamaExe = Get-ChildItem -Recurse (Join-Path $Root 'tools') -Filter llama-server.exe -ErrorAction SilentlyContinue | Select-Object -First 1

# ---------------------------------------------------------------- inputs ----
$allModels = Get-Content (Join-Path $Root 'models.json') | ConvertFrom-Json
$allCorpora = Get-Content (Join-Path $Root 'corpora.json') | ConvertFrom-Json
if ($Corpora) { $allCorpora = @($allCorpora | Where-Object { $_.name -in $Corpora }) }
if (-not $Models) {
    $Models = @('lexical') + @($allModels | Where-Object {
        $_.id -ne 'lexical' -and (Test-Path (Join-Path $Root "models\$($_.id).gguf")) } | ForEach-Object id)
}

function Wait-Http([string]$Url, [int]$TimeoutSec, [string]$What) {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($sw.Elapsed.TotalSeconds -lt $TimeoutSec) {
        try { Invoke-RestMethod $Url -TimeoutSec 5 | Out-Null; return $sw.Elapsed.TotalSeconds }
        catch { Start-Sleep -Milliseconds 500 }
    }
    throw "$What did not become healthy within ${TimeoutSec}s ($Url)"
}

function Get-PromptTokens([int]$Port) {
    try {
        $text = Invoke-RestMethod "http://127.0.0.1:$Port/metrics" -TimeoutSec 5
        foreach ($line in ($text -split "`n")) {
            if ($line -match '^llamacpp:prompt_tokens_total\S*\s+([0-9.eE+]+)') { return [double]$Matches[1] }
        }
    } catch {}
    return $null
}

$runSet = Get-Date -Format 'yyyyMMdd-HHmmss'
foreach ($modelId in $Models) {
    $m = $allModels | Where-Object id -eq $modelId
    if (-not $m) { Write-Warning "unknown model id '$modelId' — skipping"; continue }
    $isLexical = $modelId -eq 'lexical'
    $outDir = Join-Path $Root "results\$runSet-$modelId"
    New-Item -ItemType Directory -Force $outDir, (Join-Path $outDir 'searches') | Out-Null
    Write-Host "`n=== $modelId ==="

    # -- fresh isolated data dir -------------------------------------------
    $dataDir = Join-Path $Root "data\$modelId"
    if ([IO.Path]::GetFullPath($dataDir) -eq [IO.Path]::GetFullPath($mainDataDir)) { throw 'bench data dir resolves to the main daemon data dir — refusing' }
    if (Test-Path $dataDir) { Remove-Item -Recurse -Force $dataDir }
    New-Item -ItemType Directory -Force $dataDir | Out-Null

    if ($isLexical) {
        Set-Content (Join-Path $dataDir 'config.toml') "# lexical-only control arm: no embedding endpoint on purpose.`n"
    } else {
        @(
            '[embeddings]'
            "endpoint = ""http://127.0.0.1:$LlamaPort/v1"""
            "model = ""$modelId"""
            "dimensions = $($m.dimensions)"
            "query_prefix = $($m.query_prefix | ConvertTo-Json)"
            "document_prefix = $($m.document_prefix | ConvertTo-Json)"
            'batch_max_items = 64'
        ) | Set-Content (Join-Path $dataDir 'config.toml')
    }

    $llama = $null; $daemon = $null
    $run = [ordered]@{
        model = $modelId; run_set = $runSet; started = (Get-Date -Format o)
        config = $m; corpora = @(); warnings = @()
    }
    try {
        # -- llama-server ---------------------------------------------------
        if (-not $isLexical) {
            if (-not $llamaExe) { throw 'llama-server.exe missing — run setup.ps1 first' }
            $gguf = Join-Path $Root "models\$modelId.gguf"
            if (-not (Test-Path $gguf)) { throw "$gguf missing — run setup.ps1" }
            $args = @('-m', $gguf, '--embedding', '--pooling', $m.pooling,
                      '-c', $m.ctx, '-b', 8192, '-ub', 8192, '--parallel', 4,
                      '-ngl', 999, '--metrics', '--host', '127.0.0.1', '--port', $LlamaPort)
            $llama = Start-Process -FilePath $llamaExe.FullName -ArgumentList $args -PassThru -WindowStyle Hidden `
                -RedirectStandardOutput (Join-Path $outDir 'llama.out.log') -RedirectStandardError (Join-Path $outDir 'llama.err.log')
            $run.model_load_seconds = [math]::Round((Wait-Http "http://127.0.0.1:$LlamaPort/health" 300 'llama-server'), 1)
        }

        # -- isolated lore daemon ------------------------------------------
        $savedEnv = $env:LORE_DATA_DIR
        try {
            $env:LORE_DATA_DIR = $dataDir
            $daemon = Start-Process -FilePath $LoreExe -ArgumentList 'daemon' -PassThru -WindowStyle Hidden `
                -RedirectStandardOutput (Join-Path $outDir 'daemon.out.log') -RedirectStandardError (Join-Path $outDir 'daemon.err.log')
        } finally { $env:LORE_DATA_DIR = $savedEnv }
        $handshake = Join-Path $dataDir 'daemon.json'
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        while (-not (Test-Path $handshake)) {
            if ($sw.Elapsed.TotalSeconds -gt 60) { throw 'daemon handshake never appeared' }
            if ($daemon.HasExited) { throw "daemon exited early — see daemon.err.log" }
            Start-Sleep -Milliseconds 300
        }
        $port = (Get-Content $handshake | ConvertFrom-Json).port
        if ($port -eq (Get-Content (Join-Path $mainDataDir 'daemon.json') -ErrorAction SilentlyContinue | ConvertFrom-Json).port) { throw 'bench daemon reports the main daemon port — refusing' }
        $api = "http://127.0.0.1:$port/v1"
        Wait-Http "$api/status" 60 'bench daemon' | Out-Null
        Write-Host "  daemon up on :$port (data: $dataDir)"

        # -- register corpora & drain --------------------------------------
        $tok0 = if (-not $isLexical) { Get-PromptTokens $LlamaPort } else { $null }
        foreach ($c in $allCorpora) {
            Invoke-RestMethod -Method Post "$api/projects" -ContentType 'application/json' `
                -Body (@{ root = $c.root; name = $c.name } | ConvertTo-Json) | Out-Null
        }
        $drainSw = [System.Diagnostics.Stopwatch]::StartNew()
        $stable = 0; $lastCounts = ''
        $vram = New-Object System.Collections.Generic.List[string]
        $vram.Add('elapsed_s,llama_mib,gpu_mib,chunks,embedded')
        $maxLlama = 0; $maxGpu = 0
        while ($true) {
            Start-Sleep -Seconds $PollSec
            $st = Invoke-RestMethod "$api/status" -TimeoutSec 15
            $ps = @($st.projects | Where-Object { $_.name -in $allCorpora.name })
            $chunks = ($ps | Measure-Object chunks -Sum).Sum
            $embedded = ($ps | Measure-Object embedded_chunks -Sum).Sum
            $llamaMib = 0; $gpuMib = 0
            try {
                $gpuMib = [int](& nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits)
                if ($llama) {
                    $apps = & nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits
                    foreach ($line in $apps) {
                        $p, $mem = $line -split ',\s*'
                        if ([int]$p -eq $llama.Id) { $llamaMib = [int]$mem }
                    }
                }
            } catch {}
            if ($llamaMib -gt $maxLlama) { $maxLlama = $llamaMib }
            if ($gpuMib -gt $maxGpu) { $maxGpu = $gpuMib }
            $vram.Add("$([math]::Round($drainSw.Elapsed.TotalSeconds,1)),$llamaMib,$gpuMib,$chunks,$embedded")
            $counts = ($ps | ForEach-Object { "$($_.name):$($_.chunks)/$($_.embedded_chunks)" }) -join ' '
            Write-Host "  [$([math]::Round($drainSw.Elapsed.TotalMinutes,1))m] $counts  vram:$llamaMib MiB"
            $done = ($chunks -gt 0) -and ($counts -eq $lastCounts) -and ($isLexical -or ($embedded -eq $chunks))
            $lastCounts = $counts
            if ($done) { $stable++ } else { $stable = 0 }
            if ($stable -ge $StablePollsNeeded) { break }
            if ($drainSw.Elapsed.TotalMinutes -gt $DrainTimeoutMin) { throw "drain timeout after $DrainTimeoutMin min" }
        }
        $drainSec = [math]::Round($drainSw.Elapsed.TotalSeconds - ($StablePollsNeeded * $PollSec), 1)
        $tok1 = if (-not $isLexical) { Get-PromptTokens $LlamaPort } else { $null }
        $vram | Set-Content (Join-Path $outDir 'vram.csv')
        $totalChunks = ($lastCounts -split ' ' | ForEach-Object { [int]($_ -split '[:/]')[1] } | Measure-Object -Sum).Sum
        $run.drain = [ordered]@{
            seconds = $drainSec
            chunks = $totalChunks
            chunks_per_sec = if ($drainSec -gt 0) { [math]::Round($totalChunks / $drainSec, 1) } else { $null }
            prompt_tokens = if ($tok1 -ne $null -and $tok0 -ne $null) { [long]($tok1 - $tok0) } else { $null }
            tokens_per_sec = if ($tok1 -ne $null -and $tok0 -ne $null -and $drainSec -gt 0) { [math]::Round(($tok1 - $tok0) / $drainSec) } else { $null }
            vram_llama_max_mib = $maxLlama
            vram_gpu_max_mib = $maxGpu
        }

        # -- queries --------------------------------------------------------
        $latencies = New-Object System.Collections.Generic.List[double]
        foreach ($c in $allCorpora) {
            $qFile = Join-Path $Root $c.queries
            if (-not (Test-Path $qFile)) { $run.warnings += "no query file for $($c.name)"; continue }
            $key = Get-Content $qFile -Raw | ConvertFrom-Json
            $out = @()
            foreach ($q in $key.queries) {
                $qsw = [System.Diagnostics.Stopwatch]::StartNew()
                $resp = Invoke-RestMethod -Method Post "$api/search" -ContentType 'application/json' `
                    -Body (@{ query = $q.query; project = $c.name; limit = $TopK } | ConvertTo-Json)
                $qsw.Stop()
                $latencies.Add($qsw.Elapsed.TotalMilliseconds)
                if ($resp.lexical_only -ne $isLexical) { $run.warnings += "lexical_only=$($resp.lexical_only) unexpected for $($q.id)" }
                $out += [ordered]@{
                    id = $q.id; kind = $q.kind; query = $q.query
                    lexical_only = $resp.lexical_only
                    latency_ms = [math]::Round($qsw.Elapsed.TotalMilliseconds, 1)
                    results = @($resp.results | ForEach-Object { [ordered]@{
                        path = $_.path; line_start = $_.line_start; line_end = $_.line_end; score = $_.score } })
                }
            }
            $out | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $outDir "searches\$($c.name).json")
            $run.corpora += $c.name
            Write-Host "  $($c.name): $($out.Count) queries"
        }
        # Daemon-side latency percentiles (additive `latency` field on /status):
        # global endpoints separate the embed-query wait (model cost) from the
        # whole search handler; ?project= adds that corpus's store-scan window.
        $lat = @{ global = (Invoke-RestMethod "$api/status" -TimeoutSec 15).latency }
        foreach ($c in $allCorpora) {
            $entry = (Invoke-RestMethod "$api/status?project=$([uri]::EscapeDataString($c.name))" -TimeoutSec 15).latency |
                Where-Object endpoint -eq "search_store:$($c.name)"
            if ($entry) { $lat[$c.name] = $entry }
        }
        $run.daemon_latency = $lat
        $sorted = $latencies | Sort-Object
        if ($sorted.Count) {
            $run.query_latency_ms = [ordered]@{
                p50 = [math]::Round($sorted[[int]($sorted.Count * 0.5)], 1)
                p95 = [math]::Round($sorted[[math]::Min([int]($sorted.Count * 0.95), $sorted.Count - 1)], 1)
            }
        }
        $run.finished = Get-Date -Format o
    } finally {
        if ($daemon -and -not $daemon.HasExited) { Stop-Process -Id $daemon.Id -Force }
        if ($llama -and -not $llama.HasExited) { Stop-Process -Id $llama.Id -Force }
        $run | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $outDir 'run.json')
    }
    Write-Host "  done -> $outDir"
}
Write-Host "`nAll models finished. Score with: .\score.ps1 -RunSet $runSet"
