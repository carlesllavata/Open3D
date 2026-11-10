<#
.SYNOPSIS
    Builds a local test catalog from the app checkouts: one zip per bundle plus catalog.json.

.EXAMPLE
    powershell -NoProfile -File dev\make_test_catalog.ps1 -Out C:\dev\open3d-test
    open3d-launcher --catalog C:\dev\open3d-test\catalog.json --plugins-dir C:\dev\open3d-test\plugins

.NOTES
    Development only: the real catalog is written by tools/publish.py. Uses the bundles as they are in the
    checkouts (OpenManager: dist\OpenManager.bundle, so run its tools\build.py first).
#>
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [string]$Apps = ""
)

$ErrorActionPreference = "Stop"
if (-not $Apps) {
    # launcher\dev\ -> launcher\ -> suite root -> apps\  ($PSScriptRoot is empty in param defaults on PS 5.1)
    $here = Split-Path -Parent $MyInvocation.MyCommand.Path
    $Apps = Join-Path (Split-Path -Parent (Split-Path -Parent $here)) "apps"
}
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem

$defs = @(
    @{ id = "opentools"; name = "OpenTools"; summary = "Tool hub for 3ds Max: scene clean-up, materials, clipboard and more.";
       bundle = "OpenTools.bundle"; src = "OpenTools\OpenTools.bundle"; changelog = "OpenTools\CHANGELOG.md" },
    @{ id = "openmanager"; name = "OpenManager"; summary = "Scene setups and batch rendering for 3ds Max.";
       bundle = "OpenManager.bundle"; src = "OpenManager\dist\OpenManager.bundle"; changelog = "OpenManager\CHANGELOG.md" }
)

function Get-AppVersion([string]$bundleDir) {
    $xml = [IO.File]::ReadAllText((Join-Path $bundleDir "PackageContents.xml"))
    if ($xml -match '\sAppVersion\s*=\s*"([^"]+)"') { return $Matches[1] }
    throw "No AppVersion in $bundleDir"
}

# The first released section ("## 0.5.0 — date"), without its heading.
function Get-Notes([string]$changelog, [string]$version) {
    $lines = [IO.File]::ReadAllLines($changelog, [Text.Encoding]::UTF8)
    $out = New-Object System.Collections.Generic.List[string]
    $inside = $false
    foreach ($l in $lines) {
        if ($l -match '^## ') {
            if ($inside) { break }
            if ($l -match ('^## ' + [regex]::Escape($version) + '\b')) { $inside = $true }
            continue
        }
        if ($inside) { $out.Add($l) }
    }
    return ($out -join "`n").Trim()
}

function New-BundleZip([string]$bundleDir, [string]$bundleName, [string]$zipPath) {
    if (Test-Path $zipPath) { Remove-Item $zipPath }
    $zip = [IO.Compression.ZipFile]::Open($zipPath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        $root = (Resolve-Path $bundleDir).Path.TrimEnd('\')
        Get-ChildItem $root -Recurse -File | Where-Object { $_.FullName -notmatch '\\__pycache__\\' } | ForEach-Object {
            # Zip entries always use forward slashes.
            $rel = $bundleName + "/" + $_.FullName.Substring($root.Length + 1).Replace('\', '/')
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $_.FullName, $rel, [IO.Compression.CompressionLevel]::Optimal)
        }
    } finally { $zip.Dispose() }
}

New-Item -ItemType Directory -Force -Path $Out | Out-Null
$entries = @()
foreach ($d in $defs) {
    $src = Join-Path $Apps $d.src
    $version = Get-AppVersion $src
    $zipName = "$($d.name)-$version.zip"
    $zipPath = Join-Path $Out $zipName
    New-BundleZip $src $d.bundle $zipPath
    $entries += [ordered]@{
        id = $d.id; name = $d.name; summary = $d.summary; bundle = $d.bundle; max_min = 2025
        latest = [ordered]@{
            version = $version
            date    = (Get-Date -Format "yyyy-MM-dd")
            url     = $zipName
            sha256  = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLower()
            size    = (Get-Item $zipPath).Length
            notes   = (Get-Notes (Join-Path $Apps $d.changelog) $version)
        }
    }
    Write-Host ("{0,-12} {1,-7} {2,8:N0} KB" -f $d.name, $version, ((Get-Item $zipPath).Length / 1KB))
}

$catalog = [ordered]@{ schema = 1; apps = $entries }
$json = $catalog | ConvertTo-Json -Depth 6
[IO.File]::WriteAllText((Join-Path $Out "catalog.json"), $json, (New-Object Text.UTF8Encoding($false)))
Write-Host "Catalog: $(Join-Path $Out 'catalog.json')"
