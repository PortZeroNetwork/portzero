#Requires -Version 5.1
param(
    [Parameter(Mandatory = $true)]
    [string] $Version,

    [Parameter(Mandatory = $true)]
    [string] $MsiPath,

    [string] $OutputRoot = (Join-Path $PSScriptRoot "..\packaging\winget\manifests\p\PortZeroNetwork\PortZero"),

    [string] $Repo = "PortZeroNetwork/portzero",

    [string] $UpgradeCode = "{8D64B464-96F7-4CB9-A452-372F0AC067AF}"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-MsiProperty {
    param(
        [string] $Path,
        [string] $Property
    )

    $installer = New-Object -ComObject WindowsInstaller.Installer
    $database = $installer.GetType().InvokeMember(
        "OpenDatabase",
        "InvokeMethod",
        $null,
        $installer,
        @($Path, 0)
    )
    $view = $database.GetType().InvokeMember(
        "OpenView",
        "InvokeMethod",
        $null,
        $database,
        @("SELECT Value FROM Property WHERE Property = '$Property'")
    )
    $view.GetType().InvokeMember("Execute", "InvokeMethod", $null, $view, $null) | Out-Null
    $record = $view.GetType().InvokeMember("Fetch", "InvokeMethod", $null, $view, $null)
    if (-not $record) {
        throw "MSI property '$Property' was not found in $Path"
    }

    return $record.GetType().InvokeMember("StringData", "GetProperty", $null, $record, 1)
}

if (-not (Test-Path -LiteralPath $MsiPath)) {
    throw "MSI not found: $MsiPath"
}

$msi = Resolve-Path -LiteralPath $MsiPath
$sha256 = (Get-FileHash -LiteralPath $msi -Algorithm SHA256).Hash
$productCode = Get-MsiProperty -Path $msi -Property "ProductCode"
$installerUrl = "https://github.com/$Repo/releases/download/v$Version/portzero-$Version-x86_64.msi"

$manifestDir = Join-Path $OutputRoot $Version
New-Item -ItemType Directory -Force -Path $manifestDir | Out-Null

$versionYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.1.9.0.schema.json

PackageIdentifier: PortZeroNetwork.PortZero
PackageVersion: $Version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.9.0
"@

$localeYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.1.9.0.schema.json

PackageIdentifier: PortZeroNetwork.PortZero
PackageVersion: $Version
PackageLocale: en-US
Publisher: Port Zero Network
PublisherUrl: https://portzero.cloud/
PublisherSupportUrl: https://github.com/$Repo/issues
PackageName: PortZero
PackageUrl: https://portzero.cloud/
License: GPL-3.0-or-later
LicenseUrl: https://github.com/$Repo/blob/staging/LICENSE
Copyright: Copyright (c) Loum Technologies, LLC
ShortDescription: Eliminate port conflicts in local dev environments
Description: |
  Port Zero eliminates port conflicts in your dev environment by letting the OS pick
  random available ports, then forwarding those ports to virtual domains on a virtual NIC.
  Run multiple branches simultaneously without clashing.

  After installing, open http://portzero.local in your browser.
Moniker: portzero
Tags:
- development
- networking
- port-forwarding
- tunnel
Commands:
- portzero
- portzero-app
ManifestType: defaultLocale
ManifestVersion: 1.9.0
"@

$installerYaml = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.1.9.0.schema.json

PackageIdentifier: PortZeroNetwork.PortZero
PackageVersion: $Version
InstallerType: wix
Scope: machine
InstallModes:
- interactive
- silent
- silentWithProgress
UpgradeBehavior: install
Commands:
- portzero
- portzero-app
Installers:
- Architecture: x64
  InstallerUrl: $installerUrl
  InstallerSha256: $sha256
  ProductCode: '$productCode'
  AppsAndFeaturesEntries:
  - ProductCode: '$productCode'
    UpgradeCode: '$UpgradeCode'
  InstallationMetadata:
    DefaultInstallLocation: '%ProgramFiles%/Port Zero'
ManifestType: installer
ManifestVersion: 1.9.0
"@

function Write-Utf8NoBomFile {
    param(
        [string] $Path,
        [string] $Content
    )

    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

Write-Utf8NoBomFile -Path (Join-Path $manifestDir "PortZeroNetwork.PortZero.version.yaml") -Content $versionYaml
Write-Utf8NoBomFile -Path (Join-Path $manifestDir "PortZeroNetwork.PortZero.locale.en-US.yaml") -Content $localeYaml
Write-Utf8NoBomFile -Path (Join-Path $manifestDir "PortZeroNetwork.PortZero.installer.yaml") -Content $installerYaml

Write-Host "Wrote winget manifests (coming soon) to $manifestDir"
Write-Host "InstallerSha256: $sha256"
Write-Host "ProductCode: $productCode"
