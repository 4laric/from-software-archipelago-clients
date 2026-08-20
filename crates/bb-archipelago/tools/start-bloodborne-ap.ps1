[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $BBLauncherPath,
    [Parameter(Mandatory)] [string] $ClientPath,
    [Parameter(Mandatory)] [string] $Server,
    [Parameter(Mandatory)] [string] $Slot,
    [Parameter(Mandatory)] [string] $Config,
    [Parameter(Mandatory)] [string] $Ledger,
    [string] $Password,
    [switch] $AssumeCorrectSave,
    [ValidateRange(10, 3600)] [int] $WaitSeconds = 600
)

$ErrorActionPreference = "Stop"

function Test-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Quote-Argument([string] $Value) {
    '"' + $Value.Replace('"', '\"') + '"'
}

if (-not (Test-Administrator)) {
    $shell = (Get-Process -Id $PID).Path
    $arguments = @(
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-File", (Quote-Argument $PSCommandPath),
        "-BBLauncherPath", (Quote-Argument $BBLauncherPath),
        "-ClientPath", (Quote-Argument $ClientPath),
        "-Server", (Quote-Argument $Server),
        "-Slot", (Quote-Argument $Slot),
        "-Config", (Quote-Argument $Config),
        "-Ledger", (Quote-Argument $Ledger),
        "-WaitSeconds", $WaitSeconds
    )
    if ($Password) {
        $arguments += @("-Password", (Quote-Argument $Password))
    }
    if ($AssumeCorrectSave) {
        $arguments += "-AssumeCorrectSave"
    }
    Start-Process -FilePath $shell -ArgumentList ($arguments -join " ") -Verb RunAs
    exit
}

foreach ($path in @($BBLauncherPath, $ClientPath, $Config)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required file does not exist: $path"
    }
}

Write-Host "Starting BBLauncher. Choose the Bloodborne build and press Play."
# BBLauncher must stay visible because the player selects patches/build and starts the game.
Start-Process -FilePath (Resolve-Path -LiteralPath $BBLauncherPath).Path | Out-Null

$deadline = [DateTime]::UtcNow.AddSeconds($WaitSeconds)
$shad = $null
while ([DateTime]::UtcNow -lt $deadline) {
    $matches = @(Get-Process -Name shadPS4 -ErrorAction SilentlyContinue)
    if ($matches.Count -eq 1) {
        $shad = $matches[0]
        break
    }
    if ($matches.Count -gt 1) {
        throw "Multiple shadPS4 processes are running; close the unused instances."
    }
    Start-Sleep -Milliseconds 500
}
if (-not $shad) {
    throw "Timed out waiting for shadPS4 after $WaitSeconds seconds."
}

Write-Host ("shadPS4 PID {0} detected; starting Bloodborne AP client." -f $shad.Id)
$clientArguments = @($Server, $Slot, $Config, $Ledger)
if ($Password) {
    $clientArguments += $Password
}
if ($AssumeCorrectSave) {
    $clientArguments += "--assume-correct-save"
}
& (Resolve-Path -LiteralPath $ClientPath).Path @clientArguments
exit $LASTEXITCODE
