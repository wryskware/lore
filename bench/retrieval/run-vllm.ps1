# Retrieval run against the PRODUCTION embedding stack (vLLM in WSL2, D-0014
# successor) instead of the model-matrix's own llama.cpp server. run.ps1
# predates the 2026-08-17 vLLM switch; this is the drain that matches what
# lore actually ships. The embedding server is expected to be up already
# (scripts\serve-embeddings-vllm.ps1) — this script refuses to start it.
#
#   .\run-vllm.ps1 -Corpora lexomancy
param(
    [string[]]$Corpora = @('lexomancy'),
    [int]$TopK = 20,
    [string]$Endpoint = 'http://127.0.0.1:8000/v1',
    [string]$ModelLabel = 'qwen3-4b-vllm',
    [int]$DrainTimeoutMin = 45,
    [string]$LoreExe = (Join-Path $PSScriptRoot '..\..\target\release\lore.exe')
)
$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot

try { Invoke-RestMethod "$Endpoint/models" -TimeoutSec 5 | Out-Null }
catch { throw "embedding endpoint $Endpoint is not answering - start scripts\serve-embeddings-vllm.ps1 first" }

$mainDataDir = Join-Path $env:LOCALAPPDATA 'lore'
$dataDir = Join-Path $Root "data\$ModelLabel"
if ([IO.Path]::GetFullPath($dataDir) -eq [IO.Path]::GetFullPath($mainDataDir)) { throw 'bench data dir resolves to the main daemon data dir - refusing' }
if (Test-Path $dataDir) { Remove-Item -Recurse -Force $dataDir }
New-Item -ItemType Directory -Force $dataDir | Out-Null
@(
    '[embeddings]'
    "endpoint = ""$Endpoint"""
    'model = "Qwen/Qwen3-Embedding-4B"'
    'dimensions = 2560'
    'query_prefix = "Instruct: Given a natural language query, retrieve relevant code snippets or documentation passages\nQuery: "'
    'document_prefix = ""'
    'batch_max_items = 64'
    'batch_max_bytes = 262144'
    'concurrency = 16'
    'max_embed_bytes = 3584'
) | Set-Content (Join-Path $dataDir 'config.toml')

$allCorpora = Get-Content (Join-Path $Root 'corpora.json') | ConvertFrom-Json |
    Where-Object { $_.name -in $Corpora }
if (-not $allCorpora) { throw 'no corpora selected' }

$runSet = Get-Date -Format 'yyyyMMdd-HHmmss'
$outDir = Join-Path $Root "results\$runSet-$ModelLabel"
New-Item -ItemType Directory -Force $outDir | Out-Null

$savedEnv = $env:LORE_DATA_DIR
$daemon = $null
try {
    $env:LORE_DATA_DIR = $dataDir
    $daemon = Start-Process -FilePath $LoreExe -ArgumentList 'daemon' -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput (Join-Path $outDir 'daemon.out.log') -RedirectStandardError (Join-Path $outDir 'daemon.err.log')
} finally { $env:LORE_DATA_DIR = $savedEnv }

try {
    $handshake = Join-Path $dataDir 'daemon.json'
    $sw = [Diagnostics.Stopwatch]::StartNew()
    while (-not (Test-Path $handshake)) {
        if ($sw.Elapsed.TotalSeconds -gt 60) { throw 'daemon handshake never appeared' }
        if ($daemon.HasExited) { throw 'daemon exited early - see daemon.err.log' }
        Start-Sleep -Milliseconds 300
    }
    $port = (Get-Content $handshake | ConvertFrom-Json).port
    $mainPort = (Get-Content (Join-Path $mainDataDir 'daemon.json') -ErrorAction SilentlyContinue | ConvertFrom-Json).port
    if ($port -eq $mainPort) { throw 'bench daemon reports the main daemon port - refusing' }
    $api = "http://127.0.0.1:$port/v1"
    Write-Host "daemon up on :$port (data: $dataDir)"

    foreach ($c in $allCorpora) {
        Invoke-RestMethod -Method Post "$api/projects" -ContentType 'application/json' `
            -Body (@{ root = $c.root; name = $c.name } | ConvertTo-Json) | Out-Null
    }
    $last = ''; $stable = 0
    while ($true) {
        Start-Sleep -Seconds 10
        $st = Invoke-RestMethod "$api/status" -TimeoutSec 15
        $ps = @($st.projects | Where-Object { $_.name -in $allCorpora.name })
        $chunks = ($ps | Measure-Object chunks -Sum).Sum
        $embedded = ($ps | Measure-Object embedded_chunks -Sum).Sum
        $counts = ($ps | ForEach-Object { "$($_.name):$($_.chunks)/$($_.embedded_chunks)" }) -join ' '
        Write-Host "  [$([math]::Round($sw.Elapsed.TotalMinutes,1))m] $counts"
        if ($chunks -gt 0 -and $counts -eq $last -and $embedded -eq $chunks) { $stable++ } else { $stable = 0 }
        $last = $counts
        if ($stable -ge 3) { break }
        if ($sw.Elapsed.TotalMinutes -gt $DrainTimeoutMin) { throw "drain timeout after $DrainTimeoutMin min" }
    }

    $qr = & (Join-Path $Root 'query.ps1') -Api $api -OutDir $outDir -Corpora @($allCorpora.name) -TopK $TopK
    [ordered]@{
        model = $ModelLabel; run_set = $runSet; started = (Get-Date -Format o)
        note = 'production vLLM embedding stack (run-vllm.ps1), not the model-matrix llama-server'
        corpora = $qr.corpora; warnings = $qr.warnings
    } | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $outDir 'run.json')
    Write-Host "done -> $outDir"
} finally {
    if ($daemon -and -not $daemon.HasExited) { Stop-Process -Id $daemon.Id -Force }
}
