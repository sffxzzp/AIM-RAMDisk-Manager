param(
    [string] $OutputDirectory
)

$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "This script must be run on Windows to build the x86_64-pc-windows-msvc target."
}

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$runtimeReadme = Join-Path $repositoryRoot "runtime\README.md"
$manualRuntimeArchive = Join-Path $repositoryRoot "runtime\AIMtk.zip"
$executableSource = Join-Path $repositoryRoot "target\x86_64-pc-windows-msvc\release\aim-ramdisk-manager.exe"
$shimBuildRoot = Join-Path $repositoryRoot "target\x86_64-pc-windows-msvc\release\build"
# Pin the upstream runtime files. SourceForge may serve the ZIP through different
# mirrors, so validate the extracted binaries instead of the transport container's hash.
$runtimePackageVersion = "20260420"
$runtimePackageUrl = "https://sourceforge.net/projects/aim-toolkit/files/$runtimePackageVersion/AIMtk.zip/download"
$runtimeComponentSha256 = @{
    "RamDyn.exe" = "d3ad871d5fa825ca93e52ea133f8f66101267859d30f58bc3f7ca87e5aaaed8b"
    "aimapi.dll" = "7dca99ed272e1fffcad79d0a068a24e95057c28e2d3ba2ef5485617c5af7b575"
}
$packageMarkerName = ".aimtoolkit-dev-portable"
$packageMarkerContents = "AIMToolkit development portable directory"
$directorySeparators = [char[]]@('\', '/')

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot "dist\dev-portable"
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot $OutputDirectory
}
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$outputPathRoot = [System.IO.Path]::GetPathRoot($OutputDirectory)
if ($OutputDirectory.Length -gt $outputPathRoot.Length) {
    $OutputDirectory = $OutputDirectory.TrimEnd($directorySeparators)
}
if ($OutputDirectory.Equals($outputPathRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "The output directory cannot be a drive or network share root: $OutputDirectory"
}
$parentDirectory = [System.IO.Path]::GetDirectoryName($OutputDirectory)
$directoryName = [System.IO.Path]::GetFileName($OutputDirectory)
if ([string]::IsNullOrWhiteSpace($parentDirectory) -or [string]::IsNullOrWhiteSpace($directoryName)) {
    throw "Invalid output directory: $OutputDirectory"
}
$packageMarkerPath = Join-Path $parentDirectory (".{0}{1}" -f $directoryName, $packageMarkerName)

function Test-X64PeFile {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    $stream = [System.IO.File]::OpenRead($Path)
    $reader = [System.IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) {
            throw "Not a valid PE file: $Path"
        }

        $stream.Position = 0x3C
        $peOffset = [long] $reader.ReadUInt32()
        if ($peOffset -lt 0x40 -or ($peOffset + 6) -gt $stream.Length) {
            throw "The PE header is outside the file bounds: $Path"
        }

        $stream.Position = $peOffset
        if ($reader.ReadUInt32() -ne 0x00004550) {
            throw "Invalid PE signature: $Path"
        }

        $machine = $reader.ReadUInt16()
        if ($machine -ne 0x8664) {
            throw ("The file must be an x64 PE: {0} (Machine=0x{1:X4})" -f $Path, $machine)
        }
    } finally {
        $reader.Dispose()
    }
}

function Test-RuntimePackageArchive {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path
    )

    $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
    try {
        if ($zip.Entries.Count -eq 0) {
            throw "The ZIP archive is empty."
        }
        $cabinetEntries = @(
            $zip.Entries | Where-Object {
                [System.IO.Path]::GetFileName(
                    $_.FullName.Replace('/', [System.IO.Path]::DirectorySeparatorChar)
                ) -ieq "files.cab"
            }
        )
        if ($cabinetEntries.Count -ne 1) {
            throw "Expected one files.cab in the ZIP archive; found $($cabinetEntries.Count)."
        }
    } finally {
        $zip.Dispose()
    }
}

$runtimeNames = @("RamDyn.exe", "aimapi.dll")
foreach ($sourcePath in @(
    (Join-Path $repositoryRoot "README.md"),
    $runtimeReadme
)) {
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) {
        throw "Missing package documentation file: $sourcePath"
    }
}

$sidecarMarkerIsValid = $false
if (Test-Path -LiteralPath $packageMarkerPath) {
    if (-not (Test-Path -LiteralPath $packageMarkerPath -PathType Leaf)) {
        throw "The portable package marker path exists but is not a file: $packageMarkerPath"
    }
    $sidecarMarkerIsValid = [System.IO.File]::ReadAllText($packageMarkerPath) -ceq $packageMarkerContents
    if (-not $sidecarMarkerIsValid) {
        throw "The portable package marker is invalid. Stopping to avoid overwriting unrelated files: $packageMarkerPath"
    }
}

if (Test-Path -LiteralPath $OutputDirectory -PathType Leaf) {
    throw "The output path exists and is not a directory: $OutputDirectory"
}
$existingOutputIsOwned = $false
if (Test-Path -LiteralPath $OutputDirectory -PathType Container) {
    $existingMarker = Join-Path $OutputDirectory $packageMarkerName
    $legacyMarkerIsValid = Test-Path -LiteralPath $existingMarker -PathType Leaf
    if ($legacyMarkerIsValid) {
        $legacyMarkerIsValid = [System.IO.File]::ReadAllText($existingMarker) -ceq $packageMarkerContents
    }
    if (-not $sidecarMarkerIsValid -and -not $legacyMarkerIsValid) {
        throw "The output directory already exists and was not created by this script. Stopping to avoid deleting its contents: $OutputDirectory"
    }
    $existingOutputIsOwned = $true
}

$repositoryRootWithSeparator = $repositoryRoot.TrimEnd($directorySeparators) + [System.IO.Path]::DirectorySeparatorChar
$distRoot = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot "dist"))
$distRootWithSeparator = $distRoot.TrimEnd($directorySeparators) + [System.IO.Path]::DirectorySeparatorChar
$outputDirectoryWithSeparator = $OutputDirectory.TrimEnd($directorySeparators) + [System.IO.Path]::DirectorySeparatorChar
$pathComparison = [System.StringComparison]::OrdinalIgnoreCase
$isRepositoryRoot = $OutputDirectory.Equals($repositoryRoot, $pathComparison)
$isParentOfRepository = $repositoryRoot.StartsWith($outputDirectoryWithSeparator, $pathComparison)
$isInsideRepository = $OutputDirectory.StartsWith($repositoryRootWithSeparator, $pathComparison)
$isInsideDist = $OutputDirectory.StartsWith($distRootWithSeparator, $pathComparison)
if ($isRepositoryRoot -or $isParentOfRepository -or ($isInsideRepository -and -not $isInsideDist)) {
    throw "Output directories inside the project must be under dist. Do not overwrite source files, runtime components, or the project root: $OutputDirectory"
}

$cargo = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue
if ($null -eq $cargo) {
    throw "cargo.exe was not found. Install Rust stable (minimum 1.88) and the MSVC build tools, then add Cargo to PATH."
}

Push-Location $repositoryRoot
try {
    & $cargo.Source build --locked --release --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code: $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $executableSource -PathType Leaf)) {
    throw "The manager executable was not generated: $executableSource"
}
Test-X64PeFile -Path $executableSource
if (-not (Test-Path -LiteralPath $shimBuildRoot -PathType Container)) {
    throw "The Cargo build did not create its output directory for the RamDyn adapter shim: $shimBuildRoot"
}
$shimCandidates = @(
    Get-ChildItem -LiteralPath $shimBuildRoot -Recurse -File -Filter "aimapi-adapter-shim.dll" |
        Where-Object { $_.Length -gt 0 } |
        Sort-Object LastWriteTimeUtc -Descending
)
if ($shimCandidates.Count -eq 0) {
    throw "The Cargo build did not produce aimapi-adapter-shim.dll. Build on Windows x64 with the MSVC toolchain."
}
$adapterShimSource = $shimCandidates[0].FullName
Test-X64PeFile -Path $adapterShimSource

[System.IO.Directory]::CreateDirectory($parentDirectory) | Out-Null

$uniqueSuffix = [System.Guid]::NewGuid().ToString("N")
$stagingDirectory = Join-Path $parentDirectory (".{0}.staging-{1}" -f $directoryName, $uniqueSuffix)
$backupDirectory = Join-Path $parentDirectory (".{0}.backup-{1}" -f $directoryName, $uniqueSuffix)
$runtimeOutput = Join-Path $stagingDirectory "runtime"
$packageCommitted = $false
$packageMarkerCreated = $false

try {
    [System.IO.Directory]::CreateDirectory($runtimeOutput) | Out-Null
    Copy-Item -LiteralPath $executableSource -Destination $stagingDirectory
    $downloadRoot = Join-Path ([System.IO.Path]::GetTempPath()) (
        "AIMToolkit-runtime-{0}" -f [System.Guid]::NewGuid().ToString("N")
    )
    $archiveDirectory = Join-Path $downloadRoot "archive"
    $manualArchiveExists = Test-Path -LiteralPath $manualRuntimeArchive
    if ($manualArchiveExists -and -not (Test-Path -LiteralPath $manualRuntimeArchive -PathType Leaf)) {
        throw "The manual runtime archive path exists but is not a file. Remove or rename it and retry: $manualRuntimeArchive"
    }
    $useManualArchive = $manualArchiveExists
    if ($useManualArchive) {
        $archivePath = $manualRuntimeArchive
    } else {
        $archivePath = Join-Path $downloadRoot "AIMtk.zip"
    }
    $preserveDownloadRoot = $false
    try {
        [System.IO.Directory]::CreateDirectory($archiveDirectory) | Out-Null
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $downloadFailures = [System.Collections.Generic.List[string]]::new()
        $runtimeDownloadUris = @()
        if ($useManualArchive) {
            Write-Host "Found the manually placed AIM Toolkit archive; skipping the online download: $manualRuntimeArchive"
            try {
                [void] (Test-RuntimePackageArchive -Path $archivePath)
            } catch {
                throw "The manual archive is invalid: $manualRuntimeArchive. Download AIMtk.zip again from $runtimePackageUrl and replace this file. Details: $($_.Exception.Message)"
            }
            $archiveIsValid = $true
            $archiveInfo = Get-Item -LiteralPath $archivePath
            Write-Host ("Local archive validation passed: {0} ({1} bytes)" -f $archivePath, $archiveInfo.Length)
        } else {
            Write-Host "Downloading AIM Toolkit $runtimePackageVersion runtime components from SourceForge..."
            $curlCommand = Get-Command curl.exe -CommandType Application -ErrorAction SilentlyContinue
            $runtimeDownloadUris = @($runtimePackageUrl)
            $archiveIsValid = $false
        }
        $previousSecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol
        $previousProgressPreference = $ProgressPreference
        try {
            [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
            $ProgressPreference = "SilentlyContinue"
            foreach ($downloadUri in $runtimeDownloadUris) {
                if (Test-Path -LiteralPath $archivePath -PathType Leaf) {
                    Remove-Item -LiteralPath $archivePath -Force
                }

                $response = $null
                $finalUri = $downloadUri
                $httpStatus = ""
                $contentType = ""
                $contentLength = ""
                try {
                    if ($null -ne $curlCommand) {
                        $curlMetadataPrefix = "AIMTK_CURL_METADATA:"
                        $curlWriteOut = "${curlMetadataPrefix}%{http_code}|%{url_effective}|%{size_download}|%{content_type}"
                        $curlArguments = @(
                            "--location",
                            "--silent",
                            "--show-error",
                            "--max-redirs", "10",
                            "--connect-timeout", "20",
                            "--max-time", "60",
                            "--output", $archivePath,
                            "--user-agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36",
                            "--header", "Accept: application/zip, application/octet-stream, */*",
                            "--write-out", $curlWriteOut,
                            $downloadUri
                        )
                        $curlOutput = @(& $curlCommand.Source @curlArguments 2>&1)
                        $curlExitCode = $LASTEXITCODE
                        $curlLines = @($curlOutput | ForEach-Object { [string] $_ })
                        $metadataLine = $curlLines |
                            Where-Object { $_.StartsWith($curlMetadataPrefix) } |
                            Select-Object -Last 1
                        $curlMessages = $curlLines |
                            Where-Object { -not $_.StartsWith($curlMetadataPrefix) }
                        if ($null -ne $metadataLine) {
                            $metadataParts = $metadataLine.Substring($curlMetadataPrefix.Length) -split '\|', 4
                            if ($metadataParts.Count -ge 4) {
                                $httpStatus = $metadataParts[0]
                                $finalUri = $metadataParts[1]
                                $contentType = $metadataParts[3]
                            }
                        }
                        if ($curlExitCode -ne 0) {
                            throw "curl.exe download failed with exit code ${curlExitCode}: $($curlMessages -join ' ')"
                        }
                        if ([string]::IsNullOrWhiteSpace($httpStatus)) {
                            throw "curl.exe did not return HTTP status information: $($curlMessages -join ' ')"
                        }
                        if ($httpStatus -ne "200") {
                            throw "The server returned HTTP $httpStatus."
                        }
                    } else {
                        $response = Invoke-WebRequest `
                            -Uri $downloadUri `
                            -OutFile $archivePath `
                            -UseBasicParsing `
                            -PassThru `
                            -MaximumRedirection 10 `
                            -Headers @{ Accept = "application/zip, application/octet-stream, */*" } `
                            -UserAgent "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/131.0 Safari/537.36" `
                            -TimeoutSec 60

                        if ($null -ne $response.Headers) {
                            $contentType = [string] $response.Headers["Content-Type"]
                            $contentLength = [string] $response.Headers["Content-Length"]
                        }
                        if ($null -ne $response.BaseResponse) {
                            if ($null -ne $response.BaseResponse.ResponseUri) {
                                $finalUri = $response.BaseResponse.ResponseUri.AbsoluteUri
                            } elseif ($null -ne $response.BaseResponse.RequestMessage.RequestUri) {
                                $finalUri = $response.BaseResponse.RequestMessage.RequestUri.AbsoluteUri
                            }
                        }
                        $httpStatus = [string] $response.StatusCode
                    }

                    if (-not (Test-Path -LiteralPath $archivePath -PathType Leaf)) {
                        throw "The download request finished but did not create an archive file."
                    }
                    $archiveInfo = Get-Item -LiteralPath $archivePath
                    if ($contentLength -match '^\d+$' -and [long] $contentLength -ne $archiveInfo.Length) {
                        throw "The downloaded size does not match the server declaration (declared $contentLength bytes, actual $($archiveInfo.Length) bytes)."
                    }
                    if ($contentType -match 'text/html|application/json') {
                        throw "The server response is not a ZIP archive (Content-Type: $contentType)."
                    }

                    [void] (Test-RuntimePackageArchive -Path $archivePath)

                    $archiveIsValid = $true
                    Write-Host ("SourceForge download validation passed: {0} ({1} bytes)" -f $finalUri, $archiveInfo.Length)
                    break
                } catch {
                    $errorMessage = $_.Exception.Message
                    if ($null -ne $_.Exception.Response) {
                        $failedResponse = $_.Exception.Response
                        if ($null -ne $failedResponse.StatusCode) {
                            $httpStatus = [string] [int] $failedResponse.StatusCode
                        }
                        if ($null -ne $failedResponse.ResponseUri) {
                            $finalUri = $failedResponse.ResponseUri.AbsoluteUri
                        }
                        if ($null -ne $failedResponse.Headers) {
                            $contentType = [string] $failedResponse.Headers["Content-Type"]
                            $contentLength = [string] $failedResponse.Headers["Content-Length"]
                        }
                    }
                    $archiveSize = "unknown"
                    $archiveHash = "unknown"
                    if (Test-Path -LiteralPath $archivePath -PathType Leaf) {
                        $failedArchiveInfo = Get-Item -LiteralPath $archivePath
                        $archiveSize = [string] $failedArchiveInfo.Length
                        $archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash
                    }
                    $downloadFailures.Add((
                        "Request URL: {0}; Final URL: {1}; HTTP: {2}; Content-Type: {3}; Server size: {4}; Actual size: {5}; SHA-256: {6}; Error: {7}" -f
                        $downloadUri,
                        $finalUri,
                        $(if ([string]::IsNullOrWhiteSpace($httpStatus)) { "unknown" } else { $httpStatus }),
                        $(if ([string]::IsNullOrWhiteSpace($contentType)) { "unknown" } else { $contentType }),
                        $(if ([string]::IsNullOrWhiteSpace($contentLength)) { "unknown" } else { $contentLength }),
                        $archiveSize,
                        $archiveHash,
                        $errorMessage
                    ))
                    Write-Warning ("SourceForge automatic download failed for {0}: {1}" -f $downloadUri, $errorMessage)
                }
            }

            if (-not $archiveIsValid) {
                if ((Test-Path -LiteralPath $archivePath -PathType Leaf) -and $httpStatus -notmatch '^[45]\d\d$') {
                    $preserveDownloadRoot = $true
                }
                $failureDetails = $downloadFailures -join [System.Environment]::NewLine
                $manualInstructions = @(
                    "Automatic download failed. Download the AIM Toolkit $runtimePackageVersion archive in a web browser:",
                    $runtimePackageUrl,
                    "Save the downloaded file as AIMtk.zip at: $manualRuntimeArchive",
                    "Then rerun scripts\build-dev-portable.ps1; the script will detect the file and skip the online download."
                ) -join [System.Environment]::NewLine
                if ($preserveDownloadRoot) {
                    throw "$manualInstructions`n`nDownload diagnostics: $failureDetails`nFailed response kept at: $archivePath"
                }
                throw "$manualInstructions`n`nDownload diagnostics: $failureDetails"
            }
        } finally {
            $ProgressPreference = $previousProgressPreference
            [System.Net.ServicePointManager]::SecurityProtocol = $previousSecurityProtocol
        }

        Expand-Archive -LiteralPath $archivePath -DestinationPath $archiveDirectory
        $cabinetFiles = @(Get-ChildItem -LiteralPath $archiveDirectory -Recurse -File -Filter "files.cab")
        if ($cabinetFiles.Count -ne 1) {
            throw "Expected one files.cab in the SourceForge archive; found $($cabinetFiles.Count)."
        }
        $cabinetPath = $cabinetFiles[0].FullName
        $expandExecutable = Join-Path $env:SystemRoot "System32\expand.exe"
        if (-not (Test-Path -LiteralPath $expandExecutable -PathType Leaf)) {
            throw "Windows built-in expand.exe was not found: $expandExecutable"
        }
        foreach ($runtimeName in $runtimeNames) {
            & $expandExecutable "-F:$runtimeName" $cabinetPath $runtimeOutput
            if ($LASTEXITCODE -ne 0) {
                throw "Failed to extract $runtimeName from the SourceForge archive (exit code: $LASTEXITCODE)."
            }
        }
    } finally {
        if ($preserveDownloadRoot) {
            Write-Warning "The SourceForge download did not pass validation and was kept for diagnosis: $archivePath"
        } elseif (Test-Path -LiteralPath $downloadRoot -PathType Container) {
            try {
                Remove-Item -LiteralPath $downloadRoot -Recurse -Force -ErrorAction Stop
            } catch {
                Write-Warning "Failed to clean up the temporary SourceForge download directory: $downloadRoot; reason: $_"
            }
        }
    }
    foreach ($runtimeName in $runtimeNames) {
        $runtimePath = Join-Path $runtimeOutput $runtimeName
        if (-not (Test-Path -LiteralPath $runtimePath -PathType Leaf)) {
            throw "Runtime component is missing after extraction: $runtimePath"
        }
        $expectedHash = $runtimeComponentSha256[$runtimeName]
        $actualHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $runtimePath).Hash
        if ($actualHash -ne $expectedHash) {
            throw "SHA-256 mismatch for runtime component $runtimeName. Expected $expectedHash, got $actualHash. Verify that AIMtk.zip is the 20260420 release from $runtimePackageUrl. Replace the manual archive at $manualRuntimeArchive and retry."
        }
        Test-X64PeFile -Path $runtimePath
    }

    $backendApiPath = Join-Path $runtimeOutput "aimapi-backend.dll"
    Move-Item -LiteralPath (Join-Path $runtimeOutput "aimapi.dll") -Destination $backendApiPath
    Copy-Item -LiteralPath $adapterShimSource -Destination (Join-Path $runtimeOutput "aimapi.dll")

    Copy-Item -LiteralPath (Join-Path $repositoryRoot "README.md") -Destination $stagingDirectory
    Copy-Item -LiteralPath $runtimeReadme -Destination $runtimeOutput
    if (-not $sidecarMarkerIsValid) {
        [System.IO.File]::WriteAllText(
            $packageMarkerPath,
            $packageMarkerContents,
            [System.Text.Encoding]::UTF8
        )
        $packageMarkerCreated = $true
    }

    $existingOutput = Test-Path -LiteralPath $OutputDirectory -PathType Container
    if ($existingOutput -and -not $existingOutputIsOwned) {
        throw "The output directory appeared during the build and was not created by this script. Stopping to avoid deleting its contents: $OutputDirectory"
    }
    if ($existingOutput) {
        [System.IO.Directory]::Move($OutputDirectory, $backupDirectory)
    }

    try {
        [System.IO.Directory]::Move($stagingDirectory, $OutputDirectory)
        $packageCommitted = $true
    } catch {
        $commitError = $_
        if ($existingOutput -and (Test-Path -LiteralPath $backupDirectory -PathType Container)) {
            try {
                [System.IO.Directory]::Move($backupDirectory, $OutputDirectory)
            } catch {
                throw "Failed to commit the package, and failed to restore the previous directory. The backup is at $backupDirectory. Commit error: $commitError; restore error: $_"
            }
        }
        throw $commitError
    }

    if ($existingOutput -and (Test-Path -LiteralPath $backupDirectory -PathType Container)) {
        try {
            [System.IO.Directory]::Delete($backupDirectory, $true)
        } catch {
            Write-Warning "The new portable directory was created, but the previous directory backup could not be deleted: $backupDirectory; reason: $_"
        }
    }
} finally {
    if (-not $packageCommitted -and (Test-Path -LiteralPath $stagingDirectory -PathType Container)) {
        try {
            [System.IO.Directory]::Delete($stagingDirectory, $true)
        } catch {
            Write-Warning "Failed to clean up the staging directory: $stagingDirectory; reason: $_"
        }
    }
    if (-not $packageCommitted -and $packageMarkerCreated -and -not $existingOutputIsOwned) {
        try {
            Remove-Item -LiteralPath $packageMarkerPath -Force -ErrorAction Stop
        } catch {
            Write-Warning "Failed to remove the portable package marker: $packageMarkerPath; reason: $_"
        }
    }
}

Write-Host "Development portable directory created: $OutputDirectory"
Get-FileHash -Algorithm SHA256 -LiteralPath @(
    (Join-Path $OutputDirectory "aim-ramdisk-manager.exe"),
    (Join-Path $OutputDirectory "runtime\RamDyn.exe"),
    (Join-Path $OutputDirectory "runtime\aimapi-backend.dll"),
    (Join-Path $OutputDirectory "runtime\aimapi.dll")
)
