[CmdletBinding()]
param(
    [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [string] $ArtifactRoot = (Join-Path ([System.IO.Path]::GetTempPath()) "ee-windows-installer-mocked-flow"),
    [string] $LogPath = (Join-Path ([System.IO.Path]::GetTempPath()) "ee-windows-installer-mocked-flow.jsonl"),
    [switch] $ResetLog,
    [switch] $ExtractionOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$installPath = Join-Path $RepoRoot "install.ps1"
$logDir = Split-Path -Parent $LogPath
if (-not [string]::IsNullOrWhiteSpace($logDir)) {
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
}
if ($ResetLog) {
    [System.IO.File]::WriteAllText($LogPath, "", [System.Text.Encoding]::UTF8)
}
New-Item -ItemType Directory -Force -Path $ArtifactRoot | Out-Null

if ([string]::IsNullOrWhiteSpace($env:WINDIR)) {
    throw "windows-installer-mocked-flow-check.ps1 must run on Windows"
}

$scriptHash = ""
if (Test-Path $installPath) {
    $scriptHash = (Get-FileHash -Path $installPath -Algorithm SHA256).Hash.ToLowerInvariant()
}

$failures = New-Object "System.Collections.Generic.List[string]"
$originalProcessEnv = @{}
foreach ($name in @("APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "USERPROFILE", "PATH", "PSModulePath", "EE_REQUIRE_PROVENANCE", "EE_SKIP_VERIFY", "EE_MOCK_COSIGN_LOG", "NO_COLOR")) {
    $originalProcessEnv[$name] = [Environment]::GetEnvironmentVariable($name, "Process")
}
$originalUserPath = [Environment]::GetEnvironmentVariable("PATH", "User")

function Set-ProcessEnv {
    param([string] $Name, [string] $Value)
    [Environment]::SetEnvironmentVariable($Name, $Value, "Process")
}

function Restore-Environment {
    foreach ($entry in $originalProcessEnv.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
    }
    [Environment]::SetEnvironmentVariable("PATH", $originalUserPath, "User")
}

function Text-Contains {
    param([string] $Haystack, [string] $Needle)
    return $Haystack.IndexOf($Needle, [System.StringComparison]::OrdinalIgnoreCase) -ge 0
}

function Write-FlowEvent {
    param(
        [Parameter(Mandatory = $true)][string] $ScenarioId,
        [Parameter(Mandatory = $true)][ValidateSet("pass", "fail")][string] $Result,
        [Parameter(Mandatory = $true)][int] $ExpectedExitCode,
        [Parameter(Mandatory = $true)][int] $ActualExitCode,
        [Parameter(Mandatory = $true)][hashtable] $MockedInputs,
        [Parameter(Mandatory = $true)][string[]] $RequiredOutput,
        [Parameter(Mandatory = $true)][string[]] $ForbiddenOutput,
        [Parameter(Mandatory = $true)][bool] $ExpectedCosignInvocation,
        [Parameter(Mandatory = $true)][bool] $ActualCosignInvocation,
        [string] $StdoutArtifact = "",
        [string] $FirstFailureDiagnosis = ""
    )

    $event = [ordered]@{
        schema = "ee.test_event.v1"
        kind = "windows_installer_mocked_flow"
        bead_id = "bd-3tprq.3"
        related_bead_ids = @("bd-3tprq.3", "bd-3tprq.5")
        surface = "install_ps1_mocked_verification_flow"
        scenario_id = $ScenarioId
        result = $Result
        shell = $PSVersionTable.PSEdition
        powershell_version = $PSVersionTable.PSVersion.ToString()
        os = [System.Environment]::OSVersion.VersionString
        script_hash = $scriptHash
        expected_exit_code = $ExpectedExitCode
        actual_exit_code = $ActualExitCode
        mocked_inputs = $MockedInputs
        required_output_markers = $RequiredOutput
        forbidden_output_markers = $ForbiddenOutput
        expected_cosign_invoked = $ExpectedCosignInvocation
        actual_cosign_invoked = $ActualCosignInvocation
        stdout_artifact = $StdoutArtifact
        first_failure_diagnosis = $FirstFailureDiagnosis
    }
    $event | ConvertTo-Json -Compress -Depth 8 | Add-Content -Path $LogPath -Encoding utf8
    if ($Result -eq "fail") {
        $failures.Add("${ScenarioId}: $FirstFailureDiagnosis") | Out-Null
    }
}

function Find-Python {
    foreach ($candidate in @("python", "python3")) {
        $cmd = Get-Command $candidate -ErrorAction SilentlyContinue
        if ($cmd) {
            return [pscustomobject]@{ Exe = $cmd.Source; PrefixArgs = @() }
        }
    }
    $py = Get-Command "py" -ErrorAction SilentlyContinue
    if ($py) {
        return [pscustomobject]@{ Exe = $py.Source; PrefixArgs = @("-3") }
    }
    throw "python is required to serve loopback installer fixtures"
}

function Get-FreeLoopbackPort {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Parse("127.0.0.1"), 0)
    try {
        $listener.Start()
        return ([System.Net.IPEndPoint] $listener.LocalEndpoint).Port
    } finally {
        $listener.Stop()
    }
}

function Start-FixtureServer {
    param([Parameter(Mandatory = $true)][string] $Root)

    $python = Find-Python
    $port = Get-FreeLoopbackPort
    $stdoutPath = Join-Path $Root "fixture-server.stdout.txt"
    $stderrPath = Join-Path $Root "fixture-server.stderr.txt"
    $serverArgs = @($python.PrefixArgs) + @("-m", "http.server", [string] $port, "--bind", "127.0.0.1", "--directory", $Root)
    $process = Start-Process -FilePath $python.Exe -ArgumentList $serverArgs -PassThru -WindowStyle Hidden -RedirectStandardOutput $stdoutPath -RedirectStandardError $stderrPath
    $probeUrl = "http://127.0.0.1:$port/ee-x86_64-pc-windows-msvc.tar.xz"
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        try {
            Invoke-WebRequest -Uri $probeUrl -UseBasicParsing -TimeoutSec 2 | Out-Null
            return [pscustomobject]@{ Process = $process; Port = $port; BaseUrl = "http://127.0.0.1:$port" }
        } catch {
            Start-Sleep -Milliseconds 250
        }
    }
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    throw "local fixture HTTP server did not become ready"
}

function New-BaseArtifact {
    $baseRoot = Join-Path $ArtifactRoot "base"
    $payloadRoot = Join-Path $baseRoot "payload"
    New-Item -ItemType Directory -Force -Path $payloadRoot | Out-Null
    $payloadExe = ""
    if (-not [string]::IsNullOrWhiteSpace($env:WINDIR)) {
        $whereExe = Join-Path $env:WINDIR "System32\where.exe"
        if (Test-Path $whereExe) {
            $payloadExe = $whereExe
        }
    }
    if ([string]::IsNullOrWhiteSpace($payloadExe)) {
        $payloadExe = (Get-Process -Id $PID).Path
    }
    if ([string]::IsNullOrWhiteSpace($payloadExe) -or -not (Test-Path $payloadExe)) {
        $pwsh = Get-Command "pwsh" -ErrorAction SilentlyContinue
        if (-not $pwsh) { throw "could not locate current PowerShell executable for ee.exe payload" }
        $payloadExe = $pwsh.Source
    }
    Copy-Item $payloadExe (Join-Path $payloadRoot "ee.exe") -Force

    $tarballName = "ee-x86_64-pc-windows-msvc.tar.xz"
    $tarballPath = Join-Path $baseRoot $tarballName
    $tar = Get-Command "tar" -ErrorAction SilentlyContinue
    if (-not $tar) { throw "tar is required to create installer fixture archive" }
    & $tar.Source -cJf $tarballPath -C $payloadRoot "ee.exe" | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $tarballPath)) {
        throw "tar failed to create $tarballName"
    }
    $checksum = (Get-FileHash -Path $tarballPath -Algorithm SHA256).Hash.ToLowerInvariant()
    return [pscustomobject]@{ TarballName = $tarballName; TarballPath = $tarballPath; Checksum = $checksum }
}

function Invoke-RealExtractionChecks {
    param([Parameter(Mandatory = $true)][pscustomobject] $BaseArtifact)

    $sevenZip = Get-Command (Join-Path $env:ProgramFiles "7-Zip\7z.exe") -ErrorAction SilentlyContinue
    if (-not $sevenZip) { $sevenZip = Get-Command 7z -ErrorAction SilentlyContinue }
    if (-not $sevenZip) { throw "actual 7-Zip is required for -ExtractionOnly" }

    # Load only the canonical extractor, never installer setup or PATH updates.
    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($installPath, [ref] $tokens, [ref] $parseErrors)
    if ($parseErrors.Count -ne 0) { throw "install.ps1 failed to parse" }
    $extractor = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq "Expand-Tarball"
    }, $true)
    if ($null -eq $extractor) { throw "install.ps1 has no Expand-Tarball function" }
    Invoke-Expression $extractor.Extent.Text
    function Write-Info { param([string] $Message) Write-Host $Message }
    function Write-ErrorExit { param([string] $Message) throw $Message }
    # Retain owned intermediates for diagnosis; cleanup is not qualified here.
    function Remove-Item {
        param([string] $LiteralPath, [string] $ErrorAction)
        Write-Host "Retained extraction intermediate: $LiteralPath"
    }
    function Write-ExtractionPass {
        param([string] $ScenarioId, [string] $BinaryHash = "", [string] $ObservedError = "")
        [ordered]@{
            schema = "ee.test_event.v1"
            kind = "windows_installer_real_extraction"
            scenario_id = $ScenarioId
            result = "pass"
            powershell_version = $PSVersionTable.PSVersion.ToString()
            script_hash = $scriptHash
            archive_sha256 = $BaseArtifact.Checksum
            payload_kind = "local_windows_executable_fixture"
            available_seven_zip = $sevenZip.Source
            binary_sha256 = $BinaryHash
            observed_error = $ObservedError
            cleanup_qualified = $false
            installer_setup_invoked = $false
        } | ConvertTo-Json -Compress | Add-Content -Path $LogPath -Encoding utf8
    }

    $savedPath = $env:PATH
    try {
        $env:PATH = Split-Path -Parent $sevenZip.Source
        if (Get-Command tar, xz -ErrorAction SilentlyContinue) {
            throw "7-Zip fallback isolation failed: tar or xz is still available"
        }
        $expectedHash = (Get-FileHash -LiteralPath (Join-Path $ArtifactRoot "base\payload\ee.exe") -Algorithm SHA256).Hash
        $validRoot = Join-Path $ArtifactRoot "real-extraction-valid"
        New-Item -ItemType Directory -Path $validRoot | Out-Null
        $validArchive = Join-Path $validRoot $BaseArtifact.TarballName
        Copy-Item -LiteralPath $BaseArtifact.TarballPath -Destination $validArchive
        $validDest = Join-Path $validRoot "extracted"
        Expand-Tarball -TarballPath $validArchive -DestDir $validDest
        $actualHash = (Get-FileHash -LiteralPath (Join-Path $validDest "ee.exe") -Algorithm SHA256).Hash
        if ($actualHash -ne $expectedHash) { throw "7-Zip extraction changed the actual payload bytes" }
        $tarName = [System.IO.Path]::GetFileNameWithoutExtension($BaseArtifact.TarballName)
        $validTar = Join-Path $validRoot $tarName
        if (-not (Test-Path -LiteralPath $validTar -PathType Leaf)) {
            throw "7-Zip did not produce the TAR named after the downloaded target archive"
        }
        Write-ExtractionPass -ScenarioId "seven_zip_actual_binary_bytes" -BinaryHash $actualHash.ToLowerInvariant()

        # A real, valid XZ stream whose payload is invalid TAR must fail stage two.
        $badTarRoot = Join-Path $ArtifactRoot "real-extraction-invalid-tar"
        New-Item -ItemType Directory -Path $badTarRoot | Out-Null
        $badTar = Join-Path $badTarRoot $tarName
        [System.IO.File]::WriteAllBytes($badTar, [byte[]] @(0, 255, 17, 128, 42, 99, 0, 1))
        $badTarArchive = Join-Path $badTarRoot $BaseArtifact.TarballName
        & $sevenZip.Source a -txz $badTarArchive $badTar | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "7-Zip failed to create the invalid-TAR control" }
        & $sevenZip.Source t $badTarArchive | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "invalid-TAR control must contain a valid XZ stream" }
        $badTarDest = Join-Path $badTarRoot "extracted"
        $observedError = ""
        try { Expand-Tarball -TarballPath $badTarArchive -DestDir $badTarDest }
        catch { $observedError = $_.Exception.Message }
        if ($observedError -notlike "xz decompression unavailable.*" -or
            (Test-Path -LiteralPath (Join-Path $badTarDest "ee.exe"))) {
            throw "invalid TAR was not rejected by the canonical extraction failure path: $observedError"
        }
        Write-ExtractionPass -ScenarioId "seven_zip_invalid_tar_rejected" -ObservedError $observedError

        # A stale valid TAR cannot turn a failed first-stage XZ extraction into success.
        $badXzRoot = Join-Path $ArtifactRoot "real-extraction-invalid-xz"
        New-Item -ItemType Directory -Path $badXzRoot | Out-Null
        $badXz = Join-Path $badXzRoot $BaseArtifact.TarballName
        [System.IO.File]::WriteAllBytes($badXz, [byte[]] @(0, 255, 17, 128, 42, 99, 0, 1))
        Copy-Item -LiteralPath $validTar -Destination (Join-Path $badXzRoot $tarName)
        $badXzDest = Join-Path $badXzRoot "extracted"
        $observedError = ""
        try { Expand-Tarball -TarballPath $badXz -DestDir $badXzDest }
        catch { $observedError = $_.Exception.Message }
        if ($observedError -notlike "xz decompression unavailable.*" -or
            (Test-Path -LiteralPath (Join-Path $badXzDest "ee.exe"))) {
            throw "invalid XZ with stale TAR was not rejected by the canonical extraction failure path: $observedError"
        }
        Write-ExtractionPass -ScenarioId "seven_zip_invalid_xz_with_stale_tar_rejected" -ObservedError $observedError

        # Exercise the real default tool order too, including GNU tar on hosts
        # where it precedes Windows bsdtar. Its errors must allow fallback.
        $env:PATH = $originalProcessEnv["PATH"]
        Get-Command tar, xz, 7z -ErrorAction SilentlyContinue | Format-Table Name, Source | Out-Host
        $defaultRoot = Join-Path $ArtifactRoot "real-extraction-default-path"
        New-Item -ItemType Directory -Path $defaultRoot | Out-Null
        $defaultArchive = Join-Path $defaultRoot $BaseArtifact.TarballName
        Copy-Item -LiteralPath $BaseArtifact.TarballPath -Destination $defaultArchive
        $defaultDest = Join-Path $defaultRoot "extracted"
        Expand-Tarball -TarballPath $defaultArchive -DestDir $defaultDest
        $defaultHash = (Get-FileHash -LiteralPath (Join-Path $defaultDest "ee.exe") -Algorithm SHA256).Hash
        if ($defaultHash -ne $expectedHash) { throw "default-PATH extraction changed the actual payload bytes" }
        Write-ExtractionPass -ScenarioId "default_path_actual_binary_bytes" -BinaryHash $defaultHash.ToLowerInvariant()
    } finally {
        $env:PATH = $savedPath
    }
    Write-Host "Windows installer actual extraction checks passed (4 scenarios). Log: $LogPath"
}

function New-FakeCosign {
    param([Parameter(Mandatory = $true)][string] $ModuleRoot)

    $moduleDir = Join-Path $ModuleRoot "cosign"
    New-Item -ItemType Directory -Force -Path $moduleDir | Out-Null
    $modulePath = Join-Path $moduleDir "cosign.psm1"
    $manifestPath = Join-Path $moduleDir "cosign.psd1"
    $moduleContent = @'
function cosign {
    param(
        [Parameter(ValueFromRemainingArguments = $true)]
        [string[]] $CosignArgs
    )

    $logPath = [Environment]::GetEnvironmentVariable("EE_MOCK_COSIGN_LOG", "Process")
    if (-not [string]::IsNullOrWhiteSpace($logPath)) {
        [System.IO.File]::AppendAllText($logPath, (($CosignArgs -join " ") + [Environment]::NewLine), [System.Text.Encoding]::UTF8)
    }
    $global:LASTEXITCODE = 0
}

Export-ModuleMember -Function cosign
'@
    $manifestContent = @"
@{
    RootModule = 'cosign.psm1'
    ModuleVersion = '0.0.0'
    GUID = '63d89d52-94e8-474a-9c0f-cdce597c2f23'
    FunctionsToExport = @('cosign')
    CmdletsToExport = @()
    VariablesToExport = @()
    AliasesToExport = @()
}
"@
    [System.IO.File]::WriteAllText($modulePath, $moduleContent, [System.Text.Encoding]::UTF8)
    [System.IO.File]::WriteAllText($manifestPath, $manifestContent, [System.Text.Encoding]::UTF8)
}

function New-ScenarioFiles {
    param(
        [Parameter(Mandatory = $true)][pscustomobject] $BaseArtifact,
        [Parameter(Mandatory = $true)][string] $ScenarioId,
        [Parameter(Mandatory = $true)][bool] $BundleAvailable
    )

    $root = Join-Path $ArtifactRoot $ScenarioId
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    Copy-Item $BaseArtifact.TarballPath (Join-Path $root $BaseArtifact.TarballName) -Force
    if ($BundleAvailable) {
        [System.IO.File]::WriteAllText((Join-Path $root "$($BaseArtifact.TarballName).sigstore.json"), '{"mock":"sigstore-bundle"}', [System.Text.Encoding]::UTF8)
    }
    return $root
}

function Invoke-MockedScenario {
    param(
        [Parameter(Mandatory = $true)][pscustomobject] $Scenario,
        [Parameter(Mandatory = $true)][pscustomobject] $BaseArtifact
    )

    $scenarioRoot = New-ScenarioFiles -BaseArtifact $BaseArtifact -ScenarioId $Scenario.Id -BundleAvailable $Scenario.BundleAvailable
    $server = $null
    try {
        $server = Start-FixtureServer -Root $scenarioRoot
        $installRoot = Join-Path $scenarioRoot "install-root"
        $appData = Join-Path $scenarioRoot "appdata"
        $localAppData = Join-Path $scenarioRoot "localappdata"
        $userProfile = Join-Path $scenarioRoot "userprofile"
        $tempRoot = Join-Path $scenarioRoot "tmp"
        New-Item -ItemType Directory -Force -Path $installRoot, $appData, $localAppData, $userProfile, $tempRoot | Out-Null
        if ($Scenario.AgentMarker) {
            New-Item -ItemType Directory -Force -Path (Join-Path $userProfile ".codex") | Out-Null
        }

        $mockBin = Join-Path $scenarioRoot "mock-bin"
        $cosignLog = Join-Path $scenarioRoot "cosign-invocations.txt"
        if ($Scenario.FakeCosign) {
            New-FakeCosign -ModuleRoot $mockBin
        } else {
            New-Item -ItemType Directory -Force -Path $mockBin | Out-Null
        }

        $windowsPath = @(
            (Join-Path $env:WINDIR "System32"),
            $env:WINDIR,
            (Join-Path $env:WINDIR "System32\WindowsPowerShell\v1.0"),
            $PSHOME
        ) -join ";"
        $effectivePath = if ($Scenario.FakeCosign) { "$mockBin;$windowsPath" } else { $windowsPath }

        Set-ProcessEnv -Name "APPDATA" -Value $appData
        Set-ProcessEnv -Name "LOCALAPPDATA" -Value $localAppData
        Set-ProcessEnv -Name "TEMP" -Value $tempRoot
        Set-ProcessEnv -Name "TMP" -Value $tempRoot
        Set-ProcessEnv -Name "USERPROFILE" -Value $userProfile
        Set-ProcessEnv -Name "PATH" -Value $effectivePath
        if ($Scenario.FakeCosign) {
            $baseModulePath = [Environment]::GetEnvironmentVariable("PSModulePath", "Process")
            if ([string]::IsNullOrWhiteSpace($baseModulePath)) {
                Set-ProcessEnv -Name "PSModulePath" -Value $mockBin
            } else {
                Set-ProcessEnv -Name "PSModulePath" -Value "$mockBin;$baseModulePath"
            }
        } else {
            Set-ProcessEnv -Name "PSModulePath" -Value $null
        }
        Set-ProcessEnv -Name "NO_COLOR" -Value "1"
        Set-ProcessEnv -Name "EE_MOCK_COSIGN_LOG" -Value $cosignLog
        if ($Scenario.EnvRequireProvenance) {
            Set-ProcessEnv -Name "EE_REQUIRE_PROVENANCE" -Value "1"
        } else {
            Set-ProcessEnv -Name "EE_REQUIRE_PROVENANCE" -Value $null
        }
        Set-ProcessEnv -Name "EE_SKIP_VERIFY" -Value $null
        [Environment]::SetEnvironmentVariable("PATH", $originalUserPath, "User")

        $artifactUrl = "$($server.BaseUrl)/$($BaseArtifact.TarballName)"
        $childArgs = @(
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-File", $installPath,
            "-Version", "v0.1.0",
            "-ArtifactUrl", $artifactUrl,
            "-InstallDir", $installRoot,
            "-Checksum", $BaseArtifact.Checksum,
            "-Force",
            "-Offline"
        )
        if ($Scenario.NoVerify) { $childArgs += "-NoVerify" }
        if ($Scenario.RequireProvenanceSwitch) { $childArgs += "-RequireProvenance" }
        if ($Scenario.NoConfigure) { $childArgs += "-NoConfigure" }

        $powerShellExe = (Get-Process -Id $PID).Path
        $outputObjects = & $powerShellExe @childArgs *>&1
        $exitCode = $LASTEXITCODE
        $outputText = ($outputObjects | Out-String)
        $stdoutPath = Join-Path $scenarioRoot "installer-output.txt"
        [System.IO.File]::WriteAllText($stdoutPath, $outputText, [System.Text.Encoding]::UTF8)

        $diagnostics = New-Object "System.Collections.Generic.List[string]"
        if ($exitCode -ne $Scenario.ExpectedExitCode) {
            $diagnostics.Add("expected exit $($Scenario.ExpectedExitCode), got $exitCode") | Out-Null
        }
        foreach ($marker in $Scenario.RequiredOutput) {
            if (-not (Text-Contains -Haystack $outputText -Needle $marker)) {
                $diagnostics.Add("missing output marker: $marker") | Out-Null
            }
        }
        foreach ($marker in $Scenario.ForbiddenOutput) {
            if (Text-Contains -Haystack $outputText -Needle $marker) {
                $diagnostics.Add("forbidden output marker present: $marker") | Out-Null
            }
        }
        $actualCosign = (Test-Path $cosignLog) -and ((Get-Content -Raw -Path $cosignLog).Trim().Length -gt 0)
        if ($actualCosign -ne $Scenario.ExpectCosignInvocation) {
            $diagnostics.Add("expected cosign_invoked=$($Scenario.ExpectCosignInvocation), got $actualCosign") | Out-Null
        }
        $result = if ($diagnostics.Count -eq 0) { "pass" } else { "fail" }
        $diagnosis = $diagnostics -join "; "
        $mockedInputs = @{
            fake_cosign = [bool] $Scenario.FakeCosign
            sigstore_bundle = [bool] $Scenario.BundleAvailable
            no_verify = [bool] $Scenario.NoVerify
            require_provenance_switch = [bool] $Scenario.RequireProvenanceSwitch
            require_provenance_env = [bool] $Scenario.EnvRequireProvenance
            agent_marker = [bool] $Scenario.AgentMarker
            external_network = $false
            local_rust_compile = $false
        }
        Write-FlowEvent `
            -ScenarioId $Scenario.Id `
            -Result $result `
            -ExpectedExitCode $Scenario.ExpectedExitCode `
            -ActualExitCode $exitCode `
            -MockedInputs $mockedInputs `
            -RequiredOutput $Scenario.RequiredOutput `
            -ForbiddenOutput $Scenario.ForbiddenOutput `
            -ExpectedCosignInvocation $Scenario.ExpectCosignInvocation `
            -ActualCosignInvocation $actualCosign `
            -StdoutArtifact ([System.IO.Path]::GetFileName($stdoutPath)) `
            -FirstFailureDiagnosis $diagnosis
    } finally {
        if ($server -and $server.Process -and -not $server.Process.HasExited) {
            Stop-Process -Id $server.Process.Id -Force -ErrorAction SilentlyContinue
        }
        Restore-Environment
    }
}

if (-not (Test-Path $installPath)) {
    throw "install.ps1 is missing from $RepoRoot"
}

if ($ExtractionOnly) {
    $savedPath = $env:PATH
    try {
        # Prefer Windows bsdtar for fixture creation; GNU tar treats C: as a host.
        $env:PATH = "$(Join-Path $env:WINDIR 'System32');$savedPath"
        $baseArtifact = New-BaseArtifact
        Invoke-RealExtractionChecks -BaseArtifact $baseArtifact
    } finally {
        $env:PATH = $savedPath
    }
    exit 0
}

$baseArtifact = New-BaseArtifact
$scenarios = @(
    [pscustomobject]@{
        Id = "default_sha256_no_cosign_warns"
        FakeCosign = $false
        BundleAvailable = $false
        NoVerify = $false
        RequireProvenanceSwitch = $false
        EnvRequireProvenance = $false
        AgentMarker = $false
        NoConfigure = $true
        ExpectedExitCode = 0
        ExpectCosignInvocation = $false
        RequiredOutput = @("Checksum verified", "cosign not found; skipping Sigstore signature verification", "Installed to")
        ForbiddenOutput = @("Sigstore signature verified", "Verification skipped")
    },
    [pscustomobject]@{
        Id = "default_cosign_bundle_missing_warns"
        FakeCosign = $true
        BundleAvailable = $false
        NoVerify = $false
        RequireProvenanceSwitch = $false
        EnvRequireProvenance = $false
        AgentMarker = $false
        NoConfigure = $true
        ExpectedExitCode = 0
        ExpectCosignInvocation = $false
        RequiredOutput = @("Checksum verified", "Sigstore bundle unavailable", "skipping signature verification", "Installed to")
        ForbiddenOutput = @("Sigstore signature verified")
    },
    [pscustomobject]@{
        Id = "require_provenance_missing_cosign_fails"
        FakeCosign = $false
        BundleAvailable = $false
        NoVerify = $false
        RequireProvenanceSwitch = $true
        EnvRequireProvenance = $false
        AgentMarker = $false
        NoConfigure = $true
        ExpectedExitCode = 1
        ExpectCosignInvocation = $false
        RequiredOutput = @("cosign not found but -RequireProvenance was set")
        ForbiddenOutput = @("Installed to")
    },
    [pscustomobject]@{
        Id = "env_require_provenance_missing_bundle_fails"
        FakeCosign = $true
        BundleAvailable = $false
        NoVerify = $false
        RequireProvenanceSwitch = $false
        EnvRequireProvenance = $true
        AgentMarker = $false
        NoConfigure = $true
        ExpectedExitCode = 1
        ExpectCosignInvocation = $false
        RequiredOutput = @("Sigstore bundle unavailable", "required by -RequireProvenance")
        ForbiddenOutput = @("Installed to")
    },
    [pscustomobject]@{
        Id = "sigstore_success_fake_cosign"
        FakeCosign = $true
        BundleAvailable = $true
        NoVerify = $false
        RequireProvenanceSwitch = $true
        EnvRequireProvenance = $false
        AgentMarker = $false
        NoConfigure = $true
        ExpectedExitCode = 0
        ExpectCosignInvocation = $true
        RequiredOutput = @("Checksum verified", "Sigstore signature verified", "Installed to")
        ForbiddenOutput = @("Sigstore bundle unavailable")
    },
    [pscustomobject]@{
        Id = "no_verify_skips_checksum_and_sigstore"
        FakeCosign = $true
        BundleAvailable = $false
        NoVerify = $true
        RequireProvenanceSwitch = $false
        EnvRequireProvenance = $false
        AgentMarker = $false
        NoConfigure = $true
        ExpectedExitCode = 0
        ExpectCosignInvocation = $false
        RequiredOutput = @("Verification skipped (-NoVerify / EE_SKIP_VERIFY=1)", "Installed to")
        ForbiddenOutput = @("Checksum verified", "Sigstore signature verified", "Sigstore bundle unavailable")
    },
    [pscustomobject]@{
        Id = "agent_integration_codex_no_other_matches_strict"
        FakeCosign = $false
        BundleAvailable = $false
        NoVerify = $false
        RequireProvenanceSwitch = $false
        EnvRequireProvenance = $false
        AgentMarker = $true
        NoConfigure = $false
        ExpectedExitCode = 0
        ExpectCosignInvocation = $false
        RequiredOutput = @("Agent integration", "Codex CLI", "Installed to")
        ForbiddenOutput = @("Installation failed")
    }
)

try {
    foreach ($scenario in $scenarios) {
        Invoke-MockedScenario -Scenario $scenario -BaseArtifact $baseArtifact
    }
} finally {
    Restore-Environment
}

if ($failures.Count -gt 0) {
    $message = "Windows installer mocked flow conformance failed: $($failures -join '; ')"
    Write-Error $message
    exit 1
}

Write-Host "Windows installer mocked flow conformance passed. Log: $LogPath"
