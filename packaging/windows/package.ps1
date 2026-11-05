<#
.SYNOPSIS
  Builds the portable Windows package: livedeck.exe, libmpv, and a bundled Streamlink in one folder
  and one zip. Users unpack it and run livedeck.exe, there is nothing to install.

.PARAMETER Exe
  The release build of livedeck.exe (built with --features mpv).
.PARAMETER MpvDll
  libmpv-2.dll, which has to sit next to the executable.
.PARAMETER StreamlinkDir
  The unpacked portable Streamlink build (the folder that has bin\streamlink.exe in it). It is
  copied to streamlink\, which is where the app looks for it.
.PARAMETER Version
  Used in the folder and zip name, e.g. v0.1.0.
.PARAMETER OutDir
  Where the folder and the zip are written.
#>
param(
    [Parameter(Mandatory)][string]$Exe,
    [Parameter(Mandatory)][string]$MpvDll,
    [Parameter(Mandatory)][string]$StreamlinkDir,
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$OutDir
)
$ErrorActionPreference = 'Stop'

$repo = Resolve-Path (Join-Path $PSScriptRoot '..\..')
if (-not (Test-Path (Join-Path $StreamlinkDir 'bin\streamlink.exe'))) {
    throw "$StreamlinkDir\bin\streamlink.exe not found"
}

$name = "LiveDeck-$Version-windows-x64"
$pkg = Join-Path $OutDir $name
if (Test-Path $pkg) { Remove-Item $pkg -Recurse -Force }
New-Item -ItemType Directory -Force $pkg | Out-Null

Copy-Item $Exe (Join-Path $pkg 'livedeck.exe')
Copy-Item $MpvDll (Join-Path $pkg 'libmpv-2.dll')
Copy-Item $StreamlinkDir (Join-Path $pkg 'streamlink') -Recurse
Copy-Item (Join-Path $repo 'LICENSE') $pkg
Copy-Item (Join-Path $repo 'packaging\THIRD_PARTY.md') $pkg
Copy-Item (Join-Path $repo 'assets\fonts\OFL.txt') (Join-Path $pkg 'OFL-AtkinsonHyperlegible.txt')

@"
LiveDeck $Version (portable)

Run livedeck.exe. Nothing needs to be installed: libmpv (video) and Streamlink (stream
lookup) are in this folder. Settings are kept in %APPDATA%\livedeck, so deleting this folder
removes the program and leaves your settings.

Windows SmartScreen may warn about an unknown publisher, because the program is not code-signed.
Choose "More info", then "Run anyway".

LiveDeck is an unofficial Twitch client. It is not affiliated with Twitch or Valve. Streams are
found with Streamlink through unofficial endpoints, which can break and may conflict with Twitch's
terms of service. See the project README. Licences of the bundled software: THIRD_PARTY.md.
"@ | Set-Content (Join-Path $pkg 'README.txt') -Encoding utf8

$zip = Join-Path $OutDir "$name.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $pkg -DestinationPath $zip -CompressionLevel Optimal
Write-Host "Wrote $zip ($([math]::Round((Get-Item $zip).Length / 1MB)) MB)"
