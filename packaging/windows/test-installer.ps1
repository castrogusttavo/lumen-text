# End-to-end test of the Windows installer on a runner: silent install into a temp dir, assert files,
# uninstall entry, shortcuts, PATH and context menus, run the installed exe, upgrade over it, then
# silent uninstall and assert everything is gone while user settings survive.
#   test-installer.ps1 -Setup <setup.exe> -Version <v> [-ExpectVersionOutput "lumen-text <v>"]
param(
  [Parameter(Mandatory)] [string] $Setup,
  [Parameter(Mandatory)] [string] $Version,
  [string] $ExpectVersionOutput = ""
)
$ErrorActionPreference = "Stop"
$appId = "{B7C1E0D4-5A3F-4C8E-9E21-6F0A7D3C41B2}_is1"
$uninstKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appId"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("lumen-setup-test-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmp | Out-Null
$dir = Join-Path $tmp "Lumen"
$setup = (Resolve-Path $Setup).Path
$startMenu = Join-Path ([Environment]::GetFolderPath("Programs")) "Lumen.lnk"
$desktop = Join-Path ([Environment]::GetFolderPath("DesktopDirectory")) "Lumen.lnk"
$settings = Join-Path $env:APPDATA "lumen-text"

function Assert($cond, $msg) { if (-not $cond) { throw "ASSERT FAILED: $msg" } }
function Run($file, $argList) {
  $p = Start-Process -FilePath $file -ArgumentList $argList -Wait -PassThru
  return $p.ExitCode
}
function Install($n) {
  $log = Join-Path $tmp "install$n.log"
  $code = Run $setup @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER", "/DIR=`"$dir`"",
    "/TASKS=`"desktopicon,addtopath,contextmenu,fileassoc`"", "/LOG=`"$log`"")
  if ($code -ne 0) { Get-Content $log -ErrorAction SilentlyContinue | Select-Object -Last 40; throw "installer exit code $code" }
}
function UserPath() { (Get-ItemProperty -Path "HKCU:\Environment" -Name Path -ErrorAction SilentlyContinue).Path }

try {
  New-Item -ItemType Directory -Path $settings -Force | Out-Null
  Set-Content -Path (Join-Path $settings "ci-marker") -Value "keep me"

  Write-Host "== silent install"
  Install 1
  foreach ($f in "lumen-text.exe", "ghp-lsp.exe", "lumen-text.ico", "LICENSE", "unins000.exe", "assets\language-manifest.json") {
    Assert (Test-Path (Join-Path $dir $f)) "missing $f in the install dir"
  }
  Assert (-not (Test-Path (Join-Path $dir "assets\icons\hugeicons"))) "hugeicons leaked into the installer"
  $k = Get-ItemProperty -Path $uninstKey
  Assert ($k.DisplayVersion -eq $Version) "DisplayVersion is '$($k.DisplayVersion)', expected $Version"
  Assert ($k.DisplayName -like "Lumen*") "DisplayName is '$($k.DisplayName)'"
  Assert ($k.Publisher) "Publisher is empty"
  Assert ($k.URLInfoAbout -like "https://github.com/*") "URLInfoAbout is '$($k.URLInfoAbout)'"
  Assert ($k.InstallLocation.TrimEnd('\') -eq $dir) "InstallLocation is '$($k.InstallLocation)'"
  Assert (Test-Path $startMenu) "Start Menu shortcut missing: $startMenu"
  Assert (Test-Path $desktop) "desktop shortcut missing: $desktop"
  Assert ((UserPath) -split ";" -contains $dir) "install dir is not in the user PATH"
  foreach ($sub in "*\shell\Lumen\command", "Directory\shell\Lumen\command", "Directory\Background\shell\Lumen\command") {
    Assert (Test-Path "HKCU:\Software\Classes\$sub") "context menu key missing: $sub"
  }
  Assert ((Get-ItemProperty "HKCU:\Software\Classes\.md\OpenWithProgids").PSObject.Properties.Name -contains "Lumen.File") "Open with entry missing"

  Write-Host "== run the installed exe"
  $out = & (Join-Path $dir "lumen-text.exe") --version 2>&1 | Out-String
  Write-Host $out
  if ($ExpectVersionOutput) { Assert ($out.Trim() -eq $ExpectVersionOutput) "--version printed '$($out.Trim())', expected '$ExpectVersionOutput'" }

  Write-Host "== upgrade over the previous install"
  Set-Content -Path (Join-Path $dir "assets\stale-file") -Value "removed by the upgrade"
  Install 2
  Assert (-not (Test-Path (Join-Path $dir "assets\stale-file"))) "stale asset survived the upgrade"
  Assert (Test-Path (Join-Path $dir "assets\language-manifest.json")) "assets missing after the upgrade"
  $n = ((UserPath) -split ";" | Where-Object { $_ -eq $dir }).Count
  Assert ($n -eq 1) "install dir appears $n times in PATH after the upgrade"

  Write-Host "== silent uninstall"
  $code = Run (Join-Path $dir "unins000.exe") @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART")
  Assert ($code -eq 0) "uninstaller exit code $code"
  # The uninstaller finishes from a temp copy; give it a moment.
  for ($i = 0; $i -lt 60 -and (Test-Path $dir); $i++) { Start-Sleep -Seconds 1 }
  Assert (-not (Test-Path $dir)) "install dir still exists after uninstall: $(Get-ChildItem $dir -ErrorAction SilentlyContinue | Out-String)"
  Assert (-not (Test-Path $uninstKey)) "uninstall registry key still exists"
  Assert (-not (Test-Path $startMenu)) "Start Menu shortcut still exists"
  Assert (-not (Test-Path $desktop)) "desktop shortcut still exists"
  Assert (-not ((UserPath) -split ";" -contains $dir)) "install dir still in the user PATH"
  foreach ($sub in "*\shell\Lumen", "Directory\shell\Lumen", "Directory\Background\shell\Lumen", "Lumen.File") {
    Assert (-not (Test-Path "HKCU:\Software\Classes\$sub")) "registry key still exists: $sub"
  }
  Assert (Test-Path (Join-Path $settings "ci-marker")) "user settings were deleted by the silent uninstall"
  Write-Host "installer test OK"
}
finally {
  Remove-Item -Recurse -Force $settings -ErrorAction SilentlyContinue
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
