# tools/release/package-windows.ps1 -Binary <zipp.exe> -OutDir <dir> -Version <v> -Commit <sha> -WixBin <dir> [-TestInstall]
#
# The two Windows release assets, from one built zipp.exe:
#   zipp-<version>-windows-x86_64-portable.exe   the binary itself; run it anywhere
#   zipp-<version>-windows-x86_64-installer.msi  a per-user install of the same
#       binary into %LOCALAPPDATA%\Programs\ZIPP, added to the user's PATH
#       (tools/release/zipp.wxs; WiX Toolset 3.14's candle and light in -WixBin)
#
# Both are checked before they count. The CLI runs the VM on its process main
# thread, so the PE stack fields are a runtime safety contract: the exact
# copy that will be uploaded is inspected rather than assuming the link
# argument took effect. It must also report this version at this commit from
# a clean tree (an asset reporting another commit, or dirty:true, used to be
# published immutably).
param(
  [Parameter(Mandatory)] [string] $Binary,
  [Parameter(Mandatory)] [string] $OutDir,
  [Parameter(Mandatory)] [string] $Version,
  [Parameter(Mandatory)] [string] $Commit,
  [Parameter(Mandatory)] [string] $WixBin,
  # Install the MSI for this user, run the installed zipp, uninstall, and
  # check that the files and the PATH entry are gone (CI's runner is
  # disposable; locally this briefly changes the user's PATH).
  [switch] $TestInstall
)
$ErrorActionPreference = 'Stop'

function Get-PeStackSizes([string] $Path) {
  [byte[]] $bytes = [IO.File]::ReadAllBytes($Path)
  if ($bytes.LongLength -lt 64 -or $bytes[0] -ne 0x4d -or $bytes[1] -ne 0x5a) {
    throw "packaged zipp.exe has no valid DOS header"
  }
  [uint64] $peOffset64 = [BitConverter]::ToUInt32($bytes, 0x3c)
  if ($peOffset64 + 26 -gt [uint64] $bytes.LongLength) {
    throw "packaged zipp.exe has a truncated PE header"
  }
  [int] $peOffset = $peOffset64
  if ($bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or
      $bytes[$peOffset + 2] -ne 0 -or $bytes[$peOffset + 3] -ne 0) {
    throw "packaged zipp.exe has no valid PE signature"
  }
  # The optional header follows the 4-byte signature and 20-byte COFF
  # header. PE32+ stores the two stack fields as u64; PE32 uses u32 fields
  # at the corresponding offsets.
  [int] $optional = $peOffset + 24
  [uint16] $magic = [BitConverter]::ToUInt16($bytes, $optional)
  if ($magic -eq 0x20b) {
    if ([uint64] $optional + 0x58 -gt [uint64] $bytes.LongLength) {
      throw "packaged zipp.exe has a truncated PE32+ optional header"
    }
    [uint64] $reserve = [BitConverter]::ToUInt64($bytes, $optional + 0x48)
    [uint64] $commitSize = [BitConverter]::ToUInt64($bytes, $optional + 0x50)
  } elseif ($magic -eq 0x10b) {
    if ([uint64] $optional + 0x50 -gt [uint64] $bytes.LongLength) {
      throw "packaged zipp.exe has a truncated PE32 optional header"
    }
    [uint64] $reserve = [BitConverter]::ToUInt32($bytes, $optional + 0x48)
    [uint64] $commitSize = [BitConverter]::ToUInt32($bytes, $optional + 0x4c)
  } else {
    throw ('packaged zipp.exe has unexpected optional-header magic 0x{0:x}' -f $magic)
  }
  return [pscustomobject]@{ Reserve = $reserve; Commit = $commitSize }
}

function Assert-Identity([string] $Path) {
  $identity = (& $Path --version --json) | ConvertFrom-Json
  if ($LASTEXITCODE -ne 0) { throw "$Path --version failed" }
  if ($identity.name -ne 'zipp' -or $identity.version -ne $Version -or
      $identity.commit -ne $Commit -or $identity.dirty -ne $false) {
    throw "$Path does not report release $Version at $Commit (commit=$($identity.commit), dirty=$($identity.dirty))"
  }
}

$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
New-Item -ItemType Directory -Path $OutDir -Force | Out-Null
$OutDir = (Resolve-Path $OutDir).Path
$portable = Join-Path $OutDir "zipp-$Version-windows-x86_64-portable.exe"
$msi = Join-Path $OutDir "zipp-$Version-windows-x86_64-installer.msi"
Copy-Item -LiteralPath $Binary -Destination $portable -Force

$stack = Get-PeStackSizes $portable
if ($stack.Reserve -ne 268435456 -or $stack.Commit -ne 4096) {
  throw "packaged zipp.exe has stack reserve=$($stack.Reserve), commit=$($stack.Commit); expected reserve=268435456, commit=4096"
}
Write-Host "verified packaged PE stack: reserve=$($stack.Reserve), commit=$($stack.Commit)"
Assert-Identity $portable
Write-Host "verified identity: zipp $Version at $Commit"

# The installer, around the portable binary's exact bytes.
$work = Join-Path ([IO.Path]::GetTempPath()) "zipp-msi-$PID"
Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $work | Out-Null
try {
  & (Join-Path $WixBin 'candle.exe') -nologo -arch x64 -out "$work\zipp.wixobj" `
    "-dVersion=$Version" "-dCommit=$Commit" "-dExe=$portable" `
    "-dReadme=$root\README.md" "-dLicense=$root\LICENSE-APACHE" "$root\tools\release\zipp.wxs"
  if ($LASTEXITCODE -ne 0) { throw "candle failed ($LASTEXITCODE)" }
  & (Join-Path $WixBin 'light.exe') -nologo -spdb -out $msi "$work\zipp.wixobj"
  if ($LASTEXITCODE -ne 0) { throw "light failed ($LASTEXITCODE)" }

  # What the database says: the version, the per-user scope, the PATH
  # entry ('=-' sets it and removes it on uninstall; '[~];' appends).
  Add-Type -Path (Join-Path $WixBin 'Microsoft.Deployment.WindowsInstaller.dll')
  $db = New-Object Microsoft.Deployment.WindowsInstaller.Database($msi, [Microsoft.Deployment.WindowsInstaller.DatabaseOpenMode]::ReadOnly)
  try {
    $msiVersion = $db.ExecuteScalar("SELECT Value FROM Property WHERE Property='ProductVersion'")
    $allUsers = @($db.ExecuteStringQuery("SELECT Value FROM Property WHERE Property='ALLUSERS'"))
    $pathValue = $db.ExecuteScalar("SELECT Value FROM Environment WHERE Name='=-PATH'")
  } finally {
    $db.Close()
  }
  if ($msiVersion -ne $Version) { throw "installer ProductVersion is $msiVersion, not $Version" }
  if ($allUsers.Count -ne 0) { throw "installer sets ALLUSERS; it must be a per-user package" }
  if ($pathValue -ne '[~];[INSTALLFOLDER]') { throw "installer PATH entry is '$pathValue'" }

  # Its payload, extracted without installing: the portable binary's bytes.
  $extract = Join-Path $work 'extract'
  $p = Start-Process msiexec.exe -ArgumentList @('/a', "`"$msi`"", '/qn', "TARGETDIR=`"$extract`"") -Wait -PassThru
  if ($p.ExitCode -ne 0) { throw "administrative extract failed ($($p.ExitCode))" }
  $inside = Get-ChildItem -Recurse -File $extract -Filter zipp.exe | Select-Object -First 1
  if (-not $inside -or (Get-FileHash $inside.FullName).Hash -ne (Get-FileHash $portable).Hash) {
    throw "the installer's zipp.exe is not the portable binary"
  }
  Write-Host "verified installer: $Version, per-user, PATH += [INSTALLFOLDER], payload = portable exe"

  if ($TestInstall) {
    $folder = Join-Path $env:LOCALAPPDATA 'Programs\ZIPP'
    $userPath = { [Environment]::GetEnvironmentVariable('PATH', 'User') }
    $p = Start-Process msiexec.exe -ArgumentList @('/i', "`"$msi`"", '/qn', '/l*v', "`"$work\install.log`"") -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "install failed ($($p.ExitCode)); see $work\install.log" }
    $installed = Join-Path $folder 'zipp.exe'
    if (-not (Test-Path $installed)) { throw "install left no $installed" }
    if ((& $userPath) -notlike "*$folder*") { throw "install did not add $folder to the user PATH" }
    Assert-Identity $installed
    [IO.File]::WriteAllText("$work\probe.js", 'console.log(6 * 7)')
    $out = & $installed js "$work\probe.js"
    if ($out -ne '42') { throw "installed zipp printed '$out'" }
    Write-Host "installed zipp ran: $out"
    $p = Start-Process msiexec.exe -ArgumentList @('/x', "`"$msi`"", '/qn', '/l*v', "`"$work\uninstall.log`"") -Wait -PassThru
    if ($p.ExitCode -ne 0) { throw "uninstall failed ($($p.ExitCode)); see $work\uninstall.log" }
    if (Test-Path $installed) { throw "uninstall left $installed" }
    if ((& $userPath) -like "*$folder*") { throw "uninstall left $folder on the user PATH" }
    Write-Host "verified install, run and uninstall for this user"
  }
} finally {
  Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
Write-Output $portable
Write-Output $msi