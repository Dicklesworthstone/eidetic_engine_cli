param(
    [Parameter(Mandatory = $true)]
    [string] $EeBinary,

    [string] $WorkspaceRoot = $(Join-Path $env:LOCALAPPDATA "ee-windows-msvc-cli-smoke-script"),

    [string] $LogPath = $(Join-Path $env:TEMP "ee-windows-msvc-cli-smoke.jsonl")
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

function Write-TestEvent {
    param(
        [string] $Phase,
        [string] $Command,
        [int] $ExitCode,
        [string] $StdoutPath,
        [string] $StderrPath,
        [string] $Diagnosis = ""
    )
    $event = [ordered]@{
        schema = "ee.test_event.v1"
        kind = "windows_msvc_cli_smoke"
        bead_id = "bd-3usjw.68"
        surface = "windows_msvc_cli_smoke"
        phase = $Phase
        command = $Command
        target_triple = "x86_64-pc-windows-msvc"
        path_root = $WorkspaceRoot
        exit_code = $ExitCode
        stdout_path = $StdoutPath
        stderr_path = $StderrPath
        first_failure_diagnosis = $Diagnosis
    }
    $event | ConvertTo-Json -Compress | Add-Content -Path $LogPath -Encoding utf8
}

function Invoke-Ee {
    param(
        [string] $Name,
        [string[]] $Arguments
    )
    $artifactDir = Join-Path $WorkspaceRoot "artifacts"
    New-Item -ItemType Directory -Force -Path $artifactDir | Out-Null
    $stdoutPath = Join-Path $artifactDir "$Name.stdout.json"
    $stderrPath = Join-Path $artifactDir "$Name.stderr.txt"
    $allArgs = @("--workspace", $WorkspaceRoot, "--json") + $Arguments
    $commandText = "$EeBinary $($allArgs -join ' ')"
    if ((Test-Path $stdoutPath) -or (Test-Path $stderrPath)) {
        throw "refusing to overwrite stale command artifacts for $Name under $artifactDir"
    }
    Write-TestEvent -Phase "input" -Command $commandText -ExitCode 0 -StdoutPath $stdoutPath -StderrPath $stderrPath

    & $EeBinary @allArgs 1> $stdoutPath 2> $stderrPath
    $exitCode = $LASTEXITCODE
    if ($exitCode -ne 0) {
        $stderrSummary = [string](Get-Content -Raw -Path $stderrPath)
        $stderrSummary = $stderrSummary.Trim() -replace "\s+", " "
        if ($stderrSummary.Length -gt 512) {
            $stderrSummary = $stderrSummary.Substring(0, 512)
        }
        $diagnosis = "command returned nonzero"
        if (-not [string]::IsNullOrWhiteSpace($stderrSummary)) {
            $diagnosis = "$diagnosis; stderr: $stderrSummary"
        }
        Write-TestEvent -Phase "response" -Command $commandText -ExitCode $exitCode -StdoutPath $stdoutPath -StderrPath $stderrPath -Diagnosis $diagnosis
        throw "ee command failed: $commandText"
    }

    try {
        $json = Get-Content -Raw -Path $stdoutPath | ConvertFrom-Json
    }
    catch {
        Write-TestEvent -Phase "response" -Command $commandText -ExitCode $exitCode -StdoutPath $stdoutPath -StderrPath $stderrPath -Diagnosis "stdout was not valid JSON"
        throw
    }
    if ($json.schema -ne "ee.response.v2" -or $json.success -ne $true) {
        Write-TestEvent -Phase "response" -Command $commandText -ExitCode $exitCode -StdoutPath $stdoutPath -StderrPath $stderrPath -Diagnosis "stdout was not a successful ee.response.v2 envelope"
        throw "ee command returned an unexpected response envelope: $commandText"
    }
    Write-TestEvent -Phase "response" -Command $commandText -ExitCode $exitCode -StdoutPath $stdoutPath -StderrPath $stderrPath
    return $json
}

if ([string]::IsNullOrWhiteSpace($env:APPDATA)) {
    throw "APPDATA must be set; set APPDATA or pass --workspace explicitly."
}
if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) {
    throw "LOCALAPPDATA must be set; set LOCALAPPDATA or pass --workspace explicitly."
}

New-Item -ItemType Directory -Force -Path $WorkspaceRoot | Out-Null
$logParent = Split-Path -Parent $LogPath
if (-not [string]::IsNullOrWhiteSpace($logParent)) {
    New-Item -ItemType Directory -Force -Path $logParent | Out-Null
}
if (Test-Path $LogPath) {
    throw "refusing to overwrite stale smoke log: $LogPath"
}
New-Item -ItemType File -Path $LogPath | Out-Null

Invoke-Ee -Name "01-init" -Arguments @("init") | Out-Null
$remember = Invoke-Ee -Name "02-remember" -Arguments @(
    "remember",
    "Run cargo fmt --check before release on Windows MSVC.",
    "--level",
    "procedural",
    "--kind",
    "rule"
)
$memoryId = $remember.data.memory_id
if ([string]::IsNullOrWhiteSpace($memoryId)) {
    throw "remember response did not contain data.memory_id"
}
Invoke-Ee -Name "03-search" -Arguments @("search", "Windows release fmt") | Out-Null
Invoke-Ee -Name "04-pack" -Arguments @("pack", "prepare Windows release") | Out-Null
Invoke-Ee -Name "05-why" -Arguments @("why", $memoryId) | Out-Null
Invoke-Ee -Name "06-status" -Arguments @("status") | Out-Null
Invoke-Ee -Name "07-doctor" -Arguments @("doctor") | Out-Null
$created = Invoke-Ee -Name "07b-team-create" -Arguments @("team", "create", "--name", "Windows Smoke")
$teamId = $created.data.team.teamId
if ([string]::IsNullOrWhiteSpace($teamId) -or -not $teamId.StartsWith("team_")) {
    throw "team create response did not contain data.team.teamId"
}
Invoke-Ee -Name "07c-team-status" -Arguments @("team", "status") | Out-Null
Invoke-Ee -Name "07d-team-doctor" -Arguments @("team", "doctor") | Out-Null
Invoke-Ee -Name "08-export" -Arguments @("export", "--output-dir", (Join-Path $WorkspaceRoot "export"), "--redaction", "none") | Out-Null
$backup = Invoke-Ee -Name "09-backup-create" -Arguments @(
    "backup",
    "create",
    "--output-dir",
    (Join-Path $WorkspaceRoot "backups"),
    "--redaction",
    "none",
    "--label",
    "windows-msvc-cli-smoke"
)
Invoke-Ee -Name "10-backup-restore" -Arguments @(
    "backup",
    "restore",
    $backup.data.backupId,
    "--output-dir",
    (Join-Path $WorkspaceRoot "backups"),
    "--side-path",
    (Join-Path $WorkspaceRoot "restore-side-path")
) | Out-Null
$capsule = Join-Path $WorkspaceRoot "handoff-capsule.json"
Invoke-Ee -Name "11-handoff-create" -Arguments @("handoff", "create", "--out", $capsule, "--profile", "resume") | Out-Null
Invoke-Ee -Name "12-handoff-resume" -Arguments @("handoff", "resume", $capsule) | Out-Null

Write-Host "Windows MSVC CLI smoke passed. Log: $LogPath"
