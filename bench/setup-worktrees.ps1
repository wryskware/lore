# Bench worktree / project setup. RUN THIS ON THE LIVE MACHINE, by hand.
#
# It creates the *second* working tree per bench repo (slot 'b') and registers
# it with the lore daemon as its own project, so the two arms of a T5 cell can
# run at the same time without fighting over files. Slot 'a' is the round-1
# tree and is left completely alone.
#
#   .\setup-worktrees.ps1                    # DRY RUN — prints the plan, changes nothing
#   .\setup-worktrees.ps1 -Apply             # create + register slot 'b'
#   .\setup-worktrees.ps1 -Apply -PinBinary  # also pin lore-mcp.exe for the round
#   .\setup-worktrees.ps1 -Apply -Scrub      # delete answer-key material from BOTH slots
#   .\setup-worktrees.ps1 -Apply -Authority  # write the [authority] profile (round 2)
#   .\setup-worktrees.ps1 -WaitDrained       # poll `lore status` until every
#                                            # bench project is 100% embedded
#
# Nothing here is destructive: every step refuses rather than overwrites.
#
# Lexomancy slot 'b' needs a second Unity VCS workspace, which this script will
# NOT create — `cm` workspace management is a hand operation in this project
# (see the Lexomancy repo's own CLAUDE.md / uvcs-hygiene rules). The script
# checks for it and prints the commands if it is missing.
[CmdletBinding()]
param(
    [switch]$Apply,
    [switch]$PinBinary,
    [switch]$WaitDrained,
    # Round 2: delete answer-key material out of the corpus under test, and
    # give the lore tree the authority profile the real repo runs under.
    # Both act on BOTH slots, including the round-1 slot 'a'.
    [switch]$Scrub,
    [switch]$Authority,
    [ValidateSet('lore', 'terrarium', 'lexomancy')] [string[]]$Repos = @('lore', 'terrarium', 'lexomancy'),
    # Source for the pinned lore-mcp binary. Build it yourself first; this only
    # copies, so the round's binary cannot drift when the live checkout is
    # rebuilt mid-round.
    [string]$McpSource = 'C:\Users\perag\wryskware\lore\target\debug\lore-mcp.exe',
    [int]$DrainTimeoutMinutes = 240
)

$ErrorActionPreference = 'Stop'

$benchBin = 'C:\Users\perag\bench-e2e\bin'
$McpPinned = Join-Path $benchBin 'lore-mcp.exe'

# Pins are the round-1 frozen pins. DO NOT move them without re-freezing the
# answer keys: every key in design/6_Evaluation is written against this state.
$plan = @(
    [pscustomobject]@{
        Repo = 'lore'; Kind = 'git'
        Source = 'C:\Users\perag\wryskware\lore'
        Pin = '977364a'
        Dir = 'C:\Users\perag\bench-e2e\lore-bench-b'
        Project = 'lore-bench-b'
    }
    [pscustomobject]@{
        Repo = 'terrarium'; Kind = 'git'
        Source = 'C:\Users\perag\wryskware\latent-music-terrarium'
        Pin = '3b1eacd56f'
        Dir = 'C:\Users\perag\bench-e2e\terrarium-bench-b'
        Project = 'terrarium-bench-b'
    }
    [pscustomobject]@{
        Repo = 'lexomancy'; Kind = 'junction'
        Source = 'C:\Users\perag\Unity\Lexomancy-bench'
        Pin = 'cs:134'
        Dir = 'C:\Users\perag\Unity\Lexomancy-bench-b'
        # Retrieval for BOTH Lexomancy slots targets the main `Lexomancy`
        # project: the walker does not follow junctions, so a bench root
        # indexes only its own two loose files. The second tree exists for
        # file isolation during T5, not for a second index. It is still
        # registered, because slot 'a' is: an unregistered cwd resolves to no
        # project at all, and the two slots must present the agent with the
        # same default-resolution behaviour or the arms are not comparable.
        Project = 'Lexomancy-bench-b'
        CmWorkspace = 'C:\Users\perag\Unity\Lexomancy-alt-b'
    }
) | Where-Object { $Repos -contains $_.Repo }

function Step([string]$text) { Write-Host "  $text" -ForegroundColor Gray }
function Did([string]$text) { Write-Host "  + $text" -ForegroundColor Green }
function Skip([string]$text) { Write-Host "  = $text" -ForegroundColor DarkGray }
function Todo([string]$text) { Write-Host "  ! $text" -ForegroundColor Yellow }

if (-not $Apply -and -not $WaitDrained) {
    Write-Host 'DRY RUN — nothing will be changed. Re-run with -Apply.' -ForegroundColor Yellow
}

# --- 0. Round-2 corpus hygiene ------------------------------------------
# Both slot 'a' (the round-1 tree) and slot 'b'.
$slotDirs = @{
    lore      = @('C:\Users\perag\bench-e2e\lore-bench', 'C:\Users\perag\bench-e2e\lore-bench-b')
    terrarium = @('C:\Users\perag\bench-e2e\terrarium-bench', 'C:\Users\perag\bench-e2e\terrarium-bench-b')
    lexomancy = @('C:\Users\perag\Unity\Lexomancy-bench', 'C:\Users\perag\Unity\Lexomancy-bench-b')
}

# Paths that exist at the pin and LEAK THE ANSWER KEY: the round-1 plan doc
# carries the task list and the graded answers for all three repos. Keep in
# sync with $scrubbed in run.ps1, which refuses to run a cell while one exists.
$scrubPaths = @{
    lore      = @('design/9_Scratch/2026-08-15_e2e-round-1-plan.md')
    terrarium = @()
    lexomancy = @()
}

# Which authority profile each bench project should index under. At the pins
# none of these repos had a committed .lore.toml, so round 1 indexed them all
# with `authority: none`. Round 2 restores the lore repo's real configuration
# (its own vault is the thing its T2/T3 tasks interrogate). Terrarium is left
# neutral on purpose: it has no `design_status` convention and no ledger, so a
# profile there would declare a policy the corpus does not follow. Lexomancy is
# retrieved from the main `Lexomancy` root, which already runs lore-v1 (rank).
$authorityProfile = @{
    lore      = @{ profile = 'lore-v1'; behavior = 'rank' }
    terrarium = $null
    lexomancy = $null
}

if ($Scrub) {
    Write-Host 'Scrubbing answer-key material from the bench trees' -ForegroundColor Cyan
    foreach ($repo in $Repos) {
        foreach ($rel in @($scrubPaths[$repo])) {
            foreach ($dir in $slotDirs[$repo]) {
                if (-not (Test-Path -LiteralPath $dir)) { continue }
                $full = Join-Path $dir $rel
                if (-not (Test-Path -LiteralPath $full)) {
                    Skip "$full already absent"
                    continue
                }
                Step "remove $full"
                if ($Apply) {
                    Remove-Item -LiteralPath $full -Force
                    Did "scrubbed $full"
                }
            }
        }
    }
}

if ($Authority) {
    Write-Host 'Applying authority profiles' -ForegroundColor Cyan
    foreach ($repo in $Repos) {
        $prof = $authorityProfile[$repo]
        if (-not $prof) {
            Skip "$repo : no profile on purpose (see the comment in this script)"
            continue
        }
        foreach ($dir in $slotDirs[$repo]) {
            if (-not (Test-Path -LiteralPath $dir)) { continue }
            $tomlPath = Join-Path $dir '.lore.toml'
            if (-not (Test-Path -LiteralPath $tomlPath)) {
                Todo "$tomlPath does not exist yet — register the project first, then re-run."
                continue
            }
            $body = Get-Content -LiteralPath $tomlPath -Raw
            if ($body -match '(?m)^\s*\[authority\]') {
                Skip "$tomlPath already declares [authority]"
                continue
            }
            Step "append [authority] profile=$($prof.profile) behavior=$($prof.behavior) to $tomlPath"
            if ($Apply) {
                $append = @(
                    '',
                    '# Round 2: index under the profile this repo really uses, so the',
                    '# authority/modality tasks run against annotated + ranked results.',
                    '[authority]',
                    "profile = `"$($prof.profile)`"",
                    "behavior = `"$($prof.behavior)`""
                ) -join "`n"
                Add-Content -LiteralPath $tomlPath -Value $append -Encoding utf8
                Did "wrote [authority] into $tomlPath"
                Todo 'A profile flip re-chunks Markdown (it does not re-embed). Re-run with -WaitDrained.'
            }
        }
    }
}

# --- 1. Pin the lore-mcp binary ------------------------------------------
# The arm configs point at $McpPinned, not at the live target\debug copy, so a
# rebuild of the working checkout (D-0015 is landing) cannot re-pin a round
# that is already in flight. run.ps1 records the binary's SHA-256 into every
# cell's metrics.json, which is what makes the pin auditable after the fact.
if ($PinBinary) {
    Write-Host 'Pinning lore-mcp binary' -ForegroundColor Cyan
    if (-not (Test-Path -LiteralPath $McpSource)) {
        throw "no such binary: $McpSource — build it first (cargo build -p lore-mcp)"
    }
    Step "$McpSource  ->  $McpPinned"
    if ($Apply) {
        New-Item -ItemType Directory -Force $benchBin | Out-Null
        Copy-Item -LiteralPath $McpSource -Destination $McpPinned -Force
        Did "pinned  sha256 $((Get-FileHash -LiteralPath $McpPinned -Algorithm SHA256).Hash)"
    }
}

# --- 2. Create and register slot 'b' -------------------------------------
foreach ($p in $plan) {
    Write-Host "$($p.Repo): slot 'b'" -ForegroundColor Cyan

    if ($p.Kind -eq 'git') {
        Step "git -C `"$($p.Source)`" worktree add --detach `"$($p.Dir)`" $($p.Pin)"
        if (Test-Path -LiteralPath $p.Dir) {
            Skip "$($p.Dir) already exists — leaving it alone"
        }
        elseif ($Apply) {
            git -C $p.Source worktree add --detach $p.Dir $p.Pin
            if ($LASTEXITCODE -ne 0) { throw "worktree add failed for $($p.Repo)" }
            Did "created $($p.Dir) at $($p.Pin)"
        }

        # Write .lore.toml BEFORE `lore add`, with a [project] table and
        # nothing else. Two reasons, both load-bearing:
        #   * D-0016 binds the declared name and REJECTS duplicates, so slot
        #     'b' must declare a name distinct from slot 'a'. Without this file
        #     `lore add` falls back to the directory basename, which happens to
        #     work here, but declaring it makes the identity explicit and
        #     survives a rename of the directory.
        #   * [project] only here. The [authority] table is a separate,
        #     explicit step (-Authority), because it is a deliberate round-2
        #     variable and it applies to slot 'a' too — see the top of this
        #     script for which repos get a profile and why.
        $tomlPath = Join-Path $p.Dir '.lore.toml'
        Step "write $tomlPath  ([project] only — [authority] comes from -Authority)"
        if (Test-Path -LiteralPath $tomlPath) {
            Skip "$tomlPath already exists — leaving it alone"
        }
        elseif ($Apply) {
            $body = @(
                '# Bench slot b. [project] only: the frozen pin had no .lore.toml,',
                '# and the [authority] table is written by -Authority so it stays a',
                '# deliberate, symmetric decision across both slots.',
                '[project]',
                "name = `"$($p.Project)`""
            ) -join "`n"
            Set-Content -LiteralPath $tomlPath -Value $body -Encoding utf8
            Did "wrote $tomlPath"
        }

        Step "lore add `"$($p.Dir)`" --name $($p.Project)"
        if ($Apply) {
            lore add $p.Dir --name $p.Project
            if ($LASTEXITCODE -ne 0) { throw "lore add failed for $($p.Project)" }
            Did "registered $($p.Project)"
        }
    }
    else {
        # Lexomancy: a junction tree over a SECOND cm workspace. The cm
        # workspace itself is a hand operation.
        if (-not (Test-Path -LiteralPath $p.CmWorkspace)) {
            Todo "missing second cm workspace $($p.CmWorkspace). Create it by hand, then re-run:"
            Todo "    cm workspace create Lexomancy-alt-b `"$($p.CmWorkspace)`" --repository=Lexomancy"
            Todo "    cm switch $($p.Pin) --workspace=`"$($p.CmWorkspace)`""
            Todo '  (exact invocation per the Lexomancy repo rules — never `cm diff <path>`.)'
            Todo '  Until it exists, run lexomancy T5 cells one arm at a time.'
            continue
        }
        Step "mkdir $($p.Dir)  + junctions Lexomancy/design/tools  + copy AGENTS.md/CLAUDE.md"
        if (Test-Path -LiteralPath $p.Dir) {
            Skip "$($p.Dir) already exists — leaving it alone"
        }
        elseif ($Apply) {
            New-Item -ItemType Directory -Force $p.Dir | Out-Null
            New-Item -ItemType Junction -Path (Join-Path $p.Dir 'Lexomancy') -Target $p.CmWorkspace | Out-Null
            New-Item -ItemType Junction -Path (Join-Path $p.Dir 'design') -Target 'C:\Users\perag\Unity\Lexomancy\design' | Out-Null
            New-Item -ItemType Junction -Path (Join-Path $p.Dir 'tools') -Target 'C:\Users\perag\Unity\Lexomancy\tools' | Out-Null
            foreach ($f in 'AGENTS.md', 'CLAUDE.md') {
                Copy-Item -LiteralPath (Join-Path $p.Source $f) -Destination (Join-Path $p.Dir $f)
            }
            Did "built $($p.Dir)"
        }
        # Registered for resolution symmetry with slot 'a' only. No .lore.toml
        # is pre-written: slot 'a' has none either, and the file itself is not
        # indexed, so `lore add` deriving the name is the symmetric path.
        Step "lore add `"$($p.Dir)`" --name $($p.Project)"
        Step '  (retrieval still targets project=Lexomancy for both slots — junctions are not walked)'
        if ($Apply) {
            lore add $p.Dir --name $p.Project
            if ($LASTEXITCODE -ne 0) { throw "lore add failed for $($p.Project)" }
            Did "registered $($p.Project)"
        }
    }
}

# --- 3. Wait for the index to drain --------------------------------------
# CHUNK_FORMAT_VERSION moved 4 -> 5 on main, which invalidates every persisted
# chunk: the daemon re-chunks and RE-EMBEDS every registered project on the
# first pass with the new binary. Lexomancy alone is ~326k chunks. A round
# started before this settles measures a cold index, not the product.
if ($WaitDrained) {
    Write-Host 'Waiting for every project to reach 100% embedded' -ForegroundColor Cyan
    $deadline = (Get-Date).AddMinutes($DrainTimeoutMinutes)
    while ($true) {
        $status = (lore status 2>&1) -join "`n"
        $pending = @([regex]::Matches($status, 'embedded\s+(\d+)/(\d+)') |
                Where-Object { $_.Groups[1].Value -ne $_.Groups[2].Value })
        if ($pending.Count -eq 0) {
            Write-Host $status
            Did 'all projects fully embedded'
            break
        }
        if ((Get-Date) -gt $deadline) {
            Write-Host $status
            throw "still $($pending.Count) project(s) embedding after $DrainTimeoutMinutes min"
        }
        Write-Host ("  {0} project(s) still embedding..." -f $pending.Count) -ForegroundColor DarkGray
        Start-Sleep -Seconds 30
    }
}

Write-Host ''
Write-Host 'Next: the "Round-2 setup" section of bench\README.md for the rest of the run-day checklist.' -ForegroundColor Cyan
