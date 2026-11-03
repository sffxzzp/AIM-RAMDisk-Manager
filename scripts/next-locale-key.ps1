$ErrorActionPreference = "Stop"

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$catalogPaths = @(
    (Join-Path $repositoryRoot "locales\en.json"),
    (Join-Path $repositoryRoot "locales\zh-CN.json")
)
$allKeys = @()

foreach ($catalogPath in $catalogPaths) {
    if (-not (Test-Path -LiteralPath $catalogPath -PathType Leaf)) {
        throw "Missing localization catalog: $catalogPath"
    }

    $catalog = Get-Content -LiteralPath $catalogPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $allKeys += @($catalog.messages.PSObject.Properties.Name)
}

$numbers = foreach ($key in $allKeys) {
    if ($key -notmatch '^msg_(\d+)$') {
        throw "Unexpected localization key format: $key"
    }
    [int] $Matches[1]
}

$nextNumber = ($numbers | Measure-Object -Maximum).Maximum + 1
"msg_{0:D4}" -f $nextNumber
