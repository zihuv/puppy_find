param(
    [Parameter(Mandatory = $true)]
    [string]$BinaryPath,

    [Parameter(Mandatory = $true)]
    [ValidateSet("windows", "linux", "macos")]
    [string]$Platform,

    [Parameter(Mandatory = $true)]
    [string]$PackageId,

    [Parameter(Mandatory = $true)]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [ValidateSet("nomodel", "model")]
    [string]$Flavor,

    [Parameter(Mandatory = $true)]
    [string]$OutputDir,

    [string]$ModelSourceDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [string]$Content
    )

    $parent = Split-Path -Parent $Path
    if ($parent) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }

    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function New-ZipArchive {
    param(
        [Parameter(Mandatory = $true)]
        [string]$SourceDir,

        [Parameter(Mandatory = $true)]
        [string]$ArchivePath
    )

    if (Test-Path $ArchivePath) {
        Remove-Item -LiteralPath $ArchivePath -Force
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $SourceDir,
        $ArchivePath,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false
    )
}

function New-TarGzArchive {
    param(
        [Parameter(Mandatory = $true)]
        [string]$BaseDir,

        [Parameter(Mandatory = $true)]
        [string]$FolderName,

        [Parameter(Mandatory = $true)]
        [string]$ArchivePath
    )

    if (Test-Path $ArchivePath) {
        Remove-Item -LiteralPath $ArchivePath -Force
    }

    & tar -czf $ArchivePath -C $BaseDir $FolderName
    if ($LASTEXITCODE -ne 0) {
        throw "failed to create tar.gz archive: $ArchivePath"
    }
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$binary = (Resolve-Path $BinaryPath).Path

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
$outputRoot = (Resolve-Path $OutputDir).Path

$stagingRoot = Join-Path $repoRoot ".dist"
New-Item -ItemType Directory -Force -Path $stagingRoot | Out-Null

$bundleName = "puppy_find-$Version-$PackageId-$Flavor"
$bundleRoot = Join-Path $stagingRoot $bundleName

if (Test-Path $bundleRoot) {
    Remove-Item -LiteralPath $bundleRoot -Recurse -Force
}

New-Item -ItemType Directory -Force -Path $bundleRoot | Out-Null

$binaryName = Split-Path -Leaf $binary
Copy-Item -LiteralPath $binary -Destination (Join-Path $bundleRoot $binaryName)

$materialsDir = Join-Path $bundleRoot "materials"
$modelDir = Join-Path $bundleRoot "model"
New-Item -ItemType Directory -Force -Path $materialsDir, $modelDir | Out-Null

Write-Utf8NoBom -Path (Join-Path $materialsDir "PUT_IMAGES_HERE.txt") -Content @"
Put the images you want to index into this folder.
The portable package writes its database next to the executable.
"@

if ($Flavor -eq "model") {
    if (-not $ModelSourceDir) {
        throw "ModelSourceDir is required when Flavor=model"
    }

    $resolvedModelSourceDir = (Resolve-Path $ModelSourceDir).Path
    Get-ChildItem -LiteralPath $resolvedModelSourceDir -Force |
        Where-Object { $_.Name -ne ".cache" } |
        ForEach-Object {
            Copy-Item -LiteralPath $_.FullName -Destination $modelDir -Recurse -Force
        }

    Write-Utf8NoBom -Path (Join-Path $modelDir "MODEL_INFO.txt") -Content @"
This package already includes the Hugging Face model bundle:
zihuv/chinese-clip-vit-base-patch16-onnx
"@
}
else {
    Write-Utf8NoBom -Path (Join-Path $modelDir "PUT_MODEL_HERE.txt") -Content @"
Download the Hugging Face repository below into this folder without adding an extra nested directory:
https://huggingface.co/zihuv/chinese-clip-vit-base-patch16-onnx

Expected result:
  ./model/model_config.json
  ./model/text.onnx
  ./model/visual.onnx
  ./model/vocab.txt
"@
}

Write-Utf8NoBom -Path (Join-Path $bundleRoot ".env") -Content @"
# PuppyFind portable configuration
DB_PATH="./puppy_find.sqlite3"
MODEL_PATH="./model"
OMNI_INTRA_THREADS=4
OMNI_FGCLIP_MAX_PATCHES=256
HOST="127.0.0.1"
PORT=3000
ASSET_DIR="./materials"
"@

$binaryReference = if ($Platform -eq "windows") { ".\${binaryName}" } else { "./${binaryName}" }

switch ($Platform) {
    "windows" {
        Write-Utf8NoBom -Path (Join-Path $bundleRoot "start-puppy-find.bat") -Content @"
@echo off
cd /d "%~dp0"
$binaryReference
"@
    }
    "linux" {
        $launcherPath = Join-Path $bundleRoot "start-puppy-find.sh"
        Write-Utf8NoBom -Path $launcherPath -Content @"
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
$binaryReference
"@

        & chmod +x $launcherPath (Join-Path $bundleRoot $binaryName)
    }
    "macos" {
        $shellLauncherPath = Join-Path $bundleRoot "start-puppy-find.sh"
        $commandLauncherPath = Join-Path $bundleRoot "start-puppy-find.command"

        $launcherContent = @"
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
$binaryReference
"@

        Write-Utf8NoBom -Path $shellLauncherPath -Content $launcherContent
        Write-Utf8NoBom -Path $commandLauncherPath -Content $launcherContent

        & chmod +x $shellLauncherPath $commandLauncherPath (Join-Path $bundleRoot $binaryName)
    }
}

$launchHint = switch ($Platform) {
    "windows" { "Double-click start-puppy-find.bat" }
    "linux" { "Run ./start-puppy-find.sh" }
    "macos" { "Double-click start-puppy-find.command or run ./start-puppy-find.sh" }
    default { "Run the bundled binary" }
}

$modelHint = if ($Flavor -eq "model") {
    "The model bundle is already included under ./model."
}
else {
    "Download zihuv/chinese-clip-vit-base-patch16-onnx into ./model before indexing."
}

Write-Utf8NoBom -Path (Join-Path $bundleRoot "README.txt") -Content @"
PuppyFind portable package

What is included:
- The Rust binary
- The web UI embedded inside the binary
- A portable .env pointing to ./materials and ./model

Quick start:
1. Put images into ./materials
2. $launchHint
3. The browser should open automatically at http://127.0.0.1:3000

$modelHint
"@

if ($Platform -eq "windows") {
    $archivePath = Join-Path $outputRoot "$bundleName.zip"
    New-ZipArchive -SourceDir $bundleRoot -ArchivePath $archivePath
}
else {
    $archivePath = Join-Path $outputRoot "$bundleName.tar.gz"
    New-TarGzArchive -BaseDir $stagingRoot -FolderName $bundleName -ArchivePath $archivePath
}

Write-Output $archivePath
