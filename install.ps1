<#
.SYNOPSIS
    Installs the ee (Eidetic Engine) CLI on Windows.

.DESCRIPTION
    Downloads and installs the latest release of ee to %LOCALAPPDATA%\ee\bin.
    Verifies SHA256 checksum and optionally Sigstore signature if cosign is available.
    Updates the user PATH environment variable.

.PARAMETER Version
    Specific version to install (e.g., "0.1.0"). Defaults to latest release.

.PARAMETER InstallDir
    Installation directory. Defaults to $env:LOCALAPPDATA\ee\bin.

.EXAMPLE
    & ([scriptblock]::Create((iwr -useb https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.ps1).Content)) -Version "0.1.0"

.EXAMPLE
    .\install.ps1 -Version 0.1.0

.NOTES
    Requires PowerShell 5.1 or later.
    Repository: https://github.com/Dicklesworthstone/eidetic_engine_cli
#>

[CmdletBinding()]
param(
    [Parameter()]
    [string]$Version,

    [Parameter()]
    [string]$InstallDir
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepoOwner = "Dicklesworthstone"
$RepoName = "eidetic_engine_cli"
$BinaryName = "ee.exe"

function Write-Status {
    param([string]$Message)
    Write-Host "==> " -ForegroundColor Cyan -NoNewline
    Write-Host $Message
}

function Write-Error-Exit {
    param([string]$Message)
    Write-Host "ERROR: " -ForegroundColor Red -NoNewline
    Write-Host $Message
    exit 1
}

function Get-Architecture {
    $arch = [System.Environment]::GetEnvironmentVariable("PROCESSOR_ARCHITECTURE")
    switch ($arch) {
        "AMD64" { return "x86_64" }
        "x86"   { Write-Error-Exit "Unsupported architecture: 32-bit Windows is not in the release asset matrix." }
        "ARM64" { Write-Error-Exit "Unsupported architecture: Windows ARM64 is not in the release asset matrix." }
        default { Write-Error-Exit "Unsupported architecture: $arch" }
    }
}

function Get-LatestVersion {
    Write-Status "Fetching latest release version..."
    $apiUrl = "https://api.github.com/repos/$RepoOwner/$RepoName/releases/latest"
    try {
        $response = Invoke-RestMethod -Uri $apiUrl -Headers @{ "User-Agent" = "ee-installer" }
        $tag = $response.tag_name
        if ($tag -match "^v?(.+)$") {
            return $Matches[1]
        }
        return $tag
    }
    catch {
        Write-Error-Exit "Failed to fetch latest release: $_"
    }
}

function Get-ReleaseAssetUrl {
    param(
        [string]$Version,
        [string]$AssetName
    )
    if ($Version.StartsWith("v")) {
        $tag = $Version
    }
    else {
        $tag = "v$Version"
    }
    return "https://github.com/$RepoOwner/$RepoName/releases/download/$tag/$AssetName"
}

function Download-File {
    param(
        [string]$Url,
        [string]$OutFile
    )
    Write-Status "Downloading $Url..."
    try {
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $Url -OutFile $OutFile -UseBasicParsing
        $ProgressPreference = 'Continue'
    }
    catch {
        Write-Error-Exit "Failed to download $Url`: $_"
    }
}

function Verify-SHA256 {
    param(
        [string]$FilePath,
        [string]$ExpectedHash
    )
    Write-Status "Verifying SHA256 checksum..."
    $actualHash = (Get-FileHash -Path $FilePath -Algorithm SHA256).Hash.ToLower()
    $expectedLower = $ExpectedHash.ToLower()
    if ($actualHash -ne $expectedLower) {
        Write-Error-Exit "SHA256 mismatch! Expected: $expectedLower, Got: $actualHash"
    }
    Write-Host "  Checksum verified." -ForegroundColor Green
}

function Verify-Sigstore {
    param(
        [string]$TarballPath,
        [string]$BundlePath
    )
    $cosign = Get-Command cosign -ErrorAction SilentlyContinue
    if (-not $cosign) {
        Write-Host "  Note: cosign not found, skipping Sigstore verification." -ForegroundColor Yellow
        return
    }
    Write-Status "Verifying Sigstore signature..."
    $certIdentityRegexp = "^https://github\.com/Dicklesworthstone/eidetic_engine_cli/\.github/workflows/release\.yml@refs/(tags/v[0-9].*|heads/main)$"
    $certOidcIssuer = "https://token.actions.githubusercontent.com"
    try {
        $result = & cosign verify-blob --bundle $BundlePath --certificate-identity-regexp $certIdentityRegexp --certificate-oidc-issuer $certOidcIssuer $TarballPath 2>&1
        if ($LASTEXITCODE -eq 0) {
            Write-Host "  Sigstore signature verified." -ForegroundColor Green
        }
        else {
            Write-Error-Exit "Sigstore verification failed: $result"
        }
    }
    catch {
        Write-Error-Exit "Sigstore verification error: $_"
    }
}

function Extract-Tarball {
    param(
        [string]$TarballPath,
        [string]$DestDir
    )
    Write-Status "Extracting to $DestDir..."

    if (-not (Test-Path $DestDir)) {
        New-Item -ItemType Directory -Path $DestDir -Force | Out-Null
    }

    $tarPath = $TarballPath -replace '\.xz$', ''

    if (Get-Command xz -ErrorAction SilentlyContinue) {
        & xz -d -k $TarballPath 2>$null
        & tar -xf $tarPath -C $DestDir 2>$null
        Remove-Item $tarPath -ErrorAction SilentlyContinue
    }
    elseif (Get-Command 7z -ErrorAction SilentlyContinue) {
        & 7z x $TarballPath -so 2>$null | & 7z x -si -ttar -o"$DestDir" 2>$null
    }
    else {
        try {
            Add-Type -AssemblyName System.IO.Compression.FileSystem
            $xzStream = [System.IO.File]::OpenRead($TarballPath)
            $tarStream = New-Object System.IO.MemoryStream

            $buffer = New-Object byte[] 65536
            $xzStream.Read($buffer, 0, 6) | Out-Null
            $xzStream.Position = 0

            Write-Error-Exit "xz decompression requires 'xz' or '7z' command. Please install one of them."
        }
        catch {
            Write-Error-Exit "Failed to extract tarball. Please install 'xz' or '7-Zip': $_"
        }
    }

    if (-not (Test-Path (Join-Path $DestDir $BinaryName))) {
        $subdirs = Get-ChildItem -Path $DestDir -Directory
        foreach ($subdir in $subdirs) {
            $binaryInSubdir = Join-Path $subdir.FullName $BinaryName
            if (Test-Path $binaryInSubdir) {
                Move-Item $binaryInSubdir $DestDir -Force
                Remove-Item $subdir.FullName -Recurse -Force -ErrorAction SilentlyContinue
                break
            }
        }
    }
}

function Update-UserPath {
    param([string]$Dir)
    $currentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($currentPath -split ";" -contains $Dir) {
        Write-Host "  $Dir already in PATH." -ForegroundColor Green
        return
    }
    Write-Status "Adding $Dir to user PATH..."
    $newPath = "$currentPath;$Dir"
    [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
    $env:PATH = "$env:PATH;$Dir"
    Write-Host "  PATH updated. Restart your terminal for changes to take effect." -ForegroundColor Green
}

function Verify-Installation {
    param([string]$BinaryPath)
    Write-Status "Verifying installation..."

    if (-not (Test-Path $BinaryPath)) {
        Write-Error-Exit "Binary not found at $BinaryPath"
    }

    try {
        $version = & $BinaryPath --version 2>&1
        Write-Host "  $version" -ForegroundColor Green
    }
    catch {
        Write-Error-Exit "Failed to run ee --version: $_"
    }

    try {
        $doctor = & $BinaryPath doctor --json 2>&1
        $doctorJson = $doctor | ConvertFrom-Json -ErrorAction SilentlyContinue
        if ($doctorJson -and $doctorJson.ok) {
            Write-Host "  ee doctor: OK" -ForegroundColor Green
        }
        else {
            Write-Host "  ee doctor completed (check output for details)" -ForegroundColor Yellow
        }
    }
    catch {
        Write-Host "  Note: ee doctor returned non-JSON output (this is OK for first run)" -ForegroundColor Yellow
    }
}

function Main {
    Write-Host ""
    Write-Host "ee (Eidetic Engine) Installer for Windows" -ForegroundColor Cyan
    Write-Host "=========================================" -ForegroundColor Cyan
    Write-Host ""

    $arch = Get-Architecture
    $target = "${arch}-pc-windows-msvc"
    Write-Status "Detected architecture: $arch (target: $target)"

    if (-not $Version) {
        $Version = Get-LatestVersion
    }
    Write-Status "Installing version: $Version"

    if (-not $InstallDir) {
        $InstallDir = Join-Path $env:LOCALAPPDATA "ee\bin"
    }
    Write-Status "Install directory: $InstallDir"

    $tarballName = "ee-$target.tar.xz"
    $sha256Name = "$tarballName.sha256"
    $sigstoreName = "$tarballName.sigstore.json"

    $tempDir = Join-Path $env:TEMP "ee-install-$(Get-Random)"
    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

    try {
        $tarballPath = Join-Path $tempDir $tarballName
        $sha256Path = Join-Path $tempDir $sha256Name
        $sigstorePath = Join-Path $tempDir $sigstoreName

        Download-File (Get-ReleaseAssetUrl $Version $tarballName) $tarballPath
        Download-File (Get-ReleaseAssetUrl $Version $sha256Name) $sha256Path

        $cosign = Get-Command cosign -ErrorAction SilentlyContinue
        if ($cosign) {
            Download-File (Get-ReleaseAssetUrl $Version $sigstoreName) $sigstorePath
            $hasSigstore = $true
        } else {
            $hasSigstore = $false
            Write-Host "  Note: cosign not found, skipping Sigstore verification." -ForegroundColor Yellow
        }

        $expectedHash = (Get-Content $sha256Path -Raw).Trim().Split()[0]
        Verify-SHA256 $tarballPath $expectedHash

        if ($hasSigstore) {
            Verify-Sigstore $tarballPath $sigstorePath
        }

        Extract-Tarball $tarballPath $InstallDir

        Update-UserPath $InstallDir

        $binaryPath = Join-Path $InstallDir $BinaryName
        Verify-Installation $binaryPath

        Write-Host ""
        Write-Host "Installation complete!" -ForegroundColor Green
        Write-Host "Run 'ee --help' to get started." -ForegroundColor Cyan
        Write-Host ""
    }
    finally {
        Remove-Item $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Main
