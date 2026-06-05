[CmdletBinding()]
param(
    [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [string] $Repo = "Dicklesworthstone/eidetic_engine_cli",
    [string] $Version = "latest",
    [string] $ArtifactRoot = (Join-Path ([System.IO.Path]::GetTempPath()) "ee-windows-installer-live-smoke"),
    [string] $InstallRoot = (Join-Path ([System.IO.Path]::GetTempPath()) "ee-windows-installer-live"),
    [string] $LogPath = (Join-Path ([System.IO.Path]::GetTempPath()) "ee-windows-installer-live-smoke.jsonl"),
    [switch] $ResetLog
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$ResolvedTag = ""
$InstallerUrl = ""
$DownloadedInstallerHash = ""
$InstallerOutputPath = ""
$InstalledBinary = ""
$CargoPathSuppressed = $false

$logDir = Split-Path -Parent $LogPath
if (-not [string]::IsNullOrWhiteSpace($logDir)) {
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
}
if ($ResetLog) {
    [System.IO.File]::WriteAllText($LogPath, "", [System.Text.Encoding]::UTF8)
}
New-Item -ItemType Directory -Force -Path $ArtifactRoot | Out-Null
New-Item -ItemType Directory -Force -Path $InstallRoot | Out-Null

$originalProcessEnv = @{}
foreach ($name in @("APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "USERPROFILE", "PATH", "CARGO_HOME", "RUSTUP_HOME", "NO_COLOR")) {
    $originalProcessEnv[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}

function Restore-ProcessEnv {
    foreach ($entry in $originalProcessEnv.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
    }
}

function ConvertTo-DisplayPath {
    param([string] $Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return "" }
    $runnerTemp = [Environment]::GetEnvironmentVariable("RUNNER_TEMP", "Process")
    if (-not [string]::IsNullOrWhiteSpace($runnerTemp) -and $Path.StartsWith($runnerTemp, [System.StringComparison]::OrdinalIgnoreCase)) {
        return $Path.Replace($runnerTemp, "[RUNNER_TEMP]")
    }
    return $Path
}

function Write-LiveSmokeEvent {
    param(
        [Parameter(Mandatory = $true)][string] $Phase,
        [Parameter(Mandatory = $true)][ValidateSet("pass", "fail")][string] $Result,
        [string] $Diagnosis = "",
        [hashtable] $Details = @{}
    )

    $event = [ordered]@{
        schema = "ee.test_event.v1"
        kind = "windows_installer_live_smoke"
        bead_id = "bd-3tprq.4"
        related_bead_ids = @("bd-3tprq.4", "bd-3tprq")
        surface = "install_ps1_live_release_smoke"
        phase = $Phase
        result = $Result
        repository = $Repo
        version = $ResolvedTag
        powershell_version = $PSVersionTable.PSVersion.ToString()
        os = [System.Environment]::OSVersion.VersionString
        repo_root = ConvertTo-DisplayPath -Path $RepoRoot
        artifact_root = ConvertTo-DisplayPath -Path $ArtifactRoot
        install_root = ConvertTo-DisplayPath -Path $InstallRoot
        installed_binary = ConvertTo-DisplayPath -Path $InstalledBinary
        installer_url = $InstallerUrl
        installer_sha256 = $DownloadedInstallerHash
        installer_output_artifact = ConvertTo-DisplayPath -Path $InstallerOutputPath
        cargo_path_suppressed = $CargoPathSuppressed
        first_failure_diagnosis = $Diagnosis
        details = $Details
    }
    $event | ConvertTo-Json -Compress -Depth 8 | Add-Content -Path $LogPath -Encoding utf8
}

function Fail-Smoke {
    param([string] $Phase, [string] $Diagnosis, [hashtable] $Details = @{})
    Write-LiveSmokeEvent -Phase $Phase -Result "fail" -Diagnosis $Diagnosis -Details $Details
    throw $Diagnosis
}

function Resolve-ReleaseTag {
    if ($Version -ne "latest") {
        if ($Version.StartsWith("v")) { return $Version }
        return "v$Version"
    }

    $headers = @{ "User-Agent" = "ee-windows-installer-live-smoke" }
    if (-not [string]::IsNullOrWhiteSpace($env:GH_TOKEN)) {
        $headers["Authorization"] = "Bearer $env:GH_TOKEN"
        $headers["X-GitHub-Api-Version"] = "2022-11-28"
    }
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers $headers -TimeoutSec 60
    $tag = [string] $release.tag_name
    if ([string]::IsNullOrWhiteSpace($tag)) {
        throw "latest release response did not include tag_name"
    }
    return $tag
}

function Remove-RustFromProcessPath {
    if ([string]::IsNullOrWhiteSpace($env:PATH)) { return }
    $kept = New-Object "System.Collections.Generic.List[string]"
    foreach ($segment in ($env:PATH -split ";")) {
        if ([string]::IsNullOrWhiteSpace($segment)) { continue }
        $lower = $segment.ToLowerInvariant()
        if ($lower.Contains("\.cargo\bin") -or $lower.Contains("\.rustup") -or $lower.Contains("\cargo\bin") -or $lower.Contains("\rustup")) {
            $script:CargoPathSuppressed = $true
            continue
        }
        $kept.Add($segment) | Out-Null
    }
    $env:PATH = ($kept -join ";")
}

function Find-PowerShellHost {
    $pwsh = Get-Command "pwsh" -ErrorAction SilentlyContinue
    if ($pwsh) { return $pwsh.Source }
    $powershell = Get-Command "powershell" -ErrorAction SilentlyContinue
    if ($powershell) { return $powershell.Source }
    throw "neither pwsh nor powershell is available"
}

try {
    if ([string]::IsNullOrWhiteSpace($env:WINDIR)) {
        Fail-Smoke -Phase "preflight" -Diagnosis "windows-installer-live-smoke.ps1 must run on Windows"
    }

    $sandboxUserProfile = Join-Path $ArtifactRoot "userprofile"
    $sandboxLocalAppData = Join-Path $ArtifactRoot "localappdata"
    $sandboxAppData = Join-Path $ArtifactRoot "appdata"
    $sandboxTemp = Join-Path $ArtifactRoot "temp"
    foreach ($dir in @($sandboxUserProfile, $sandboxLocalAppData, $sandboxAppData, $sandboxTemp)) {
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
    }
    $env:USERPROFILE = $sandboxUserProfile
    $env:LOCALAPPDATA = $sandboxLocalAppData
    $env:APPDATA = $sandboxAppData
    $env:TEMP = $sandboxTemp
    $env:TMP = $sandboxTemp
    $env:CARGO_HOME = Join-Path $ArtifactRoot "cargo-home-disabled"
    $env:RUSTUP_HOME = Join-Path $ArtifactRoot "rustup-home-disabled"
    $env:NO_COLOR = "1"
    Remove-RustFromProcessPath

    Write-LiveSmokeEvent -Phase "preflight" -Result "pass" -Details @{
        isolated_environment = $true
        cargo_path_suppressed = $CargoPathSuppressed
    }

    try {
        $ResolvedTag = Resolve-ReleaseTag
    } catch {
        Fail-Smoke -Phase "resolve_release" -Diagnosis "failed to resolve latest release: $($_.Exception.Message)"
    }
    Write-LiveSmokeEvent -Phase "resolve_release" -Result "pass"

    $InstallerUrl = "https://github.com/$Repo/releases/download/$ResolvedTag/install.ps1"
    $installerPath = Join-Path $ArtifactRoot "install.ps1"
    try {
        Invoke-WebRequest -Uri $InstallerUrl -UseBasicParsing -OutFile $installerPath -TimeoutSec 60
        $DownloadedInstallerHash = (Get-FileHash -Path $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    } catch {
        Fail-Smoke -Phase "download_installer" -Diagnosis "failed to download release install.ps1 with Invoke-WebRequest -OutFile: $($_.Exception.Message)"
    }
    Write-LiveSmokeEvent -Phase "download_installer" -Result "pass" -Details @{
        used_outfile = $true
    }

    $installBin = Join-Path $InstallRoot "bin"
    New-Item -ItemType Directory -Force -Path $installBin | Out-Null
    $InstalledBinary = Join-Path $installBin "ee.exe"
    $InstallerOutputPath = Join-Path $ArtifactRoot "installer-output.txt"
    $psExe = Find-PowerShellHost
    $installerArgs = @(
        "-NoLogo",
        "-NoProfile",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        $installerPath,
        "-Version",
        $ResolvedTag,
        "-InstallDir",
        $installBin,
        "-Verify",
        "-NoConfigure",
        "-Force"
    )
    $installerOutput = @(& $psExe @installerArgs 2>&1)
    $installerExitCode = $LASTEXITCODE
    [System.IO.File]::WriteAllLines($InstallerOutputPath, [string[]] @($installerOutput | ForEach-Object { [string] $_ }), [System.Text.Encoding]::UTF8)
    if ($installerExitCode -ne 0) {
        $tail = (@($installerOutput | Select-Object -Last 20) -join " | ")
        Fail-Smoke -Phase "invoke_installer" -Diagnosis "release installer exited $installerExitCode; tail: $tail" -Details @{
            exit_code = $installerExitCode
        }
    }
    Write-LiveSmokeEvent -Phase "invoke_installer" -Result "pass" -Details @{
        exit_code = $installerExitCode
        install_dir = ConvertTo-DisplayPath -Path $installBin
        verify_flag = $true
        no_configure_flag = $true
    }

    if (-not (Test-Path $InstalledBinary)) {
        Fail-Smoke -Phase "verify_binary_version" -Diagnosis "installed ee.exe was not found at $(ConvertTo-DisplayPath -Path $InstalledBinary)"
    }
    $expectedVersion = $ResolvedTag.TrimStart("v")
    $versionOutput = @(& $InstalledBinary --version 2>&1)
    $versionExitCode = $LASTEXITCODE
    $versionText = ($versionOutput -join "`n")
    if ($versionExitCode -ne 0 -or $versionText.IndexOf($expectedVersion, [System.StringComparison]::OrdinalIgnoreCase) -lt 0) {
        Fail-Smoke -Phase "verify_binary_version" -Diagnosis "ee --version did not report $expectedVersion; exit=$versionExitCode output=$versionText" -Details @{
            exit_code = $versionExitCode
            output = $versionText
        }
    }
    Write-LiveSmokeEvent -Phase "verify_binary_version" -Result "pass" -Details @{
        expected_version = $expectedVersion
        output = $versionText
    }

    $doctorOutput = @(& $InstalledBinary doctor --json 2>&1)
    $doctorExitCode = $LASTEXITCODE
    $doctorText = ($doctorOutput -join "`n")
    if ($doctorExitCode -ne 0) {
        Fail-Smoke -Phase "doctor_json" -Diagnosis "ee doctor --json exited $doctorExitCode; output=$doctorText" -Details @{
            exit_code = $doctorExitCode
            output = $doctorText
        }
    }
    try {
        $doctorJson = $doctorText | ConvertFrom-Json
        $doctorSchema = [string] $doctorJson.schema
    } catch {
        Fail-Smoke -Phase "doctor_json" -Diagnosis "ee doctor --json did not emit parseable JSON: $($_.Exception.Message)" -Details @{
            output = $doctorText
        }
    }
    Write-LiveSmokeEvent -Phase "doctor_json" -Result "pass" -Details @{
        exit_code = $doctorExitCode
        schema = $doctorSchema
    }
} finally {
    Restore-ProcessEnv
}
