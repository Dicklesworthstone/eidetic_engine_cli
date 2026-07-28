[CmdletBinding()]
param(
    [string] $RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path,
    [string] $LogPath = (Join-Path ([System.IO.Path]::GetTempPath()) "ee-windows-installer-static-conformance.jsonl"),
    [switch] $ResetLog
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$installPath = Join-Path $RepoRoot "install.ps1"
$readmePath = Join-Path $RepoRoot "README.md"
$releaseWorkflowPath = Join-Path $RepoRoot ".github/workflows/release.yml"
$conformancePath = Join-Path $RepoRoot "tests/CONFORMANCE.md"
$liveSmokePath = Join-Path $RepoRoot "scripts/windows-installer-live-smoke.ps1"

$logDir = Split-Path -Parent $LogPath
if (-not [string]::IsNullOrWhiteSpace($logDir)) {
    New-Item -ItemType Directory -Force -Path $logDir | Out-Null
}
if ($ResetLog) {
    [System.IO.File]::WriteAllText($LogPath, "", [System.Text.Encoding]::UTF8)
}

$scriptHash = ""
if (Test-Path $installPath) {
    $scriptHash = (Get-FileHash -Path $installPath -Algorithm SHA256).Hash.ToLowerInvariant()
}

$failures = New-Object "System.Collections.Generic.List[string]"

function Write-ConformanceEvent {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Assertion,
        [Parameter(Mandatory = $true)]
        [ValidateSet("pass", "fail")]
        [string] $Result,
        [string] $Diagnosis = ""
    )

    $event = [ordered]@{
        schema = "ee.test_event.v1"
        kind = "windows_installer_static_conformance"
        bead_id = "bd-3tprq.2"
        related_bead_ids = @("bd-3tprq.2", "bd-3tprq.4", "bd-3tprq.5", "bd-xww0x")
        surface = "install_ps1_parser_static"
        assertion = $Assertion
        result = $Result
        shell = $PSVersionTable.PSEdition
        powershell_version = $PSVersionTable.PSVersion.ToString()
        os = [System.Environment]::OSVersion.VersionString
        script_hash = $scriptHash
        first_failure_diagnosis = $Diagnosis
    }
    $event | ConvertTo-Json -Compress -Depth 5 | Add-Content -Path $LogPath -Encoding utf8
    if ($Result -eq "fail") {
        $failures.Add("${Assertion}: $Diagnosis") | Out-Null
    }
}

function Assert-True {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Assertion,
        [Parameter(Mandatory = $true)]
        [bool] $Condition,
        [Parameter(Mandatory = $true)]
        [string] $Diagnosis
    )

    if ($Condition) {
        Write-ConformanceEvent -Assertion $Assertion -Result "pass"
    } else {
        Write-ConformanceEvent -Assertion $Assertion -Result "fail" -Diagnosis $Diagnosis
    }
}

Assert-True `
    -Assertion "install_script_exists" `
    -Condition (Test-Path $installPath) `
    -Diagnosis "install.ps1 is missing from the repository root"
Assert-True `
    -Assertion "readme_exists" `
    -Condition (Test-Path $readmePath) `
    -Diagnosis "README.md is missing from the repository root"
Assert-True `
    -Assertion "release_workflow_exists" `
    -Condition (Test-Path $releaseWorkflowPath) `
    -Diagnosis ".github/workflows/release.yml is missing from the repository"
Assert-True `
    -Assertion "conformance_matrix_exists" `
    -Condition (Test-Path $conformancePath) `
    -Diagnosis "tests/CONFORMANCE.md is missing from the repository"
Assert-True `
    -Assertion "windows_live_smoke_script_exists" `
    -Condition (Test-Path $liveSmokePath) `
    -Diagnosis "scripts/windows-installer-live-smoke.ps1 is missing from the repository"

if (Test-Path $liveSmokePath) {
    $liveSmokeTokens = $null
    $liveSmokeParseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseFile($liveSmokePath, [ref] $liveSmokeTokens, [ref] $liveSmokeParseErrors) | Out-Null
    $liveSmokeParseErrorMessages = @($liveSmokeParseErrors | ForEach-Object { $_.Message }) -join "; "
    Assert-True `
        -Assertion "windows_live_smoke_parser_clean" `
        -Condition (@($liveSmokeParseErrors).Count -eq 0) `
        -Diagnosis "PowerShell parser reported errors in windows-installer-live-smoke.ps1: $liveSmokeParseErrorMessages"

    $liveSmokeText = Get-Content -Raw -Path $liveSmokePath
    Assert-True `
        -Assertion "windows_live_smoke_downloads_release_installer_to_file" `
        -Condition (
            $liveSmokeText -match 'Invoke-WebRequest' -and
            $liveSmokeText -match '-OutFile' -and
            $liveSmokeText -match 'releases/download/\$ResolvedTag/install\.ps1'
        ) `
        -Diagnosis "windows-installer-live-smoke.ps1 must download the release install.ps1 asset with Invoke-WebRequest -OutFile"
    Assert-True `
        -Assertion "windows_live_smoke_uses_isolated_install_dir" `
        -Condition (
            $liveSmokeText -match '-InstallDir' -and
            $liveSmokeText -match '\$InstallRoot'
        ) `
        -Diagnosis "windows-installer-live-smoke.ps1 must pass an explicit runner-temp InstallDir to install.ps1"
    Assert-True `
        -Assertion "windows_live_smoke_blocks_source_fallback_compile" `
        -Condition (
            $liveSmokeText -match 'Remove-RustFromProcessPath' -and
            $liveSmokeText -match 'cargo_path_suppressed'
        ) `
        -Diagnosis "windows-installer-live-smoke.ps1 must suppress cargo/rustup PATH entries so release download failures cannot compile Rust locally"
    Assert-True `
        -Assertion "windows_live_smoke_logs_test_events" `
        -Condition (
            $liveSmokeText -match 'ee\.test_event\.v1' -and
            $liveSmokeText -match 'first_failure_diagnosis'
        ) `
        -Diagnosis "windows-installer-live-smoke.ps1 must emit ee.test_event.v1 records with first_failure_diagnosis"
    Assert-True `
        -Assertion "windows_live_smoke_pins_semantic_first_use_evidence" `
        -Condition (
            $liveSmokeText -match 'EE_INSTALL_SEMANTIC_SMOKE\s*=\s*"require"' -and
            $liveSmokeText -match 'semantic_first_use_init' -and
            $liveSmokeText -match 'semantic_first_use_remember' -and
            $liveSmokeText -match 'semantic_first_use_rebuild' -and
            $liveSmokeText -match 'semantic_model_status' -and
            $liveSmokeText -match 'selected_model_id'
        ) `
        -Diagnosis "windows-installer-live-smoke.ps1 must set EE_INSTALL_SEMANTIC_SMOKE=require and log first-use semantic phases with selected_model_id evidence"
}

if (Test-Path $installPath) {
    $bytes = [System.IO.File]::ReadAllBytes($installPath)
    $hasUtf8Bom = $bytes.Length -ge 3 -and $bytes[0] -eq 0xef -and $bytes[1] -eq 0xbb -and $bytes[2] -eq 0xbf
    Assert-True `
        -Assertion "install_ps1_utf8_bom" `
        -Condition $hasUtf8Bom `
        -Diagnosis "install.ps1 must start with UTF-8 BOM bytes ef bb bf so Windows PowerShell 5.1 reads non-ASCII text as UTF-8"

    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($installPath, [ref] $tokens, [ref] $parseErrors)
    $parseErrorMessages = @($parseErrors | ForEach-Object { $_.Message }) -join "; "
    Assert-True `
        -Assertion "install_ps1_parser_clean" `
        -Condition (@($parseErrors).Count -eq 0) `
        -Diagnosis "PowerShell parser reported errors: $parseErrorMessages"

    $installText = Get-Content -Raw -Path $installPath
    $installLines = @($installText -split "`r?`n")
    $paramNames = @()
    if ($null -ne $ast.ParamBlock) {
        $paramNames = @($ast.ParamBlock.Parameters | ForEach-Object { $_.Name.VariablePath.UserPath })
    }
    Assert-True `
        -Assertion "require_provenance_parameter_declared" `
        -Condition ($paramNames -contains "RequireProvenance") `
        -Diagnosis "install.ps1 param block must declare a RequireProvenance switch"
    Assert-True `
        -Assertion "no_verify_parameter_declared" `
        -Condition ($paramNames -contains "NoVerify") `
        -Diagnosis "install.ps1 param block must declare a NoVerify switch"
    Assert-True `
        -Assertion "ee_require_provenance_env_bridge" `
        -Condition ($installText -match '\$env:EE_REQUIRE_PROVENANCE\s+-eq\s+"1"') `
        -Diagnosis "install.ps1 must honor EE_REQUIRE_PROVENANCE=1 as the RequireProvenance policy bridge"
    Assert-True `
        -Assertion "ee_skip_verify_env_bridge" `
        -Condition ($installText -match '\$env:EE_SKIP_VERIFY\s+-eq\s+"1"') `
        -Diagnosis "install.ps1 must honor EE_SKIP_VERIFY=1 as the NoVerify policy bridge"
    Assert-True `
        -Assertion "install_help_pins_noverify_scope" `
        -Condition (
            $installText -match 'Skip SHA256 \+ Sigstore verification' -and
            $installText -match '-NoVerify / EE_SKIP_VERIFY=1'
        ) `
        -Diagnosis "install.ps1 help must state that -NoVerify / EE_SKIP_VERIFY=1 skips both SHA256 and Sigstore verification"
    Assert-True `
        -Assertion "install_help_pins_provenance_default" `
        -Condition (
            $installText -match 'Sigstore\s+signature verification is opt-in via -RequireProvenance\s+/\s+EE_REQUIRE_PROVENANCE=1' -and
            $installText -match 'missing bundle or cosign is a warning'
        ) `
        -Diagnosis "install.ps1 help must state that Sigstore is opt-in by default and missing bundle/cosign only warns unless provenance is required"

    $badInstallExampleLines = @($installLines | Where-Object {
        $_ -match '(?i)\b(Invoke-WebRequest|Invoke-RestMethod|iwr|irm)\b' -and
        $_ -match '(?i)install\.ps1' -and
        $_ -match '(?i)(\.Content|\|\s*(iex|Invoke-Expression)\b)' -and
        $_ -notmatch '(?i)do not|does not work|not a string'
    })
    Assert-True `
        -Assertion "install_examples_avoid_content_iex" `
        -Condition ($badInstallExampleLines.Count -eq 0) `
        -Diagnosis "installer examples must not execute release-asset .Content or pipe install.ps1 into iex: $($badInstallExampleLines -join ' | ')"

    $installOutFileExamples = @($installLines | Where-Object {
        $_ -match '(?i)\b(Invoke-WebRequest|iwr)\b' -and
        $_ -match '(?i)install\.ps1' -and
        $_ -match '(?i)-OutFile\b'
    })
    Assert-True `
        -Assertion "install_examples_use_outfile" `
        -Condition ($installOutFileExamples.Count -gt 0) `
        -Diagnosis "install.ps1 examples must download install.ps1 to a file with -OutFile"

    $showAgentIntegration = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Show-AgentIntegration"
    }, $true)
    $functionText = ""
    if ($null -ne $showAgentIntegration) {
        $functionText = $showAgentIntegration.Extent.Text
    }
    $arrayAssignmentIndex = $functionText.IndexOf('$other = @(')
    $countReadIndex = $functionText.IndexOf('$other.Count')
    Assert-True `
        -Assertion "show_agent_integration_function_present" `
        -Condition ($null -ne $showAgentIntegration) `
        -Diagnosis "install.ps1 must define Show-AgentIntegration"
    Assert-True `
        -Assertion "show_agent_integration_array_wraps_optional_agents" `
        -Condition ($arrayAssignmentIndex -ge 0 -and $countReadIndex -gt $arrayAssignmentIndex -and $functionText -match '\$other\s*=\s*@\(\s*@\(') `
        -Diagnosis "Show-AgentIntegration must wrap optional-agent Where-Object results in @() before reading .Count under Set-StrictMode -Version Latest"
    Assert-True `
        -Assertion "installer_guidance_uses_canonical_pack_surface" `
        -Condition ($installText -match 'ee pack' -and $installText -notmatch 'ee context') `
        -Diagnosis "install.ps1 must introduce new users to canonical ee pack, not the soft-deprecated ee context alias"

    $testInstalledVersion = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Test-InstalledVersion"
    }, $true)
    $testInstalledVersionText = if ($null -eq $testInstalledVersion) { "" } else { $testInstalledVersion.Extent.Text }
    Assert-True `
        -Assertion "installed_version_requires_successful_native_exit" `
        -Condition (
            $testInstalledVersionText -match '\$versionExitCode\s*=\s*\$LASTEXITCODE' -and
            $testInstalledVersionText -match 'if\s*\(\$versionExitCode\s+-ne\s+0\)\s*\{\s*return\s+\$false'
        ) `
        -Diagnosis "Test-InstalledVersion must reject version-looking output from an ee.exe process that exited nonzero"

    $installCompletions = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Install-Completions"
    }, $true)
    $installCompletionsText = if ($null -eq $installCompletions) { "" } else { $installCompletions.Extent.Text }
    Assert-True `
        -Assertion "completion_generation_checks_native_exit" `
        -Condition (
            $installCompletionsText -match '\$completionExitCode\s*=\s*\$LASTEXITCODE' -and
            $installCompletionsText -match 'Failed to write PowerShell completions \(exit code \$completionExitCode\)'
        ) `
        -Diagnosis "Install-Completions must not report success when native completion generation exits nonzero"

    $selfTest = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Invoke-SelfTest"
    }, $true)
    $selfTestText = if ($null -eq $selfTest) { "" } else { $selfTest.Extent.Text }
    Assert-True `
        -Assertion "self_test_version_failure_is_fatal" `
        -Condition (
            $selfTestText -match '\$versionExitCode\s*=\s*\$LASTEXITCODE' -and
            $selfTestText -match 'ee --version failed with exit code' -and
            $selfTestText -match 'Write-ErrorExit'
        ) `
        -Diagnosis "Invoke-SelfTest must fail the installer when ee.exe --version exits nonzero"
    Assert-True `
        -Assertion "self_test_doctor_degradation_remains_advisory" `
        -Condition (
            $selfTestText -match 'doctor --json' -and
            $selfTestText -match 'Write-Warning2 "ee doctor reported issues' -and
            $selfTestText -notmatch 'Write-ErrorExit "ee doctor'
        ) `
        -Diagnosis "Invoke-SelfTest must warn, not fail, when ee doctor reports a degraded first-run posture"

    $mainFunction = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq "Main"
    }, $true)
    $mainFunctionText = if ($null -eq $mainFunction) { "" } else { $mainFunction.Extent.Text }
    $shortCircuitStart = $mainFunctionText.IndexOf("# Already-installed short-circuit")
    $shortCircuitEnd = $mainFunctionText.IndexOf("# Lock.", [Math]::Max(0, $shortCircuitStart))
    $pathRepairIndex = $mainFunctionText.IndexOf('Update-UserPath -Dir $InstallDir', [Math]::Max(0, $shortCircuitStart))
    $completionIndex = $mainFunctionText.IndexOf('Install-Completions -BinaryPath $binaryPath', [Math]::Max(0, $shortCircuitStart))
    $verifyIndex = $mainFunctionText.IndexOf('if ($Verify) { Invoke-SelfTest -BinaryPath $binaryPath }', [Math]::Max(0, $shortCircuitStart))
    Assert-True `
        -Assertion "matching_version_short_circuit_repairs_integration_and_verifies" `
        -Condition (
            $shortCircuitStart -ge 0 -and
            $shortCircuitEnd -gt $shortCircuitStart -and
            $pathRepairIndex -gt $shortCircuitStart -and
            $completionIndex -gt $pathRepairIndex -and
            $verifyIndex -gt $completionIndex -and
            $verifyIndex -lt $shortCircuitEnd
        ) `
        -Diagnosis "The matching-version branch must repair user PATH, regenerate completions, and honor -Verify before the installer lock"

    Assert-True `
        -Assertion "install_ps1_semantic_smoke_warn_require_contract" `
        -Condition (
            $installText -match 'function Get-InstallSemanticSmokeMode' -and
            $installText -match 'function Test-InstallSemanticSmokeRequired' -and
            $installText -match 'function Complete-SemanticSmokeFailure' -and
            $installText -match 'Write-ErrorExit \$Message' -and
            $installText -match 'Write-Warning2 \$Message' -and
            $installText -match 'Semantic first-use smoke did not reach semanticReadiness\.state=available mode=semantic'
        ) `
        -Diagnosis "install.ps1 must keep EE_INSTALL_SEMANTIC_SMOKE warn/require semantics and fail only when required"
}

if (Test-Path $readmePath) {
    $readmeText = Get-Content -Raw -Path $readmePath
    $readmeLines = @($readmeText -split "`r?`n")
    $badReadmeExampleLines = @($readmeLines | Where-Object {
        $_ -match '(?i)\b(Invoke-WebRequest|Invoke-RestMethod|iwr|irm)\b' -and
        $_ -match '(?i)install\.ps1' -and
        $_ -match '(?i)(\.Content|\|\s*(iex|Invoke-Expression)\b)' -and
        $_ -notmatch '(?i)do not|does not work|not a string'
    })
    Assert-True `
        -Assertion "readme_examples_avoid_content_iex" `
        -Condition ($badReadmeExampleLines.Count -eq 0) `
        -Diagnosis "README Windows installer examples must not execute release-asset .Content or pipe install.ps1 into iex: $($badReadmeExampleLines -join ' | ')"

    $readmeOutFileExamples = @($readmeLines | Where-Object {
        $_ -match '(?i)\b(Invoke-WebRequest|iwr)\b' -and
        $_ -match '(?i)install\.ps1' -and
        $_ -match '(?i)-OutFile\b'
    })
    Assert-True `
        -Assertion "readme_examples_use_outfile" `
        -Condition ($readmeOutFileExamples.Count -gt 0) `
        -Diagnosis "README Windows installer example must download install.ps1 to a file with -OutFile"
    Assert-True `
        -Assertion "readme_pins_provenance_enforcement_paths" `
        -Condition (
            $readmeText -match '-RequireProvenance' -and
            $readmeText -match 'EE_REQUIRE_PROVENANCE=1' -and
            $readmeText -match 'enforce Sigstore signature verification'
        ) `
        -Diagnosis "README Windows installer text must document both -RequireProvenance and EE_REQUIRE_PROVENANCE=1 as Sigstore enforcement paths"
}

if (Test-Path $releaseWorkflowPath) {
    $releaseWorkflowText = Get-Content -Raw -Path $releaseWorkflowPath
    $releaseWorkflowLines = @($releaseWorkflowText -split "`r?`n")
    $badReleaseWorkflowExampleLines = @($releaseWorkflowLines | Where-Object {
        $_ -match '(?i)\b(Invoke-WebRequest|Invoke-RestMethod|iwr|irm)\b' -and
        $_ -match '(?i)install\.ps1' -and
        $_ -match '(?i)(\.Content|\|\s*(iex|Invoke-Expression)\b|\[scriptblock\]::Create)' -and
        $_ -notmatch '(?i)do not|does not work|not a string'
    })
    Assert-True `
        -Assertion "release_notes_windows_examples_avoid_content_iex" `
        -Condition ($badReleaseWorkflowExampleLines.Count -eq 0) `
        -Diagnosis "release-note Windows installer examples must not execute release-asset .Content, scriptblock-created content, or pipe install.ps1 into iex: $($badReleaseWorkflowExampleLines -join ' | ')"

    $releaseWorkflowOutFileExamples = @($releaseWorkflowLines | Where-Object {
        $_ -match '(?i)\b(Invoke-WebRequest|iwr)\b' -and
        $_ -match '(?i)install\.ps1' -and
        $_ -match '(?i)-OutFile\b'
    })
    Assert-True `
        -Assertion "release_notes_windows_examples_use_outfile" `
        -Condition ($releaseWorkflowOutFileExamples.Count -gt 0) `
        -Diagnosis "release-note Windows installer example must download install.ps1 to a file with -OutFile"
}

if (Test-Path $conformancePath) {
    $conformanceText = Get-Content -Raw -Path $conformancePath
    foreach ($rowId in @("WIN-PS1-003", "WIN-PS1-004", "WIN-PS1-005", "WIN-PS1-006", "WIN-PS1-007", "WIN-PS1-008", "WIN-PS1-009", "WIN-PS1-010", "WIN-PS1-012", "WIN-PS1-013", "WIN-PS1-014", "WIN-PS1-015")) {
        Assert-True `
            -Assertion "conformance_matrix_includes_$rowId" `
            -Condition ($conformanceText -match [regex]::Escape($rowId)) `
            -Diagnosis "tests/CONFORMANCE.md must include matrix row $rowId for Windows installer drift accounting"
    }
    Assert-True `
        -Assertion "conformance_matrix_links_static_drift_guard" `
        -Condition (
            $conformanceText -match 'scripts/windows-installer-static-check\.ps1' -and
            $conformanceText -match 'bd-3tprq\.5'
        ) `
        -Diagnosis "tests/CONFORMANCE.md must link bd-3tprq.5 to scripts/windows-installer-static-check.ps1 so docs/help drift coverage is visible"
    Assert-True `
        -Assertion "conformance_matrix_pins_verification_vocabulary" `
        -Condition (
            $conformanceText -match '-NoVerify' -and
            $conformanceText -match 'EE_SKIP_VERIFY=1' -and
            $conformanceText -match '-RequireProvenance' -and
            $conformanceText -match 'EE_REQUIRE_PROVENANCE=1'
        ) `
        -Diagnosis "tests/CONFORMANCE.md must name -NoVerify, EE_SKIP_VERIFY=1, -RequireProvenance, and EE_REQUIRE_PROVENANCE=1"
    Assert-True `
        -Assertion "conformance_matrix_pins_semantic_smoke_vocabulary" `
        -Condition (
            $conformanceText -match 'EE_INSTALL_SEMANTIC_SMOKE=require' -and
            $conformanceText -match 'semanticReadiness\.state=available' -and
            $conformanceText -match 'semanticReadiness\.mode=semantic' -and
            $conformanceText -match 'semantic_model_status'
        ) `
        -Diagnosis "tests/CONFORMANCE.md must pin EE_INSTALL_SEMANTIC_SMOKE=require plus semanticReadiness state/mode and semantic_model_status evidence for bd-1et0v.24"
}

if ($failures.Count -gt 0) {
    $message = "Windows installer static conformance failed: $($failures -join '; ')"
    Write-Error $message
    exit 1
}

Write-Host "Windows installer static conformance passed. Log: $LogPath"
