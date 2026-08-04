<#
.SYNOPSIS
Materialize ee's sibling path dependencies at their locked revisions.

.DESCRIPTION
Reads franken-stack.lock, checks out each official repository at its exact
commit, and refuses to overwrite an unrelated or dirty existing checkout.
#>

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$DestinationRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Invoke-Git {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    & git @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "git $($Arguments -join ' ') failed with exit code $LASTEXITCODE"
    }
}

function Invoke-GitCaptureResult {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    $savedNativePreference = $false
    $nativePreference = Get-Variable -Name PSNativeCommandUseErrorActionPreference `
        -ErrorAction SilentlyContinue
    if ($null -ne $nativePreference) {
        $savedNativePreference = $PSNativeCommandUseErrorActionPreference
        $PSNativeCommandUseErrorActionPreference = $false
    }
    try {
        $output = & git @Arguments 2>$null
        $exitCode = $LASTEXITCODE
    } finally {
        if ($null -ne $nativePreference) {
            $PSNativeCommandUseErrorActionPreference = $savedNativePreference
        }
    }

    return [pscustomobject]@{
        Success = ($exitCode -eq 0)
        Output = (($output | Out-String).Trim())
    }
}

function Test-CleanCheckout {
    param(
        [string]$Repository,
        [string]$Revision,
        [string]$Destination,
        [string]$ExpectedUrl
    )

    $actualRevision = Invoke-GitCaptureResult -Arguments @(
        "-C", $Destination, "rev-parse", "HEAD"
    )
    if (-not $actualRevision.Success -or $actualRevision.Output -ne $Revision) {
        return $false
    }

    $actualUrl = Invoke-GitCaptureResult -Arguments @(
        "-C", $Destination, "remote", "get-url", "origin"
    )
    $acceptedUrls = @(
        $ExpectedUrl,
        $ExpectedUrl.Substring(0, $ExpectedUrl.Length - 4),
        "git@github.com:Dicklesworthstone/$Repository.git"
    )
    if (-not $actualUrl.Success -or $actualUrl.Output -notin $acceptedUrls) {
        return $false
    }

    $status = Invoke-GitCaptureResult -Arguments @(
        "-C", $Destination, "-c", "core.longpaths=true",
        "status", "--porcelain", "--untracked-files=normal"
    )
    return $status.Success -and [string]::IsNullOrEmpty($status.Output)
}

function Checkout-Repository {
    param(
        [string]$Repository,
        [string]$Revision,
        [string]$Root
    )

    $destination = Join-Path $Root $Repository
    $repositoryUrl = "https://github.com/Dicklesworthstone/$Repository.git"
    $marker = Join-Path $destination ".git\ee-franken-stack-managed"

    if (Test-Path -LiteralPath $destination) {
        $gitDirectory = Join-Path $destination ".git"
        if (-not (Test-Path -LiteralPath $gitDirectory -PathType Container)) {
            throw "$destination already exists and is not a regular Git checkout"
        }

        if (Test-CleanCheckout -Repository $Repository -Revision $Revision `
                -Destination $destination -ExpectedUrl $repositoryUrl) {
            Write-Host "franken-stack: reuse $Repository@$Revision"
            return
        }

        $markerValue = ""
        if (Test-Path -LiteralPath $marker -PathType Leaf) {
            $markerValue = [IO.File]::ReadAllText($marker).Trim()
        }
        if ($markerValue -ne "$Repository`t$Revision") {
            throw "$destination does not exactly match $Repository@$Revision; refusing to modify it"
        }
    } else {
        New-Item -ItemType Directory -Path $destination | Out-Null
        Invoke-Git -Arguments @("-C", $destination, "init", "-q")
        Invoke-Git -Arguments @("-C", $destination, "remote", "add", "origin", $repositoryUrl)
        Invoke-Git -Arguments @("-C", $destination, "config", "core.longpaths", "true")
        [IO.File]::WriteAllText($marker, "$Repository`t$Revision`n")
    }

    Invoke-Git -Arguments @("-C", $destination, "fetch", "--depth", "1", "origin", $Revision)
    Invoke-Git -Arguments @(
        "-C", $destination, "-c", "core.longpaths=true",
        "-c", "advice.detachedHead=false",
        "checkout", "--detach", "FETCH_HEAD"
    )

    $actualRevision = Invoke-GitCaptureResult -Arguments @(
        "-C", $destination, "rev-parse", "HEAD"
    )
    if (-not $actualRevision.Success -or $actualRevision.Output -ne $Revision) {
        throw "$Repository resolved to $($actualRevision.Output) instead of locked revision $Revision"
    }
    if (-not (Test-CleanCheckout -Repository $Repository -Revision $Revision `
            -Destination $destination -ExpectedUrl $repositoryUrl)) {
        throw "$Repository checkout is dirty or has unexpected provenance after checkout"
    }

    Write-Host "franken-stack: checked out $Repository@$Revision"
}

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw "git is required"
}

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$lockFile = Join-Path $repositoryRoot "franken-stack.lock"
if (-not (Test-Path -LiteralPath $lockFile -PathType Leaf)) {
    throw "missing lock file: $lockFile"
}

$resolvedRoot = [IO.Path]::GetFullPath($DestinationRoot)
$filesystemRoot = [IO.Path]::GetPathRoot($resolvedRoot)
if ($resolvedRoot.TrimEnd('\', '/') -eq $filesystemRoot.TrimEnd('\', '/')) {
    throw "refusing to populate the filesystem root"
}
New-Item -ItemType Directory -Force -Path $resolvedRoot | Out-Null

$knownRepositories = @(
    "asupersync",
    "franken_agent_detection",
    "franken_networkx",
    "frankensearch",
    "frankensqlite",
    "sqlmodel_rust",
    "toon_rust"
)
$seen = @{}

foreach ($line in [IO.File]::ReadAllLines($lockFile)) {
    if ([string]::IsNullOrWhiteSpace($line) -or $line.StartsWith("#")) {
        continue
    }

    $fields = $line.Split([char]"`t")
    if ($fields.Count -ne 2) {
        throw "malformed lock row: $line"
    }
    $repository = $fields[0]
    $revision = $fields[1]

    if ($repository -notin $knownRepositories) {
        throw "unknown repository in lock: $repository"
    }
    if ($revision -notmatch '^[0-9a-f]{40}$') {
        throw "revision for $repository is not a full lowercase hexadecimal commit ID"
    }
    if ($seen.ContainsKey($repository)) {
        throw "duplicate repository in lock: $repository"
    }

    $seen[$repository] = $true
    Checkout-Repository -Repository $repository -Revision $revision -Root $resolvedRoot
}

if ($seen.Count -ne $knownRepositories.Count) {
    throw "expected $($knownRepositories.Count) locked repositories, found $($seen.Count)"
}
foreach ($required in $knownRepositories) {
    if (-not $seen.ContainsKey($required)) {
        throw "required repository missing from lock: $required"
    }
}
