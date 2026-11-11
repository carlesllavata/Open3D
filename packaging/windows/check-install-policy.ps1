# SPDX-License-Identifier: GPL-3.0-or-later
<#
.SYNOPSIS
  Check the compiled launcher MSI's installation policy without installing anything.
.DESCRIPTION
  Uses Windows Installer's safe session (OpenPackage option 1). Only property-setting custom
  actions are exercised. No AppSearch, install, repair, removal or file-writing action runs.
  Checks: per-user scope, the UpgradeCode, the default folder under the per-user Open3D suite
  folder, that an update keeps the previous or an explicitly chosen folder (in the wizard and in
  silent updates), and the HKCU registration the launcher and the next installer read back.
#>
param([Parameter(Mandatory = $true)] [string] $MsiPath)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$policy = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'install-policy.json') | ConvertFrom-Json
if ($policy.mode -ne 'perUser') { throw 'Open3D Launcher installs per user only.' }
function Read-Rows($Database, [string] $Sql, [int] $Columns) {
  $view = $Database.OpenView($Sql)
  try {
    [void]$view.Execute()
    while ($null -ne ($record = $view.Fetch())) {
      try {
        $values = @(for ($i = 1; $i -le $Columns; $i++) {
          $record.GetType().InvokeMember('StringData', 'GetProperty', $null, $record, @($i))
        })
        ,$values
      } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($record) }
    }
  } finally {
    [void]$view.Close()
    [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($view)
  }
}
function Set-SessionProperty($Session, [string] $Name, [string] $Value) {
  [void]$Session.GetType().InvokeMember('Property', 'SetProperty', $null, $Session, @($Name, $Value))
}
function Get-SessionProperty($Session, [string] $Name) {
  $Session.GetType().InvokeMember('Property', 'GetProperty', $null, $Session, @($Name))
}
$installer = $null; $database = $null; $session = $null
try {
  $installer = New-Object -ComObject WindowsInstaller.Installer
  $file = (Resolve-Path -LiteralPath $MsiPath).Path
  $database = $installer.OpenDatabase($file, 0)
  $session = $installer.OpenPackage($file, 1)
  $properties = @{}
  foreach ($row in (Read-Rows $database 'SELECT `Property`, `Value` FROM `Property`' 2)) { $properties[$row[0]] = $row[1] }
  if ('INSTALLFOLDER' -notin @($properties['SecureCustomProperties'] -split ';')) {
    throw 'The selected installation folder must survive elevation (secure INSTALLFOLDER).'
  }
  if ($properties.ContainsKey('ALLUSERS')) { throw 'The launcher must stay per-user, so it can update itself without elevation.' }
  if ($properties['UpgradeCode'].Trim('{}') -ne $policy.upgradeCode) { throw 'The launcher upgrade identity changed.' }
  $dirs = @{}
  foreach ($row in (Read-Rows $database 'SELECT `Directory`, `Directory_Parent`, `DefaultDir` FROM `Directory`' 3)) { $dirs[$row[0]] = @($row[1], $row[2]) }
  if ($dirs['INSTALLFOLDER'][0] -ne 'Open3DSuiteFolder' -or $dirs['Open3DSuiteFolder'][1] -ne 'Open3D' -or
      $dirs['Open3DSuiteFolder'][0] -ne 'LocalProgramsFolder' -or $dirs['LocalProgramsFolder'][0] -ne 'LocalAppDataFolder') {
    throw 'A new install must go under the per-user Open3D suite folder.'
  }
  foreach ($explicit in @('', 'F:\Chosen exact launcher')) {
    Set-SessionProperty $session 'PREVIOUSINSTALLFOLDER' 'E:\Existing exact launcher'
    Set-SessionProperty $session 'INSTALLFOLDER' $explicit
    $row = @(Read-Rows $database 'SELECT `Condition` FROM `InstallUISequence` WHERE `Action` = ''Open3DRestoreLauncherFolder''' 1)[0]
    if ($session.EvaluateCondition($row[0]) -eq 1) {
      if ($session.DoAction('Open3DRestoreLauncherFolder') -ne 1) { throw 'The folder restore action failed.' }
    }
    $expected = if ($explicit) { $explicit } else { 'E:\Existing exact launcher' }
    if ((Get-SessionProperty $session 'INSTALLFOLDER') -ne $expected) { throw 'A chosen or existing folder was lost.' }
  }
  $rows = @(Read-Rows $database 'SELECT `Root`, `Key`, `Name`, `Value` FROM `Registry`' 4)
  if (@($rows | Where-Object { $_[0] -ne '1' }).Count) { throw 'The launcher registers under the current account (HKCU) only.' }
  foreach ($entry in $policy.registry) {
    if (@($rows | Where-Object { $_[0] -eq $entry[0] -and $_[1] -eq $entry[1] -and $_[2] -eq $entry[2] -and $_[3] -eq $entry[3] }).Count -ne 1) {
      throw "Registration changed unexpectedly: $($entry[1]) / $($entry[2])"
    }
  }
  # Silent and reduced-UI updates never show the wizard, so the folder must also be restored in the execute sequence.
  $execute = @{}
  foreach ($row in (Read-Rows $database 'SELECT `Action`, `Sequence` FROM `InstallExecuteSequence`' 2)) { $execute[$row[0]] = [int]$row[1] }
  if (-not ($execute.ContainsKey('Open3DRestoreLauncherFolder') -and $execute['AppSearch'] -lt $execute['Open3DRestoreLauncherFolder'] -and
      $execute['Open3DRestoreLauncherFolder'] -lt $execute['CostInitialize'])) {
    throw 'Silent updates must restore the launcher folder after AppSearch and before directory costing.'
  }
  Write-Output 'PASS Open3D Launcher: per-user scope, suite folder, remembered and chosen folders, registration. Nothing was installed.'
} finally {
  foreach ($com in @($session, $database, $installer)) {
    if ($null -ne $com) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($com) }
  }
}
