<#
.SYNOPSIS
    Installs the ee (Eidetic Engine) CLI on Windows.

.DESCRIPTION
    Downloads and verifies a signed release of ee, then installs it to a
    user-writable directory (default %LOCALAPPDATA%\ee\bin). Verifies SHA256
    and, when cosign is available, the Sigstore bundle.

    Mirrors the structure of the POSIX install.sh: branded header, platform
    detection, preflight checks, atomic locking, download + checksum + signature,
    extraction, install, user-PATH update, self-test, and a summary block with
    detected agents and uninstall instructions.

.PARAMETER Version
    Specific version to install (e.g. "0.1.0"). Defaults to latest GitHub release.

.PARAMETER InstallDir
    Installation directory. Defaults to "$env:LOCALAPPDATA\ee\bin".

.PARAMETER ArtifactUrl
    Override the tarball download URL.

.PARAMETER Checksum
    Use this SHA256 instead of fetching <url>.sha256.

.PARAMETER Force
    Reinstall even when the same version is already present.

.PARAMETER Verify
    Run `ee --version` and `ee doctor --json` after install.

.PARAMETER Quiet
    Suppress non-error output.

.PARAMETER NoConfigure
    Skip agent integration instructions.

.PARAMETER NoVerify
    Skip SHA256 + Sigstore verification (NOT recommended).

.PARAMETER Offline
    Skip network preflight checks.

.PARAMETER FromSource
    Build from source via git + cargo instead of downloading.

.EXAMPLE
    & ([scriptblock]::Create((iwr -useb https://github.com/Dicklesworthstone/eidetic_engine_cli/releases/download/v0.1.0/install.ps1).Content)) -Version "0.1.0"

.EXAMPLE
    .\install.ps1 -Version 0.1.0 -Verify

.NOTES
    Requires PowerShell 5.1+. Tested on Windows 10/11 and PowerShell 7+.
    Repository: https://github.com/Dicklesworthstone/eidetic_engine_cli
#>

[CmdletBinding()]
param(
    [string]$Version,
    [string]$InstallDir,
    [string]$ArtifactUrl,
    [string]$Checksum,
    [string]$ChecksumUrl,
    [switch]$Force,
    [switch]$Verify,
    [switch]$Quiet,
    [switch]$NoConfigure,
    [switch]$NoVerify,
    [switch]$Offline,
    [switch]$FromSource
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

# Honor legacy environment variables for parity with install.sh.
if (-not $Version    -and $env:EE_VERSION)     { $Version    = $env:EE_VERSION }
if (-not $InstallDir -and $env:EE_INSTALL_DIR) { $InstallDir = $env:EE_INSTALL_DIR }
if (-not $NoVerify   -and $env:EE_SKIP_VERIFY -eq "1") { $NoVerify = [switch]::Present }

$Script:RepoOwner   = "Dicklesworthstone"
$Script:RepoName    = "eidetic_engine_cli"
$Script:BinaryName  = "ee.exe"
$Script:ProjectName = "ee (Eidetic Engine)"

$Script:CosignIdentityRegexp = "^https://github\.com/$Script:RepoOwner/$Script:RepoName/\.github/workflows/release\.yml@refs/(tags/v[0-9].*|heads/main)$"
$Script:CosignOidcIssuer     = "https://token.actions.githubusercontent.com"

# Lock file (atomic via mkdir-equivalent: New-Item -ItemType Directory).
$Script:LockDir = Join-Path $env:TEMP "ee-install.lock.d"

# ───────────────────────────────────────────────────────────────────────────
# Output helpers
# ───────────────────────────────────────────────────────────────────────────

function Test-IsAnsiCapable {
    if ($env:NO_COLOR) { return $false }
    if ($env:TERM -match "(?i)dumb") { return $false }
    if ($Host.UI.RawUI.ForegroundColor -is [ConsoleColor]) { return $true }
    return $false
}
$Script:Color = Test-IsAnsiCapable

function Write-Info {
    param([string]$Message)
    if ($Quiet) { return }
    if ($Script:Color) {
        Write-Host "→ " -ForegroundColor Cyan -NoNewline
        Write-Host $Message
    } else {
        Write-Host "-> $Message"
    }
}

function Write-Ok {
    param([string]$Message)
    if ($Quiet) { return }
    if ($Script:Color) {
        Write-Host "✓ " -ForegroundColor Green -NoNewline
        Write-Host $Message
    } else {
        Write-Host "[ok] $Message"
    }
}

function Write-Warning2 {
    param([string]$Message)
    if ($Quiet) { return }
    if ($Script:Color) {
        Write-Host "⚠ " -ForegroundColor Yellow -NoNewline
        Write-Host $Message
    } else {
        Write-Warning $Message
    }
}

function Write-ErrorExit {
    param([string]$Message)
    if ($Script:Color) {
        Write-Host "✗ " -ForegroundColor Red -NoNewline
        Write-Host $Message
    } else {
        Write-Host "[error] $Message"
    }
    exit 1
}

function Write-Header {
    if ($Quiet) { return }
    Write-Host ""
    if ($Script:Color) {
        Write-Host "ee installer" -ForegroundColor Green
        Write-Host "Durable, local-first, explainable memory for coding agents" -ForegroundColor DarkGray
    } else {
        Write-Host "ee installer"
        Write-Host "Durable, local-first, explainable memory for coding agents"
    }
    Write-Host ""
}

# ───────────────────────────────────────────────────────────────────────────
# Proxy
# ───────────────────────────────────────────────────────────────────────────

function Get-ProxyUri {
    if ($env:HTTPS_PROXY) { return $env:HTTPS_PROXY }
    if ($env:HTTP_PROXY)  { return $env:HTTP_PROXY }
    return $null
}

function Invoke-DownloadFile {
    param(
        [Parameter(Mandatory=$true)][string]$Url,
        [Parameter(Mandatory=$true)][string]$OutFile,
        [int]$TimeoutSec = 60
    )
    $params = @{
        Uri             = $Url
        OutFile         = $OutFile
        UseBasicParsing = $true
        TimeoutSec      = $TimeoutSec
        UserAgent       = "ee-installer/1.0"
    }
    $proxy = Get-ProxyUri
    if ($proxy) { $params.Proxy = $proxy }

    $oldProgress = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        Invoke-WebRequest @params | Out-Null
    } finally {
        $ProgressPreference = $oldProgress
    }
}

function Invoke-GetString {
    param([Parameter(Mandatory=$true)][string]$Url, [int]$TimeoutSec = 30)
    $params = @{
        Uri             = $Url
        UseBasicParsing = $true
        TimeoutSec      = $TimeoutSec
        UserAgent       = "ee-installer/1.0"
    }
    $proxy = Get-ProxyUri
    if ($proxy) { $params.Proxy = $proxy }

    $oldProgress = $ProgressPreference
    $ProgressPreference = "SilentlyContinue"
    try {
        return (Invoke-WebRequest @params).Content
    } finally {
        $ProgressPreference = $oldProgress
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Platform detection
# ───────────────────────────────────────────────────────────────────────────

function Get-PlatformTarget {
    $arch = $env:PROCESSOR_ARCHITECTURE
    if (-not $arch) { $arch = "AMD64" }
    switch ($arch) {
        "AMD64" { return "x86_64-pc-windows-msvc" }
        "x86"   { Write-ErrorExit "Unsupported architecture: 32-bit Windows is not in the release asset matrix." }
        "ARM64" { Write-ErrorExit "Unsupported architecture: Windows ARM64 is not in the release asset matrix yet." }
        default { Write-ErrorExit "Unsupported architecture: $arch" }
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Agent detection (informational only)
# ───────────────────────────────────────────────────────────────────────────

function Get-DetectedAgents {
    $agents = New-Object System.Collections.Generic.List[string]
    if ((Test-Path "$env:USERPROFILE\.claude") -or (Get-Command claude -ErrorAction SilentlyContinue)) {
        $agents.Add("Claude Code") | Out-Null
    }
    if ((Test-Path "$env:USERPROFILE\.codex") -or (Get-Command codex -ErrorAction SilentlyContinue)) {
        $agents.Add("Codex CLI") | Out-Null
    }
    if ((Test-Path "$env:USERPROFILE\.gemini") -or (Test-Path "$env:USERPROFILE\.gemini-cli") -or (Get-Command gemini -ErrorAction SilentlyContinue)) {
        $agents.Add("Gemini CLI") | Out-Null
    }
    if (Get-Command aider -ErrorAction SilentlyContinue) {
        $agents.Add("Aider") | Out-Null
    }
    if ((Get-Command copilot -ErrorAction SilentlyContinue) -or (Test-Path "$env:USERPROFILE\.copilot")) {
        $agents.Add("GitHub Copilot CLI") | Out-Null
    }
    if (Test-Path "$env:USERPROFILE\.continue") {
        $agents.Add("Continue") | Out-Null
    }
    if ((Test-Path "$env:USERPROFILE\.cursor") -or (Get-Command cursor -ErrorAction SilentlyContinue)) {
        $agents.Add("Cursor IDE") | Out-Null
    }
    return $agents
}

function Show-DetectedAgents {
    param([System.Collections.Generic.List[string]]$Agents)
    if ($Quiet) { return }
    if ($Agents.Count -eq 0) {
        Write-Info "No AI coding agents detected"
        return
    }
    Write-Host ""
    if ($Script:Color) {
        Write-Host "Detected AI Coding Agents:" -ForegroundColor White
    } else {
        Write-Host "Detected AI Coding Agents:"
    }
    foreach ($a in $Agents) {
        if ($Script:Color) {
            Write-Host "  ✓ " -ForegroundColor Green -NoNewline
            Write-Host $a
        } else {
            Write-Host "  [v] $a"
        }
    }
    Write-Host ""
}

# ───────────────────────────────────────────────────────────────────────────
# Version resolution
# ───────────────────────────────────────────────────────────────────────────

function Get-LatestVersion {
    Write-Info "Resolving latest version..."
    $apiUrl = "https://api.github.com/repos/$Script:RepoOwner/$Script:RepoName/releases/latest"
    try {
        $params = @{
            Uri             = $apiUrl
            UseBasicParsing = $true
            UserAgent       = "ee-installer/1.0"
            TimeoutSec      = 30
        }
        $proxy = Get-ProxyUri
        if ($proxy) { $params.Proxy = $proxy }
        $response = Invoke-RestMethod @params
        return $response.tag_name
    }
    catch {
        Write-ErrorExit "Failed to resolve latest release: $_`nUse -Version vX.Y.Z to install a specific tag."
    }
}

function ConvertTo-TagName {
    param([string]$Version)
    if ($Version.StartsWith("v")) { return $Version }
    return "v$Version"
}

# ───────────────────────────────────────────────────────────────────────────
# Preflight
# ───────────────────────────────────────────────────────────────────────────

function Test-DiskSpace {
    param([string]$Path, [int]$MinMb = 20)
    $parent = if (Test-Path $Path) { $Path } else { Split-Path -Parent $Path }
    if (-not $parent -or -not (Test-Path $parent)) { return }
    try {
        $drive = (Get-Item $parent).PSDrive
        if ($drive) {
            $freeMb = [math]::Floor($drive.Free / 1MB)
            if ($freeMb -lt $MinMb) {
                Write-ErrorExit "Insufficient disk space on $($drive.Name): need >= ${MinMb}MB, have ${freeMb}MB"
            }
        }
    } catch {
        Write-Warning2 "Could not determine disk space for ${parent}: $_"
    }
}

function Test-WritePermission {
    param([string]$Dir)
    if (-not (Test-Path $Dir)) {
        try {
            New-Item -ItemType Directory -Path $Dir -Force -ErrorAction Stop | Out-Null
        } catch {
            Write-ErrorExit "Cannot create $Dir`: $_"
        }
    }
    try {
        $probe = Join-Path $Dir ".ee-install-probe"
        Set-Content -Path $probe -Value "ok" -ErrorAction Stop
        Remove-Item $probe -ErrorAction SilentlyContinue
    } catch {
        Write-ErrorExit "No write permission to ${Dir}: $_"
    }
}

function Test-Network {
    param([string]$Url)
    if ($Offline) { Write-Info "Offline mode; skipping network preflight"; return }
    if (-not $Url) { return }
    try {
        $params = @{
            Uri             = $Url
            Method          = "Head"
            UseBasicParsing = $true
            TimeoutSec      = 5
            UserAgent       = "ee-installer/1.0"
        }
        $proxy = Get-ProxyUri
        if ($proxy) { $params.Proxy = $proxy }
        Invoke-WebRequest @params -ErrorAction Stop | Out-Null
    } catch {
        Write-Warning2 "Network preflight failed for ${Url}: $($_.Exception.Message). Continuing; download may fail."
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Lock
# ───────────────────────────────────────────────────────────────────────────

# Track whether THIS process owns the lock. Lock-Release must not delete
# a lock another installer holds, e.g., when Lock-Acquire fails because
# someone else got there first.
$Script:LockOwned = $false

function Lock-Acquire {
    # Atomic acquisition. PowerShell's `New-Item -ItemType Directory` without
    # -Force on an existing dir is rejected by the FileSystem provider, so
    # exactly one of two racing installers wins. The prior "Test-Path then
    # New-Item -Force" pattern had an explicit TOCTOU where both racers
    # could pass the existence test and then both `-Force`-succeed. (Note:
    # this is still not as tight as bash's kernel-level mkdir(2), but it is
    # the strongest portable PowerShell primitive without dropping to P/Invoke.)
    $acquired = $false
    try {
        New-Item -ItemType Directory -Path $Script:LockDir -ErrorAction Stop | Out-Null
        $acquired = $true
    } catch {
        # Existing lock: check if stale (owner process is gone). If stale,
        # remove and retry once.
        $pidFile = Join-Path $Script:LockDir "pid"
        if (Test-Path $pidFile) {
            $oldPid = (Get-Content $pidFile -ErrorAction SilentlyContinue) -as [int]
            if ($oldPid -and -not (Get-Process -Id $oldPid -ErrorAction SilentlyContinue)) {
                Remove-Item -Recurse -Force $Script:LockDir -ErrorAction SilentlyContinue
                try {
                    New-Item -ItemType Directory -Path $Script:LockDir -ErrorAction Stop | Out-Null
                    $acquired = $true
                } catch {
                    # Race with another installer that grabbed the freshly-
                    # released lock between our Remove-Item and New-Item.
                    # Fall through to the "another installer is running" branch.
                }
            }
        }
    }
    if (-not $acquired) {
        # Do NOT set LockOwned and do NOT attempt to remove $LockDir: it
        # belongs to another live installer (or a stale-PID lock that the
        # retry above failed to recover). Lock-Release will be a no-op.
        Write-ErrorExit "Another ee installer appears to be running (lock $Script:LockDir). Re-run after it finishes."
    }
    # Mark ownership immediately so even a fatal Write-ErrorExit below leaves
    # the global catch block (line ~end of file) eligible to clean up via
    # Lock-Release. The prior version exited without releasing the freshly-
    # created lock dir if the pid-file write failed.
    $Script:LockOwned = $true
    try {
        $PID | Out-File -Encoding ASCII -FilePath (Join-Path $Script:LockDir "pid")
    } catch {
        # We own the lock dir; clean it up so the next installer is not
        # blocked by a pid-less ownerless lock that stale-PID recovery
        # cannot fix.
        Remove-Item -Recurse -Force $Script:LockDir -ErrorAction SilentlyContinue
        $Script:LockOwned = $false
        Write-ErrorExit "Could not write pid file in installer lock ${Script:LockDir}: $_"
    }
}

function Lock-Release {
    # Only release a lock this process owns. The previous version would
    # delete the lock dir of a different installer if Main reached the
    # `finally` block after Lock-Acquire reported "another installer is
    # running" — a real risk because we cannot guarantee Lock-Acquire is
    # always called outside the outer try.
    if ($Script:LockOwned -and (Test-Path $Script:LockDir)) {
        Remove-Item -Recurse -Force $Script:LockDir -ErrorAction SilentlyContinue
        $Script:LockOwned = $false
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Already-installed
# ───────────────────────────────────────────────────────────────────────────

function Test-InstalledVersion {
    param([string]$BinaryPath, [string]$TargetVersion)
    if (-not (Test-Path $BinaryPath)) { return $false }
    try {
        $raw = (& $BinaryPath --version 2>$null | Select-Object -First 1)
        if (-not $raw) { return $false }
        if ($raw -match '([0-9]+\.[0-9]+\.[0-9]+)') {
            $installed = $Matches[1]
        } else {
            return $false
        }
        $target = $TargetVersion.TrimStart('v')
        return ($installed -eq $target)
    } catch {
        return $false
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Verification
# ───────────────────────────────────────────────────────────────────────────

function Test-Sha256 {
    param([string]$FilePath, [string]$ExpectedHash)
    Write-Info "Verifying SHA256 checksum..."
    $actual = (Get-FileHash -Path $FilePath -Algorithm SHA256).Hash.ToLower()
    $expected = $ExpectedHash.ToLower()
    if ($actual -ne $expected) {
        Remove-Item $FilePath -ErrorAction SilentlyContinue
        Write-ErrorExit "Checksum mismatch! Expected: $expected, Got: $actual"
    }
    Write-Ok "Checksum verified: $($actual.Substring(0, 16))..."
}

function Test-Sigstore {
    param([string]$TarballPath, [string]$BundlePath)
    $cosign = Get-Command cosign -ErrorAction SilentlyContinue
    if (-not $cosign) {
        Write-Warning2 "cosign not found; skipping Sigstore signature verification"
        Write-Warning2 "Install cosign for cryptographic authenticity checks (https://github.com/sigstore/cosign)"
        return
    }
    Write-Info "Verifying Sigstore signature..."
    # `$args` is an automatic variable in PowerShell functions; shadowing it
    # with a local assignment is legal but error-prone (e.g., under future
    # CmdletBinding the auto-var would not exist). Use a distinct name and
    # splat it explicitly.
    $cosignArgs = @(
        "verify-blob",
        "--bundle", $BundlePath,
        "--certificate-identity-regexp", $Script:CosignIdentityRegexp,
        "--certificate-oidc-issuer",     $Script:CosignOidcIssuer,
        $TarballPath
    )
    $output = & cosign @cosignArgs 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-ErrorExit "Sigstore signature verification failed: $output"
    }
    Write-Ok "Sigstore signature verified"
}

# ───────────────────────────────────────────────────────────────────────────
# Extraction
# ───────────────────────────────────────────────────────────────────────────

function Expand-Tarball {
    param([string]$TarballPath, [string]$DestDir)
    Write-Info "Extracting $([System.IO.Path]::GetFileName($TarballPath))..."
    if (-not (Test-Path $DestDir)) {
        New-Item -ItemType Directory -Path $DestDir -Force | Out-Null
    }

    # Strategy 1: bsdtar (built into Windows 10 1803+) handles .tar.xz natively.
    $tarCmd = Get-Command tar -ErrorAction SilentlyContinue
    if ($tarCmd) {
        & tar -xJf $TarballPath -C $DestDir 2>&1 | Out-Null
        if ($LASTEXITCODE -eq 0) {
            return
        }
        # Some Windows tars don't have xz; try the dual-tool approach.
        $xz = Get-Command xz -ErrorAction SilentlyContinue
        if ($xz) {
            $decompressed = Join-Path (Split-Path -Parent $TarballPath) "ee.tar"
            & xz -d -k -c $TarballPath > $decompressed
            & tar -xf $decompressed -C $DestDir
            Remove-Item $decompressed -ErrorAction SilentlyContinue
            if ($LASTEXITCODE -eq 0) { return }
        }
    }

    # Strategy 2: 7-Zip.
    $sevenZip = Get-Command 7z -ErrorAction SilentlyContinue
    if (-not $sevenZip) {
        $sevenZip = Get-Command "$env:ProgramFiles\7-Zip\7z.exe" -ErrorAction SilentlyContinue
    }
    if ($sevenZip) {
        $tarPath = Join-Path (Split-Path -Parent $TarballPath) "ee.tar"
        & $sevenZip.Path x $TarballPath "-o$(Split-Path -Parent $TarballPath)" -y | Out-Null
        if (Test-Path $tarPath) {
            & $sevenZip.Path x $tarPath "-o$DestDir" -y | Out-Null
            Remove-Item $tarPath -ErrorAction SilentlyContinue
            return
        }
    }

    Write-ErrorExit "xz decompression unavailable. Install bsdtar (Windows 10 1803+ ships one), or 7-Zip from https://7-zip.org/."
}

# ───────────────────────────────────────────────────────────────────────────
# PATH
# ───────────────────────────────────────────────────────────────────────────

function Update-UserPath {
    param([string]$Dir)
    $current = [Environment]::GetEnvironmentVariable("PATH", "User")
    $segments = if ($current) { $current -split ";" } else { @() }
    if ($segments -contains $Dir) {
        Write-Info "$Dir already in user PATH"
        return
    }
    Write-Info "Adding $Dir to user PATH..."
    $newPath = if ($current) { "$current;$Dir" } else { $Dir }
    [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
    $env:PATH = "$env:PATH;$Dir"
    Write-Ok "PATH updated. Restart your terminal for new sessions to pick it up."
}

function Install-Completions {
    param([string]$BinaryPath)
    try {
        $helpOut = & $BinaryPath completion --help 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Info "Shell completions: skipped (not supported in this build)"
            return
        }
    } catch {
        Write-Info "Shell completions: skipped (not supported in this build)"
        return
    }
    $profileDir = Split-Path -Parent $PROFILE
    if (-not (Test-Path $profileDir)) {
        New-Item -ItemType Directory -Path $profileDir -Force | Out-Null
    }
    $target = Join-Path $profileDir "ee-completion.ps1"
    try {
        & $BinaryPath completion powershell > $target 2>$null
        Write-Ok "PowerShell completions written to $target"
        Write-Info "Add this to your `$PROFILE: . `"$target`""
    } catch {
        Write-Warning2 "Failed to write PowerShell completions: $_"
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Self-test
# ───────────────────────────────────────────────────────────────────────────

function Invoke-SelfTest {
    param([string]$BinaryPath)
    Write-Info "Running self-test"
    try {
        $version = & $BinaryPath --version 2>&1
        Write-Host "  $version"
    } catch {
        Write-ErrorExit "Failed to run ee --version: $_"
    }
    try {
        $doctor = & $BinaryPath doctor --json 2>&1
        if ($LASTEXITCODE -eq 0) {
            Write-Ok "ee doctor: pass"
        } else {
            Write-Warning2 "ee doctor reported issues — inspect with: ee doctor --json | ConvertFrom-Json"
        }
    } catch {
        Write-Warning2 "ee doctor returned non-JSON output (this is OK for first run)"
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Build-from-source path
# ───────────────────────────────────────────────────────────────────────────

function Invoke-FromSource {
    param([string]$DestDir, [string]$BinaryName, [string]$VersionTag)
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        Write-ErrorExit "git not found — required for -FromSource"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-ErrorExit "cargo not found — install Rust nightly via https://rustup.rs/ before using -FromSource"
    }
    $src = Join-Path $env:TEMP "ee-src-$([guid]::NewGuid().ToString('N'))"
    try {
        Write-Info "Cloning $Script:RepoOwner/$Script:RepoName ..."
        if ($VersionTag) {
            & git clone --depth 1 --branch $VersionTag "https://github.com/$Script:RepoOwner/$Script:RepoName.git" $src 2>&1 | Out-Null
            if ($LASTEXITCODE -ne 0) {
                # Git refuses to clone into a non-empty directory. The pinned
                # clone may have partially populated $src before failing
                # (e.g., wrong tag); wipe it before the fallback so the
                # default-branch retry has somewhere to go.
                Remove-Item -Recurse -Force $src -ErrorAction SilentlyContinue
                & git clone --depth 1 "https://github.com/$Script:RepoOwner/$Script:RepoName.git" $src 2>&1 | Out-Null
            }
        } else {
            & git clone --depth 1 "https://github.com/$Script:RepoOwner/$Script:RepoName.git" $src 2>&1 | Out-Null
        }
        if ($LASTEXITCODE -ne 0) {
            Write-ErrorExit "git clone failed."
        }
        Write-Info "Building ee (release profile)…"
        Push-Location $src
        try {
            & cargo build --release
            if ($LASTEXITCODE -ne 0) {
                Write-ErrorExit "cargo build failed."
            }
        } finally {
            Pop-Location
        }

        # CARGO_TARGET_DIR may redirect the build output away from the
        # in-tree default. Probe the in-tree path first; if missing, ask
        # cargo where the binary landed.
        $built = Join-Path $src "target\release\$BinaryName"
        if (-not (Test-Path $built)) {
            Push-Location $src
            try {
                $meta = (& cargo metadata --no-deps --format-version 1 2>$null) -join ""
                if ($meta -and $meta -match '"target_directory":"([^"]+)"') {
                    $candidate = Join-Path $Matches[1] "release\$BinaryName"
                    if (Test-Path $candidate) { $built = $candidate }
                }
            } finally {
                Pop-Location
            }
        }
        if (-not (Test-Path $built)) {
            Write-ErrorExit "Build produced no $BinaryName at $built"
        }
        Copy-Item $built (Join-Path $DestDir $BinaryName) -Force
        Write-Ok "Installed to $DestDir\$BinaryName (built from source)"
    } finally {
        Remove-Item -Recurse -Force $src -ErrorAction SilentlyContinue
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Agent integration instructions
# ───────────────────────────────────────────────────────────────────────────

function Show-AgentIntegration {
    param(
        [System.Collections.Generic.List[string]]$Agents,
        [string]$BinaryPath
    )
    if ($NoConfigure -or $Quiet) { return }
    if ($Agents.Count -eq 0) { return }

    Write-Host ""
    if ($Script:Color) {
        Write-Host "Agent integration" -ForegroundColor Magenta
        Write-Host "ee is harness-agnostic — wire it into your agents at your own pace." -ForegroundColor DarkGray
    } else {
        Write-Host "Agent integration"
        Write-Host "ee is harness-agnostic — wire it into your agents at your own pace."
    }
    Write-Host ""

    if ($Agents -contains "Claude Code") {
        if ($Script:Color) { Write-Host "  -> Claude Code" -ForegroundColor Cyan } else { Write-Host "  -> Claude Code" }
        Write-Host "      Before risky shell commands:"
        Write-Host "        ee preflight check --cmd `"<shell command>`" --workspace . --json"
        Write-Host "      Before substantial work:"
        Write-Host "        ee context `"<task>`" --workspace . --max-tokens 4000 --format markdown"
        Write-Host ""
    }
    if ($Agents -contains "Codex CLI") {
        if ($Script:Color) { Write-Host "  -> Codex CLI" -ForegroundColor Cyan } else { Write-Host "  -> Codex CLI" }
        Write-Host "      Before substantial work:"
        Write-Host "        ee context `"<task>`" --workspace . --json"
        Write-Host "      Optional risk guard:"
        Write-Host "        ee preflight check --cmd `"<command>`" --workspace . --json"
        Write-Host ""
    }
    if ($Agents -contains "Gemini CLI") {
        if ($Script:Color) { Write-Host "  -> Gemini CLI" -ForegroundColor Cyan } else { Write-Host "  -> Gemini CLI" }
        Write-Host "      For BeforeTool integration, see docs/agent-ux/auto_enrollment_onboarding.md"
        Write-Host "      For context packs:"
        Write-Host "        ee context `"<task>`" --workspace . --json"
        Write-Host ""
    }
    if ($Agents -contains "Cursor IDE") {
        if ($Script:Color) { Write-Host "  -> Cursor IDE" -ForegroundColor Cyan } else { Write-Host "  -> Cursor IDE" }
        Write-Host "      Cursor beforeShellExecution can call:"
        Write-Host "        ee preflight check --cmd `"`$COMMAND`" --workspace . --json"
        Write-Host ""
    }
    $other = @("Aider", "Continue", "GitHub Copilot CLI") | Where-Object { $Agents -contains $_ }
    if ($other.Count -gt 0) {
        if ($Script:Color) { Write-Host "  -> Aider / Continue / Copilot CLI" -ForegroundColor Cyan } else { Write-Host "  -> Aider / Continue / Copilot CLI" }
        Write-Host "      No documented PreToolUse surface for ee yet. Call directly from your prompt setup:"
        Write-Host "        ee context `"<task>`" --workspace . --json"
        Write-Host ""
    }
}

# ───────────────────────────────────────────────────────────────────────────
# Final summary
# ───────────────────────────────────────────────────────────────────────────

function Show-Summary {
    param([string]$BinaryPath, [string]$VersionTag, [string]$Target)
    if ($Quiet) { return }
    Write-Host ""
    if ($Script:Color) {
        Write-Host "ee is installed!" -ForegroundColor Green
    } else {
        Write-Host "ee is installed!"
    }
    Write-Host ""
    Write-Host "  Binary:     $BinaryPath"
    if ($VersionTag) { Write-Host "  Version:    $VersionTag" }
    Write-Host "  Target:     $Target"
    Write-Host ""
    Write-Host "  Get started:"
    Write-Host "    ee init --workspace ."
    Write-Host "    ee context `"<task>`" --workspace . --max-tokens 4000"
    Write-Host "    ee --help"
    Write-Host ""
    Write-Host "  Inspect health: ee doctor --json"
    Write-Host "  Uninstall:      Remove-Item `"$BinaryPath`""
    Write-Host "                  (config in %APPDATA%\ee\ persists; remove manually if desired)"
}

# ───────────────────────────────────────────────────────────────────────────
# Main
# ───────────────────────────────────────────────────────────────────────────

function Main {
    Write-Header

    # Agent scan first (informational only).
    $agents = Get-DetectedAgents
    Show-DetectedAgents -Agents $agents

    # Platform.
    $target = Get-PlatformTarget
    Write-Info "Platform: Windows / $target"

    # Default install dir.
    if (-not $InstallDir) {
        $InstallDir = Join-Path $env:LOCALAPPDATA "ee\bin"
    }
    Write-Info "Install directory: $InstallDir"

    # Version.
    if ($FromSource) {
        if (-not $Version) { $Version = "" }
    } else {
        if (-not $Version -and -not $ArtifactUrl) {
            $Version = Get-LatestVersion
        }
    }
    if ($Version) { $Version = ConvertTo-TagName $Version }
    if ($Version) { Write-Info "Version: $Version" }

    # Asset URL.
    $tarballName  = "ee-$target.tar.xz"
    $sha256Name   = "$tarballName.sha256"
    $sigstoreName = "$tarballName.sigstore.json"
    $effectiveUrl = if ($ArtifactUrl) {
        $ArtifactUrl
    } elseif ($Version) {
        "https://github.com/$Script:RepoOwner/$Script:RepoName/releases/download/$Version/$tarballName"
    } else {
        $null
    }

    # Preflight.
    Write-Info "Running preflight checks"
    Test-DiskSpace -Path $InstallDir -MinMb 20
    Test-WritePermission -Dir $InstallDir
    Test-Network -Url $effectiveUrl

    $binaryPath = Join-Path $InstallDir $Script:BinaryName

    # Already-installed short-circuit.
    if (-not $FromSource -and -not $Force -and $Version -and (Test-InstalledVersion -BinaryPath $binaryPath -TargetVersion $Version)) {
        Write-Ok "$Script:ProjectName $Version is already installed at $binaryPath"
        Write-Info "Use -Force to reinstall"
        Install-Completions -BinaryPath $binaryPath
        return
    }

    # Lock.
    Lock-Acquire
    try {
        $tempDir = Join-Path $env:TEMP "ee-install-$([guid]::NewGuid().ToString('N'))"
        New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

        try {
            if ($FromSource) {
                Invoke-FromSource -DestDir $InstallDir -BinaryName $Script:BinaryName -VersionTag $Version
            } else {
                $tarballPath  = Join-Path $tempDir $tarballName
                $sha256Path   = Join-Path $tempDir $sha256Name
                $sigstorePath = Join-Path $tempDir $sigstoreName

                Write-Info "Downloading $effectiveUrl"
                try {
                    Invoke-DownloadFile -Url $effectiveUrl -OutFile $tarballPath
                } catch {
                    Write-Warning2 "Artifact download failed: $($_.Exception.Message)"
                    Write-Warning2 "Falling back to -FromSource"
                    Invoke-FromSource -DestDir $InstallDir -BinaryName $Script:BinaryName -VersionTag $Version
                    Install-Completions -BinaryPath $binaryPath
                    if ($Verify) { Invoke-SelfTest -BinaryPath $binaryPath }
                    Show-AgentIntegration -Agents $agents -BinaryPath $binaryPath
                    Show-Summary -BinaryPath $binaryPath -VersionTag $Version -Target $target
                    return
                }

                if ($NoVerify) {
                    Write-Warning2 "Verification skipped (-NoVerify / EE_SKIP_VERIFY=1)"
                } else {
                    if (-not $Checksum) {
                        if (-not $ChecksumUrl) { $ChecksumUrl = "$effectiveUrl.sha256" }
                        Write-Info "Fetching checksum from $ChecksumUrl"
                        try {
                            Invoke-DownloadFile -Url $ChecksumUrl -OutFile $sha256Path
                            $Checksum = ((Get-Content $sha256Path -Raw).Trim() -split "\s+")[0]
                        } catch {
                            Write-ErrorExit "Could not fetch checksum: $($_.Exception.Message)"
                        }
                    }
                    Test-Sha256 -FilePath $tarballPath -ExpectedHash $Checksum

                    $cosign = Get-Command cosign -ErrorAction SilentlyContinue
                    if ($cosign) {
                        try {
                            Invoke-DownloadFile -Url "$effectiveUrl.sigstore.json" -OutFile $sigstorePath
                            Test-Sigstore -TarballPath $tarballPath -BundlePath $sigstorePath
                        } catch {
                            Write-Warning2 "Sigstore bundle unavailable: $($_.Exception.Message). Continuing without signature check."
                        }
                    } else {
                        Write-Warning2 "cosign not found; skipping Sigstore signature verification"
                    }
                }

                Expand-Tarball -TarballPath $tarballPath -DestDir $InstallDir

                # The release tarball top-level is just `ee.exe`; account for any subdir layout.
                $finalBinary = Join-Path $InstallDir $Script:BinaryName
                if (-not (Test-Path $finalBinary)) {
                    $found = Get-ChildItem -Path $InstallDir -Filter $Script:BinaryName -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1
                    if ($found) {
                        Move-Item $found.FullName $finalBinary -Force
                        $parent = Split-Path -Parent $found.FullName
                        if ($parent -ne $InstallDir) {
                            Remove-Item -Recurse -Force $parent -ErrorAction SilentlyContinue
                        }
                    } else {
                        Write-ErrorExit "$Script:BinaryName not found after extraction"
                    }
                }
                Write-Ok "Installed to $finalBinary"
            }

            Update-UserPath -Dir $InstallDir
            Install-Completions -BinaryPath $binaryPath
            if ($Verify) { Invoke-SelfTest -BinaryPath $binaryPath }
            Show-AgentIntegration -Agents $agents -BinaryPath $binaryPath
            Show-Summary -BinaryPath $binaryPath -VersionTag $Version -Target $target
        }
        finally {
            Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue
        }
    }
    finally {
        Lock-Release
    }
}

try {
    Main
} catch {
    Write-Host ""
    Write-Host "Installation failed: $_" -ForegroundColor Red
    # Only release a lock we own. The previous version unconditionally
    # deleted $Script:LockDir if it existed — that could clobber another
    # installer's live lock when Main died for unrelated reasons before
    # Lock-Acquire ran.
    if ($Script:LockOwned -and (Test-Path $Script:LockDir)) {
        Remove-Item -Recurse -Force $Script:LockDir -ErrorAction SilentlyContinue
    }
    exit 1
}
