# Runs a query set against an already-running lore daemon and records the
# top-K results for scoring. Nothing here spawns, drains or measures anything:
# that is run.ps1's job, and this is the part of it worth calling on its own.
#
#   .\query.ps1 -Api http://127.0.0.1:PORT/v1 -OutDir results\20260817-shipped
#
# The daemon must already have the corpora registered and drained. Point it at
# a bench daemon, or at the dogfooding one to score the config you actually
# ship — this only issues searches.
param(
    [Parameter(Mandatory)] [string]$Api,        # http://127.0.0.1:<port>/v1
    [Parameter(Mandatory)] [string]$OutDir,     # searches\<corpus>.json lands here
    [string[]]$Corpora,                         # names from corpora.json; default: all
    [int]$TopK = 20,
    [switch]$ExpectLexicalOnly                  # warn if any search used vectors
)
$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot

$allCorpora = Get-Content (Join-Path $Root 'corpora.json') | ConvertFrom-Json
if ($Corpora) { $allCorpora = @($allCorpora | Where-Object { $_.name -in $Corpora }) }
if (-not $allCorpora) { throw 'no corpora selected' }
New-Item -ItemType Directory -Force $OutDir, (Join-Path $OutDir 'searches') | Out-Null

$result = [ordered]@{ corpora = @(); warnings = @(); latencies = @() }
foreach ($c in $allCorpora) {
    $qFile = Join-Path $Root $c.queries
    if (-not (Test-Path $qFile)) { $result.warnings += "no query file for $($c.name)"; continue }
    $key = Get-Content $qFile -Raw | ConvertFrom-Json
    $out = @()
    foreach ($q in $key.queries) {
        $qsw = [System.Diagnostics.Stopwatch]::StartNew()
        $resp = Invoke-RestMethod -Method Post "$Api/search" -ContentType 'application/json' `
            -Body (@{ query = $q.query; project = $c.name; limit = $TopK } | ConvertTo-Json)
        $qsw.Stop()
        $result.latencies += $qsw.Elapsed.TotalMilliseconds
        if ($resp.lexical_only -ne [bool]$ExpectLexicalOnly) {
            $result.warnings += "lexical_only=$($resp.lexical_only) unexpected for $($q.id)"
        }
        $out += [ordered]@{
            id = $q.id; kind = $q.kind; query = $q.query
            lexical_only = $resp.lexical_only
            latency_ms = [math]::Round($qsw.Elapsed.TotalMilliseconds, 1)
            results = @($resp.results | ForEach-Object { [ordered]@{
                path = $_.path; line_start = $_.line_start; line_end = $_.line_end; score = $_.score } })
            # Lane-aware daemons report distilled cards beside the page;
            # pre-lane daemons have no field and this records @().
            distilled = @($resp.distilled | ForEach-Object { [ordered]@{
                path = $_.path; score = $_.score } })
        }
    }
    $out | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $OutDir "searches\$($c.name).json")
    $result.corpora += $c.name
    Write-Host "  $($c.name): $($out.Count) queries"
}
[pscustomobject]$result
