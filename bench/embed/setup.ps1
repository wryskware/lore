# Downloads llama.cpp (Windows CUDA build) and the candidate GGUFs.
# Network + disk only — touches no daemon, no GPU. Safe to run any time.
# Resumable: existing complete files are skipped; partial downloads resume.
param(
    [string[]]$Models,      # model ids from models.json; default: all
    [switch]$SkipTools,     # skip the llama.cpp download
    [switch]$SkipModels
)
$ErrorActionPreference = 'Stop'
$Root = $PSScriptRoot
$ToolsDir = Join-Path $Root 'tools'
$ModelsDir = Join-Path $Root 'models'
New-Item -ItemType Directory -Force $ToolsDir, $ModelsDir | Out-Null

function Get-File([string]$Url, [string]$Dest) {
    if (Test-Path $Dest) { Write-Host "  exists: $(Split-Path -Leaf $Dest)"; return }
    Write-Host "  fetching $Url"
    # curl.exe: resumable (-C -), follows redirects, retries transient failures.
    & curl.exe -L --retry 3 --retry-delay 5 -C - -o "$Dest.part" $Url
    if ($LASTEXITCODE -ne 0) { throw "download failed: $Url" }
    Move-Item -Force "$Dest.part" $Dest
}

if (-not $SkipTools) {
    Write-Host "== llama.cpp (Windows CUDA x64) =="
    $rel = Invoke-RestMethod 'https://api.github.com/repos/ggml-org/llama.cpp/releases/latest'
    Write-Host "  release: $($rel.tag_name)"
    # Highest CUDA version first — Blackwell (RTX 5090) needs CUDA >= 12.8.
    $bin = $rel.assets | Where-Object { $_.name -match 'win' -and $_.name -match 'cuda' -and $_.name -match 'x64' -and $_.name -notmatch 'cudart' } | Sort-Object name -Descending | Select-Object -First 1
    $cudart = $rel.assets | Where-Object { $_.name -match 'cudart' -and $_.name -match 'win' -and $_.name -match 'x64' } | Sort-Object name -Descending | Select-Object -First 1
    if (-not $bin) { throw "no win-cuda-x64 asset in release $($rel.tag_name); pick one manually from $($rel.html_url)" }
    $binZip = Join-Path $ToolsDir $bin.name
    Get-File $bin.browser_download_url $binZip
    $llamaDir = Join-Path $ToolsDir 'llama'
    if (-not (Test-Path (Join-Path $llamaDir 'llama-server.exe'))) {
        Expand-Archive -Force $binZip $llamaDir
    }
    if ($cudart) {
        $cudartZip = Join-Path $ToolsDir $cudart.name
        Get-File $cudart.browser_download_url $cudartZip
        # cudart DLLs must sit next to llama-server.exe
        Expand-Archive -Force $cudartZip $llamaDir
    }
    $exe = Get-ChildItem -Recurse $llamaDir -Filter llama-server.exe | Select-Object -First 1
    if (-not $exe) { throw 'llama-server.exe not found after extraction' }
    Write-Host "  llama-server: $($exe.FullName)"
}

if (-not $SkipModels) {
    Write-Host "== GGUF models =="
    $all = Get-Content (Join-Path $Root 'models.json') | ConvertFrom-Json
    foreach ($m in $all) {
        if ($m.id -eq 'lexical') { continue }
        if ($Models -and $m.id -notin $Models) { continue }
        Write-Host "-- $($m.id) ($($m.gguf_repo))"
        $dest = Join-Path $ModelsDir "$($m.id).gguf"
        if (Test-Path $dest) { Write-Host "  exists: $($m.id).gguf"; continue }
        $file = $m.gguf_file
        if (-not $file) {
            # Resolve a filename from the repo listing: prefer the configured
            # quant hint, then Q8_0, then F16, then the first .gguf.
            $tree = Invoke-RestMethod "https://huggingface.co/api/models/$($m.gguf_repo)/tree/main"
            $ggufs = @($tree | Where-Object { $_.path -like '*.gguf' })
            if (-not $ggufs) { Write-Warning "no .gguf in $($m.gguf_repo); skipping"; continue }
            foreach ($hint in @($m.quant_hint, 'Q8_0', 'F16', '')) {
                $pick = $ggufs | Where-Object { $_.path -match [regex]::Escape([string]$hint) } | Select-Object -First 1
                if ($pick) { break }
            }
            $file = $pick.path
        }
        Get-File "https://huggingface.co/$($m.gguf_repo)/resolve/main/$file" $dest
    }
}
Write-Host 'setup complete.'
