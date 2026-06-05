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
        -Assertion "ee_require_provenance_env_bridge" `
        -Condition ($installText -match '\$env:EE_REQUIRE_PROVENANCE\s+-eq\s+"1"') `
        -Diagnosis "install.ps1 must honor EE_REQUIRE_PROVENANCE=1 as the RequireProvenance policy bridge"

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
}

if ($failures.Count -gt 0) {
    $message = "Windows installer static conformance failed: $($failures -join '; ')"
    Write-Error $message
    exit 1
}

Write-Host "Windows installer static conformance passed. Log: $LogPath"
