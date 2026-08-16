---
name: lore-ignore
description: Tune a repository's .loreignore so Lore indexes authored content and not build output, vendored code, data blobs, or secrets. Use after `lore add` registers a new project, when Lore search returns noise, when the index is far larger than the repo's real source, or when the user says "tune the loreignore", "what is lore indexing", or "/lore-ignore".
---

# Tuning a project's `.loreignore`

`lore init` (and the daemon, on first scan) writes a `.loreignore` from **marker
detection only**: it sees `Cargo.toml` and writes `target/`, sees a Unity
project and writes `Library/`. That baseline is correct and it is nowhere near
sufficient. Everything repo-specific — vendored third-party trees, model
weights, corpora, serialized scene files, scratch directories, plaintext
credentials — is invisible to marker detection and has to be found by looking.

Your job is that second pass. Measure the repo, judge what you find, append to
the file, report what you did.

## 1. Ground yourself

Read the existing `.loreignore` at the project root. If there isn't one, run
`lore init` first and re-read.

The generated header block at the top is Lore's ecosystem baseline. **Do not
edit or reorder it.** Everything you add goes at the end, under your own
commented heading.

Also read `.gitignore`. Lore already honours VCS ignore rules, so anything
`.gitignore` excludes is already excluded — do not restate it. What you are
hunting for is the stuff that *is* committed (or is untracked-but-present) and
still should not be indexed.

## 2. Measure — never guess from the directory listing

Get real numbers before deciding anything. On Windows/PowerShell:

```powershell
# Largest files in the tree
Get-ChildItem -Recurse -File -EA SilentlyContinue |
  Sort-Object Length -Descending | Select-Object -First 40 FullName, Length

# Bytes by extension — where the weight actually is
Get-ChildItem -Recurse -File -EA SilentlyContinue |
  Group-Object Extension |
  Select-Object Name, Count, @{n='MB';e={[math]::Round(($_.Group|Measure-Object Length -Sum).Sum/1MB,1)}} |
  Sort-Object MB -Descending | Select-Object -First 30

# Heaviest directories
Get-ChildItem -Recurse -Directory -EA SilentlyContinue | ForEach-Object {
  [PSCustomObject]@{ Dir=$_.FullName
    MB=[math]::Round((Get-ChildItem $_.FullName -Recurse -File -EA SilentlyContinue |
        Measure-Object Length -Sum).Sum/1MB,1) }
} | Sort-Object MB -Descending | Select-Object -First 25
```

POSIX equivalents: `du -ah . | sort -rh | head -40`, and `find . -type f |
sed 's/.*\.//' | sort | uniq -c | sort -rn | head -30`.

Then **open a sample** of anything you are unsure about. A 4MB `.txt` might be
a design document or it might be a word list; the extension will not tell you
and the filename often lies.

## 3. Judge against these categories

For each heavy or numerous thing you found, ask which of these it is:

- **Vendored / third-party code.** Package caches, bundled SDKs, plugin drops,
  `Assets/Packages/`, `vendor/`, checked-in `node_modules` siblings. Nobody
  authored it here, and it drowns real hits.
- **Generated project and build files** the baseline missed — `*.csproj`,
  `*.sln`, lockfiles-as-noise, `test-results.xml`, coverage output, transpiled
  mirrors of source that is already indexed.
- **Serialized engine/tool formats.** Unity `*.unity`/`*.prefab`/`*.asset`,
  scene graphs, `.blend`, editor state. These are YAML or binary describing
  structure, not prose or code. If a subset genuinely carries authored intent
  (hand-written config assets), exclude the glob and `!`-re-include that path.
- **Data blobs and corpora.** Word lists, frequency tables, dictionaries,
  `*.csv`/`*.xlsx`/`*.jsonl`, fixtures measured in MB, `*.db`/`*.sqlite`,
  vector stores, ONNX/GGUF/safetensors weights, tokenizer files. This is
  usually the single biggest win and the one marker detection can never make.
- **Media and archives.** `*.zip`, `*.tar.gz`, screenshot dumps, texture
  libraries, audio banks, captured video.
- **Scratch and superseded work.** `tools/` graveyards, one-off scripts,
  abandoned experiments, `old/`, `_archive/`. Prefer scoping *down* to what is
  alive rather than enumerating what is dead — see §4.
- **Environment and editor state with non-standard names.** The baseline covers
  `venv/` and `.venv/`; it does not cover `.linux-venv/`, `env311/`,
  `.obsidian/`, `.idea/` and friends. Look for what this repo actually named
  them.
- **Secrets.** `*api_key*`, `*.pem`, `*.pfx`, `.env`, `credentials.json`,
  token files. **Report every one you find, loudly and by path, in addition to
  excluding it.** A credential sitting in a repo is a problem whether or not
  Lore indexes it, and quietly adding a pattern hides that from the user. Never
  paste the secret's contents into your report.

## 4. What not to exclude, and how to be reversible

The failure modes are not symmetric, but neither is free:

- **Over-excluding is silent.** The file simply never appears in search results
  and nobody learns why. When genuinely unsure whether something is authored,
  **keep it** and say so in your report.
- **Under-excluding is noisy but visible** — bad hits, wasted embedding cost.

Never exclude: source in the project's own languages, documentation and design
notes, tests, small fixtures that show intent, configuration that documents how
the system is wired, or the design vault.

Prefer a **scope-down with re-includes** over a long list of exclusions when a
directory is mostly dead:

```gitignore
tools/*
!tools/affinity_model/
!tools/affinity_model/**
tools/affinity_model/data/
```

Order matters: later lines win. Remember the syntax — patterns are unanchored
(`Library/` matches at any depth), a trailing `/` matches directories only, and
`!` re-includes.

## 5. Write it

Append one block at the end of `.loreignore`. Comment **why**, and date it, so
the next person can tell your judgment from Lore's generated baseline:

```gitignore
# Hand-tuned 2026-08-16: vendored Unity packages, ONNX weights and the word
# lists under data/ — large, not authored here, and they drown real hits.
# Delete a line (or !-re-include) to bring something back.
Assets/Packages/
*.onnx
enable1.txt
```

Group by rationale, not alphabetically. One comment per group beats one per
line. Do not rewrite or reflow anything already in the file.

## 6. Verify, then report

Re-index and check the result actually moved:

```
lore index <project>
lore status --project <project>
```

Then report to the user:

- **Excluded, by category, with the reason** and roughly how much it removed.
- **Secrets found**, by path, called out separately and first if any exist.
- **Judgment calls you made** — anything you excluded that a reasonable person
  might want indexed, and anything you deliberately kept despite its size.
- **What you did not decide** — leave open questions open rather than resolving
  them silently.

The user reviews your block as a normal diff. `.loreignore` is committed, so it
follows the repo to every machine and contributor.
