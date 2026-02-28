<#
.SYNOPSIS
    Pisci Desktop — Release Gate Smoke-Test Script
.DESCRIPTION
    Runs a lightweight battery of checks to verify the project is in a
    releasable state.  Exit code 0 means all checks passed; non-zero means
    at least one check failed.

    Checks performed:
      1. Rust unit tests  (cargo test --lib)
      2. Frontend unit tests  (npm test)
      3. TypeScript type-check  (tsc --noEmit)
      4. Rust lint  (cargo clippy -- -D warnings)  [optional, skipped if -SkipClippy]
      5. Frontend lint  (eslint)  [skipped if no .eslintrc / eslint.config.js]
      6. Build artefact existence after  cargo build --release

.PARAMETER SkipClippy
    Skip the cargo clippy step (useful on slow CI agents).
.PARAMETER SkipBuild
    Skip the release build step (saves 5–10 min on developer machines).
#>
param(
    [switch]$SkipClippy,
    [switch]$SkipBuild
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$Root   = Split-Path -Parent $PSScriptRoot
$Tauri  = Join-Path $Root "src-tauri"
$Passed = 0
$Failed = 0
$Results = @()

function Step([string]$Name, [scriptblock]$Body) {
    Write-Host ""
    Write-Host "─── $Name " -ForegroundColor Cyan -NoNewline
    Write-Host ("─" * ([Math]::Max(0, 60 - $Name.Length))) -ForegroundColor DarkGray
    try {
        & $Body
        Write-Host "  PASS" -ForegroundColor Green
        $script:Passed++
        $script:Results += [PSCustomObject]@{ Step=$Name; Status="PASS"; Detail="" }
    }
    catch {
        $msg = $_.ToString()
        Write-Host "  FAIL: $msg" -ForegroundColor Red
        $script:Failed++
        $script:Results += [PSCustomObject]@{ Step=$Name; Status="FAIL"; Detail=$msg }
    }
}

# ─── 1. Rust unit tests ───────────────────────────────────────────────────────
Step "Rust unit tests (cargo test --lib)" {
    Push-Location $Tauri
    try {
        $out = cargo test --lib 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw ($out | Select-String "FAILED|error\[" | Out-String).Trim()
        }
    }
    finally { Pop-Location }
}

# ─── 2. Frontend unit tests ───────────────────────────────────────────────────
Step "Frontend unit tests (vitest)" {
    Push-Location $Root
    try {
        $out = npm test 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw ($out | Select-String "FAIL|Error" | Out-String).Trim()
        }
    }
    finally { Pop-Location }
}

# ─── 3. TypeScript type-check ─────────────────────────────────────────────────
Step "TypeScript type-check (tsc --noEmit)" {
    Push-Location $Root
    try {
        $out = npx tsc --noEmit 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw ($out | Out-String).Trim()
        }
    }
    finally { Pop-Location }
}

# ─── 4. Rust clippy ───────────────────────────────────────────────────────────
if (-not $SkipClippy) {
    Step "Rust lint (cargo clippy)" {
        Push-Location $Tauri
        try {
            $out = cargo clippy --lib -- -D warnings 2>&1
            if ($LASTEXITCODE -ne 0) {
                throw ($out | Select-String "error\[" | Out-String).Trim()
            }
        }
        finally { Pop-Location }
    }
}

# ─── 5. Frontend ESLint (optional) ───────────────────────────────────────────
$eslintConfig = @(
    (Join-Path $Root ".eslintrc.json"),
    (Join-Path $Root ".eslintrc.js"),
    (Join-Path $Root "eslint.config.js"),
    (Join-Path $Root "eslint.config.mjs")
) | Where-Object { Test-Path $_ }

if ($eslintConfig.Count -gt 0) {
    Step "Frontend lint (eslint)" {
        Push-Location $Root
        try {
            $out = npx eslint src --ext .ts,.tsx --max-warnings 0 2>&1
            if ($LASTEXITCODE -ne 0) {
                throw ($out | Out-String).Trim()
            }
        }
        finally { Pop-Location }
    }
}
else {
    Write-Host ""
    Write-Host "  (skipping ESLint — no config file found)" -ForegroundColor DarkGray
}

# ─── 6. Release build artefact ────────────────────────────────────────────────
if (-not $SkipBuild) {
    Step "Cargo release build" {
        Push-Location $Tauri
        try {
            $out = cargo build --release 2>&1
            if ($LASTEXITCODE -ne 0) {
                throw ($out | Select-String "error\[" | Out-String).Trim()
            }
            # Verify the binary exists
            $exe = Join-Path $Tauri "target\release\pisci-desktop.exe"
            if (-not (Test-Path $exe)) {
                throw "Expected release binary not found: $exe"
            }
        }
        finally { Pop-Location }
    }
}

# ─── Summary ──────────────────────────────────────────────────────────────────
Write-Host ""
Write-Host ("═" * 62) -ForegroundColor DarkGray
Write-Host " SMOKE TEST RESULTS" -ForegroundColor White
Write-Host ("═" * 62) -ForegroundColor DarkGray
foreach ($r in $Results) {
    $color = if ($r.Status -eq "PASS") { "Green" } else { "Red" }
    Write-Host ("  [{0}]  {1}" -f $r.Status, $r.Step) -ForegroundColor $color
}
Write-Host ("═" * 62) -ForegroundColor DarkGray
Write-Host ("  Passed: {0}   Failed: {1}" -f $Passed, $Failed) -ForegroundColor White
Write-Host ("═" * 62) -ForegroundColor DarkGray

if ($Failed -gt 0) {
    Write-Host ""
    Write-Host "  RELEASE GATE: BLOCKED ($Failed check(s) failed)" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "  RELEASE GATE: PASSED — ready to ship" -ForegroundColor Green
exit 0
