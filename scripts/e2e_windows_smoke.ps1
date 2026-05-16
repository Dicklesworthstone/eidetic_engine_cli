param(
    [Parameter(Mandatory = $true)]
    [string] $EeBinary,

    [string] $WorkspaceRoot = $(Join-Path $env:LOCALAPPDATA "ee-windows-msvc-cli-smoke-script"),

    [string] $LogPath = $(Join-Path $env:TEMP "ee-windows-msvc-cli-smoke.jsonl")
)

$ErrorActionPreference = "Stop"

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
    Write-TestEvent -Phase "input" -Command $commandText -ExitCode 0 -StdoutPath $stdoutPath -StderrPath $stderrPath

    $output = & $EeBinary @allArgs 2>&1
    $exitCode = $LASTEXITCODE
    $stdout = $output | Out-String
    Set-Content -Path $stdoutPath -Value $stdout -Encoding utf8
    Set-Content -Path $stderrPath -Value "" -Encoding utf8
    if ($exitCode -ne 0) {
        Write-TestEvent -Phase "response" -Command $commandText -ExitCode $exitCode -StdoutPath $stdoutPath -StderrPath $stderrPath -Diagnosis "command returned nonzero"
        throw "ee command failed: $commandText"
    }
    $json = Get-Content -Raw -Path $stdoutPath | ConvertFrom-Json
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
Remove-Item -Force -ErrorAction SilentlyContinue $LogPath

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
Invoke-Ee -Name "03-search" -Arguments @("search", "Windows release fmt") | Out-Null
Invoke-Ee -Name "04-context" -Arguments @("context", "prepare Windows release") | Out-Null
Invoke-Ee -Name "05-why" -Arguments @("why", $memoryId) | Out-Null
Invoke-Ee -Name "06-status" -Arguments @("status") | Out-Null
Invoke-Ee -Name "07-doctor" -Arguments @("doctor") | Out-Null
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
