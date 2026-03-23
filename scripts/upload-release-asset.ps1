# Upload NSIS installer to an existing GitHub release (same version, replace asset).
# Usage:
#   $env:GITHUB_TOKEN = "ghp_xxxx"
#   .\scripts\upload-release-asset.ps1 -Tag v0.5.16 -File "target\release\bundle\nsis\OpenPisci_0.5.16_x64-setup.exe"
param(
    [string]$Repo = "njbinbin-pisci/openpisci",
    [Parameter(Mandatory = $true)][string]$Tag,
    [Parameter(Mandatory = $true)][string]$File
)
$token = $env:GITHUB_TOKEN
if (-not $token) {
    Write-Error "Set GITHUB_TOKEN (classic PAT with repo scope)."
    exit 1
}
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$base = "https://api.github.com/repos/$Repo"
$hdr = @{ Authorization = "token $token"; "User-Agent" = "pisci-upload"; Accept = "application/vnd.github+json" }

$releases = Invoke-RestMethod -Uri "$base/releases" -Headers $hdr
$rel = $releases | Where-Object { $_.tag_name -eq $Tag } | Select-Object -First 1
if (-not $rel) { Write-Error "Release $Tag not found."; exit 1 }

$fileName = [System.IO.Path]::GetFileName((Resolve-Path $File))
$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path $File))

# Remove existing asset with same name
foreach ($a in $rel.assets) {
    if ($a.name -eq $fileName) {
        Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/assets/$($a.id)" -Method DELETE -Headers $hdr | Out-Null
        Write-Host "Removed old asset: $fileName"
    }
}

$uploadUrl = "https://uploads.github.com/repos/$Repo/releases/$($rel.id)/assets?name=$([uri]::EscapeDataString($fileName))"
$result = Invoke-RestMethod -Uri $uploadUrl -Method POST `
    -Headers @{ Authorization = "token $token"; "Content-Type" = "application/octet-stream"; "User-Agent" = "pisci-upload" } `
    -Body $bytes
Write-Host "Uploaded: $($result.browser_download_url)"
