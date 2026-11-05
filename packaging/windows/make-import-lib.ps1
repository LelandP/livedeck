<#
.SYNOPSIS
  Creates the MSVC import library mpv.lib next to libmpv-2.dll, so Rust (MSVC) can link libmpv.

.DESCRIPTION
  The prebuilt libmpv development package (shinchiro/mpv-winbuild-cmake) ships only the DLL and a
  MinGW import library. This reads the DLL's exports with dumpbin and runs lib on a .def file.
  Run it from a shell where the MSVC tools are on PATH (a Developer PowerShell, or after
  ilammy/msvc-dev-cmd in CI).

.PARAMETER Dir
  The folder that contains libmpv-2.dll. mpv.lib is written there.
#>
param(
    [Parameter(Mandatory)][string]$Dir
)
$ErrorActionPreference = 'Stop'

$dll = Join-Path $Dir 'libmpv-2.dll'
if (-not (Test-Path $dll)) { throw "libmpv-2.dll not found in $Dir" }

$names = & dumpbin /exports $dll | ForEach-Object {
    if ($_ -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]{8}\s+(\S+)') { $Matches[1] }
}
if (-not $names) { throw 'dumpbin found no exports. Are the MSVC tools on PATH?' }

$def = Join-Path $Dir 'mpv.def'
Set-Content $def (@('LIBRARY libmpv-2', 'EXPORTS') + $names) -Encoding ascii
& lib /nologo "/def:$def" /machine:x64 "/out:$(Join-Path $Dir 'mpv.lib')"
if ($LASTEXITCODE -ne 0) { throw "lib.exe failed with exit code $LASTEXITCODE" }
Write-Host "Wrote $(Join-Path $Dir 'mpv.lib') ($($names.Count) exports)"
