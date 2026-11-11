# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
  Build non-executable MSI metadata fixtures of the launcher and check their policy; never install them.
.DESCRIPTION
  The payload files are placeholders, so the MSIs can't install anything useful. They check the
  compiled paths, identity and registration (check-install-policy.ps1) for each architecture,
  without a release build. Needs WiX 5.0.2 with its UI and Util extensions.
#>
param(
  [Parameter(Mandatory = $true)] [string] $OutputDirectory,
  [ValidateSet('x64','x86','arm64')] [string[]] $Architectures = @('x64','x86','arm64')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).ProviderPath
$policy = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'install-policy.json') | ConvertFrom-Json
[void](New-Item -ItemType Directory -Force -Path $OutputDirectory)
$output = (Resolve-Path -LiteralPath $OutputDirectory).ProviderPath
Add-Type -AssemblyName System.Drawing
foreach ($arch in $Architectures) {
  $stage = Join-Path $output $arch
  [void](New-Item -ItemType Directory -Force -Path $stage)
  foreach ($name in $policy.payloads) {
    Set-Content -LiteralPath (Join-Path $stage $name) -Encoding Ascii -Value 'NON-EXECUTABLE MSI METADATA TEST FIXTURE'
  }
  foreach ($spec in @(@('banner.bmp', 493, 58), @('dialog.bmp', 493, 312))) {
    $bitmap = [Drawing.Bitmap]::new($spec[1], $spec[2], [Drawing.Imaging.PixelFormat]::Format24bppRgb)
    try { $bitmap.Save((Join-Path $stage $spec[0]), [Drawing.Imaging.ImageFormat]::Bmp) }
    finally { $bitmap.Dispose() }
  }
  $sources = @($policy.sources | ForEach-Object { Join-Path $PSScriptRoot $_ })
  $msi = Join-Path $stage "$($policy.id)-NOT-INSTALLABLE.msi"
  $extensions = @($policy.extensions | ForEach-Object { '-ext'; "WixToolset.$_.wixext" })
  & wix build @sources -arch $arch @extensions `
    -culture en-US -d 'Version=0.0.1' -d 'DisplayVersion=0.0.1-metadata-test' `
    -d "BinDir=$stage" -d "IconPath=$(Join-Path $root $policy.icon)" `
    -d "BannerBmp=$(Join-Path $stage 'banner.bmp')" -d "DialogBmp=$(Join-Path $stage 'dialog.bmp')" `
    -o $msi @($policy.localization | ForEach-Object { '-loc'; Join-Path $PSScriptRoot $_ })
  if ($LASTEXITCODE -ne 0) { throw "WiX build failed for $arch (exit $LASTEXITCODE)." }
  & (Join-Path $PSScriptRoot 'check-install-policy.ps1') -MsiPath $msi
  Write-Output "PASS $arch metadata-only package. This is not an executable installer."
}
