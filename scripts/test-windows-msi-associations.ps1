# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
# Real MSI publication/maintenance regression. Runs only on a disposable hosted
# runner, or an explicitly authorized Windows machine. Never modifies UserChoice.
param(
    [Parameter(Mandatory = $true)][string]$MsiPath,
    [string]$EvidencePath = 'msi-association-evidence',
    [switch]$AllowMachineChanges
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($env:OS -ne 'Windows_NT') { throw 'This regression requires native Windows.' }
if (-not $AllowMachineChanges -and -not ($env:GITHUB_ACTIONS -eq 'true' -and $env:RUNNER_ENVIRONMENT -eq 'github-hosted')) {
    throw 'Use a disposable GitHub-hosted runner or explicitly authorize machine changes with -AllowMachineChanges.'
}
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run elevated to test the machine-scope MSI.' }
foreach ($hive in @('HKCU:', 'HKLM:')) {
    foreach ($sub in @('Software\Microsoft\Windows\CurrentVersion\Uninstall', 'Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall')) {
        $existing = Get-ItemProperty "$hive\$sub\*" -ErrorAction SilentlyContinue | Where-Object {
            $_.PSObject.Properties['DisplayName'] -and $_.DisplayName -like '*AeroFTP*'
        }
        if ($existing) { throw 'An AeroFTP installation exists. Refusing to replace it for this regression.' }
    }
}
$MsiPath = (Resolve-Path -LiteralPath $MsiPath).Path
$evidence = (New-Item -ItemType Directory -Force -Path $EvidencePath).FullName
$installDir = Join-Path ([IO.Path]::GetTempPath()) ('AeroFTP MSI regression ' + [guid]::NewGuid().ToString('N'))
$installer = New-Object -ComObject WindowsInstaller.Installer
$db = $installer.OpenDatabase($MsiPath, 0)
$view = $db.OpenView("SELECT ``Value`` FROM ``Property`` WHERE ``Property`` = 'ProductCode'")
$view.Execute()
$product = $view.Fetch().StringData(1)
$view.Close()
$catalog = Get-Content -Raw (Join-Path $PSScriptRoot '..\src-tauri\installer\association-registry.json') | ConvertFrom-Json
$ownedFeature = 'AeroFTPOwnedAssociations'
$archiveFeature = 'AeroFTPArchiveAssociations'
# Same-hive foreign fixture, never a default or a protected UserChoice.
$foreignPath = 'Software\Classes\.zip\OpenWithProgids'
$foreignName = 'AeroFTP.Test.Foreign.' + [guid]::NewGuid().ToString('N')
$fixture = [Microsoft.Win32.Registry]::LocalMachine.CreateSubKey($foreignPath)
$fixture.SetValue($foreignName, 'keep', [Microsoft.Win32.RegistryValueKind]::String)
$fixture.Dispose()

function Invoke-Msi([string]$Label, [string]$Arguments) {
    $log = Join-Path $evidence ($Label + '.log')
    $start = [Diagnostics.ProcessStartInfo]::new('msiexec.exe')
    $start.UseShellExecute = $false
    $start.Arguments = $Arguments + ' /qn REBOOT=ReallySuppress /l*v "' + $log + '"'
    $process = [Diagnostics.Process]::Start($start)
    if (-not $process.WaitForExit(300000)) { throw "MSI timed out: $Label (see $log)" }
    if ($process.ExitCode -notin @(0, 3010)) { throw "MSI failed: $Label, exit $($process.ExitCode) (see $log)" }
}
function Assert-State([string]$Label, [bool]$Owned, [bool]$Archives) {
    $ownedState = $installer.FeatureState($product, $ownedFeature)
    $archiveState = $installer.FeatureState($product, $archiveFeature)
    if ($ownedState -ne $(if ($Owned) { 3 } else { 2 }) -or $archiveState -ne $(if ($Archives) { 3 } else { 2 })) {
        throw "$Label has unexpected feature states: owned=$ownedState archives=$archiveState"
    }
    if ($installer.FeatureState($product, 'MainProgram') -ne 3) { throw "$Label lost MainProgram" }
    foreach ($binary in @('aeroftp.exe', 'aeroftp-cli.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $installDir $binary))) { throw "$Label lost $binary" }
    }
    $arp = Get-ItemProperty -LiteralPath "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$product"
    if ($arp.InstallLocation.TrimEnd('\') -ne $installDir) { throw "$Label changed INSTALLDIR to $($arp.InstallLocation)" }
    foreach ($value in $catalog.values) {
        $enabled = switch ($value.group) { 'owned' { $Owned } 'archives' { $Archives } 'common' { $Owned -or $Archives } }
        $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($value.key)
        try {
            $actual = if ($key) { $key.GetValue($value.name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) } else { $null }
            if ($enabled) {
                $expected = $value.value.Replace('@EXE@', (Join-Path $installDir 'aeroftp.exe')).Replace('@DIR@', $installDir)
                if ($null -eq $actual -or $actual -ne $expected) { throw "$Label missing/wrong catalog value: $($value.key) [$($value.name)]" }
            } elseif ($null -ne $actual) { throw "$Label retained disabled catalog value: $($value.key) [$($value.name)]" }
        } finally { if ($key) { $key.Dispose() } }
    }
    Assert-Foreign
    [pscustomobject]@{ Label = $Label; Product = $product; InstallDir = $installDir; Owned = $ownedState; Archives = $archiveState } |
        ConvertTo-Json | Set-Content -LiteralPath (Join-Path $evidence ($Label + '.json'))
    Write-Output "PASS $Label (owned=$ownedState archives=$archiveState, full catalog, mandatory binaries, space path)"
}
function Assert-Foreign {
    $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($foreignPath)
    try { if (-not $key -or $key.GetValue($foreignName) -ne 'keep') { throw 'MSI removed a foreign shared value.' } }
    finally { if ($key) { $key.Dispose() } }
}
try {
    Invoke-Msi 'fresh-noassoc' ('/i "' + $MsiPath + '" AEROFTP_NOASSOC=1 INSTALLDIR="' + $installDir + '"')
    Assert-State 'fresh-noassoc' $false $false
    foreach ($choice in @(@($true, $false), @($true, $true), @($false, $true), @($false, $false), @($true, $false), @($false, $false))) {
        $add = @(); $remove = @()
        if ($choice[0]) { $add += $ownedFeature } else { $remove += $ownedFeature }
        if ($choice[1]) { $add += $archiveFeature } else { $remove += $archiveFeature }
        $arguments = '/i ' + $product + ' INSTALLDIR="' + $installDir + '"'
        if ($add.Count) { $arguments += ' ADDLOCAL=' + ($add -join ',') }
        if ($remove.Count) { $arguments += ' REMOVE=' + ($remove -join ',') }
        $label = 'maintenance-' + [guid]::NewGuid().ToString('N')
        Invoke-Msi $label $arguments
        Assert-State $label $choice[0] $choice[1]
    }
    Invoke-Msi 'same-version-off' ('/i "' + $MsiPath + '" INSTALLDIR="' + $installDir + '"')
    Assert-State 'same-version-off' $false $false
} finally {
    # Always uninstall the exact product created here before removing the fixture.
    try {
        if ($installer.ProductState($product) -eq 5) {
            Invoke-Msi 'uninstall' ('/x ' + $product)
            Assert-Foreign
            if (Test-Path -LiteralPath (Join-Path $installDir 'aeroftp.exe')) { throw 'MSI uninstall retained the application.' }
        }
    } finally {
        $key = [Microsoft.Win32.Registry]::LocalMachine.OpenSubKey($foreignPath, $true)
        if ($key) { $key.DeleteValue($foreignName, $false); $key.Dispose() }
    }
}
Write-Output 'PASS native MSI NOASSOC, independent maintenance, common component lifetime and uninstall'
