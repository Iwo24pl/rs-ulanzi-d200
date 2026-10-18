<# PowerShell equivalent of pack.sh for Windows.
   Usage:
     powershell -ExecutionPolicy Bypass -File pack.ps1 debug
     powershell -ExecutionPolicy Bypass -File pack.ps1 release
#>
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("debug", "release")]
    [string]$Mode
)

$ErrorActionPreference = "Stop"

$BinaryName   = "rs-ulanzi-d200.exe"
$ManifestSrc  = "src/manifest.json"
$AssetsSrc    = "src/assets"
$ConfigYaml   = "config.yaml"
$PluginFolder = "com.iwo24pl.rs-ulanzi-d200.sdPlugin"

if ($Mode -eq "debug") {
    $BinaryPath = "target/debug/$BinaryName"
    $ZipName    = "rs-ulanzi-d200-debug.zip"
    cargo build
} else {
    $BinaryPath = "target/release/$BinaryName"
    $ZipName    = "com.iwo24pl.rs-ulanzi-d200-windows.zip"
    cargo build --release
}

if (-not (Test-Path -LiteralPath $BinaryPath)) {
    throw "Binary not found at $BinaryPath"
}
foreach ($p in @($ManifestSrc, $AssetsSrc, $ConfigYaml)) {
    if (-not (Test-Path -LiteralPath $p)) {
        throw "Required source not found: $p"
    }
}

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $TmpDir | Out-Null
try {
    $Dest = Join-Path $TmpDir $PluginFolder
    New-Item -ItemType Directory -Path $Dest | Out-Null

    Copy-Item -LiteralPath $BinaryPath -Destination (Join-Path $Dest $BinaryName)
    Copy-Item -LiteralPath $ManifestSrc -Destination (Join-Path $Dest "manifest.json")
    Copy-Item -LiteralPath $ConfigYaml  -Destination (Join-Path $Dest "config.yaml")
    Copy-Item -LiteralPath $AssetsSrc   -Destination (Join-Path $Dest "assets") -Recurse

    if (-not (Test-Path -LiteralPath $ZipName)) { Remove-Item -LiteralPath $ZipName -Force -ErrorAction SilentlyContinue }

Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$fs = [IO.File]::Open((Join-Path (Get-Location) $ZipName), [IO.FileMode]::CreateNew)
$zip = New-Object IO.Compression.ZipArchive($fs, [IO.Compression.ZipArchiveMode]::Create)
Get-ChildItem (Join-Path $TmpDir $PluginFolder) -Recurse -File | ForEach-Object {
    $rel = $_.FullName.Substring((Join-Path $TmpDir $PluginFolder).Length + 1).Replace('\', '/')
    $entry = $zip.CreateEntry("$PluginFolder/$rel", [IO.Compression.CompressionLevel]::Optimal)
    $es = $entry.Open()
    $bytes = [IO.File]::ReadAllBytes($_.FullName)
    $es.Write($bytes, 0, $bytes.Length)
    $es.Close()
}
$zip.Dispose()
$fs.Close()

Write-Output "Successfully created $ZipName"
Write-Output "Zip contains the top-level folder: $PluginFolder/"
}
finally {
    Remove-Item -LiteralPath $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
