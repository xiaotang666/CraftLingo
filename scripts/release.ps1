<#
.SYNOPSIS
  方块译匠 · CraftLingo 一键发版：打包 -> 签名 -> 生成 update.json -> 上传 GitHub Release。

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release.ps1 -Notes "修复若干问题"

.EXAMPLE
  # 只用已打好的包，不重新编译
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/release.ps1 -SkipBuild -DryRun
#>
param(
  [string]$Version,
  [string]$Notes = "",
  [string]$Repo = "xiaotang666/CraftLingo",
  [string]$KeyPath = "",
  [switch]$SkipBuild,
  [switch]$Portable,
  [switch]$DryRun
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
Set-Location $root

$cargoBin = Join-Path $env:USERPROFILE ".cargo/bin"
if (Test-Path $cargoBin) {
  $env:PATH = "$cargoBin;$env:PATH"
}

# ── 版本号：以 tauri.conf.json 为准 ──────────────────────────
$conf = Get-Content -Raw (Join-Path $root "src-tauri/tauri.conf.json") | ConvertFrom-Json
$confVersion = $conf.version
if (-not $Version) {
  $Version = $confVersion
}
elseif ($Version -ne $confVersion) {
  throw "版本号不一致：-Version=$Version，tauri.conf.json=$confVersion。请先同步再发版。"
}

$tag = "v$Version"
$setupName = "CraftLingo_${Version}_x64-setup.exe"
$assetDir = (Resolve-Path (Join-Path $root "..\资产")).Path
$nsisDir = Join-Path $root "src-tauri/target/release/bundle/nsis"

Write-Host "==> 版本 $Version / 仓库 $Repo" -ForegroundColor Cyan

# ── 签名环境：从私钥文件加载，密码交互输入 ──────────────────
if (-not $KeyPath) { $KeyPath = Join-Path $root "..\.keys\craftlingo.key" }

function Initialize-SigningKey {
  if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
    if (-not (Test-Path $KeyPath)) { throw "找不到签名私钥：$KeyPath" }
    $env:TAURI_SIGNING_PRIVATE_KEY = (Get-Content -Raw $KeyPath).Trim()
  }
  # 私钥未加密时不需要密码；仅在外部已提供时才使用
  if ($env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -eq "") {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
  }
}

# ── 1. 打包（含签名） ────────────────────────────────────────
if (-not $SkipBuild) {
  Write-Host "==> 开始打包..." -ForegroundColor Cyan
  Initialize-SigningKey
  & (Join-Path $PSScriptRoot "package-exe.ps1")
  if ($LASTEXITCODE -ne 0) { throw "打包失败（exit $LASTEXITCODE）" }
}

$exe = Join-Path $nsisDir $setupName
if (-not (Test-Path $exe)) {
  throw "安装包不存在：$exe（先执行打包，或加 -SkipBuild 前先确认产物路径）"
}
$sig = "$exe.sig"

# ── 2. 缺少签名时用私钥补签 ─────────────────────────────────
if (-not (Test-Path $sig)) {
  Initialize-SigningKey
  Write-Host "==> 补签安装包..." -ForegroundColor Cyan
  $signer = Join-Path $root "node_modules/.bin/tauri.cmd"
  $signArgs = @("signer", "sign", "-f", $KeyPath)
  if ($env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) { $signArgs += @("-p", $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) }
  $signArgs += $exe
  $sigText = (& $signer @signArgs 2>$null | Out-String).Trim()
  if (-not $sigText) { throw "签名失败" }
  [IO.File]::WriteAllText($sig, $sigText, (New-Object Text.UTF8Encoding($false)))
}

# ── 3. 整理产物到资产目录（先归档旧版本） ──────────────────
$archiveRoot = Join-Path (Split-Path $assetDir -Parent) "旧版本资产归档"

function Move-OldAssets {
  if (-not (Test-Path $assetDir)) { return }
  Get-ChildItem $assetDir -File | ForEach-Object {
    $match = [regex]::Match($_.Name, '(\d+\.\d+\.\d+)')
    if (-not $match.Success) { return }
    $fileVersion = $match.Groups[1].Value
    if ($fileVersion -eq $Version) { return }
    $dest = Join-Path $archiveRoot "v$fileVersion"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Move-Item $_.FullName $dest -Force
    Write-Host "==> 归档旧资产 $($_.Name) -> v$fileVersion" -ForegroundColor DarkGray
  }
  if (Test-Path $archiveRoot) {
    Get-ChildItem $archiveRoot -Directory |
      Where-Object { $_.Name -match '^v\d+\.\d+\.\d+$' } |
      Sort-Object { [version]($_.Name.TrimStart('v')) } -Descending |
      Select-Object -Skip 5 |
      ForEach-Object {
        Remove-Item $_.FullName -Recurse -Force
        Write-Host "==> 清理过期归档 $($_.Name)" -ForegroundColor DarkGray
      }
  }
}

if (-not $DryRun) { Move-OldAssets }
if (-not (Test-Path $assetDir)) { New-Item -ItemType Directory -Path $assetDir | Out-Null }
Copy-Item $exe (Join-Path $assetDir $setupName) -Force
Copy-Item $sig (Join-Path $assetDir "$setupName.sig") -Force

$portableExe = $null
$portableZip = $null
if ($Portable) {
  $bin = Join-Path $root "src-tauri/target/release/craftlingo.exe"
  if (-not (Test-Path $bin)) { throw "便携版源文件不存在：$bin" }
  $portableExe = Join-Path $assetDir "CraftLingo-v$Version-portable.exe"
  $portableZip = Join-Path $assetDir "CraftLingo-v$Version-portable.zip"
  Copy-Item $bin $portableExe -Force
  Compress-Archive -Path $portableExe -DestinationPath $portableZip -Force
}

# ── 4. 生成 update.json ─────────────────────────────────────
$signature = (Get-Content -Raw $sig).Trim()
$payload = [ordered]@{
  version   = $Version
  notes     = $(if ($Notes) { $Notes } else { "方块译匠 · CraftLingo v$Version" })
  pub_date  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
  platforms = [ordered]@{
    "windows-x86_64" = [ordered]@{
      signature = $signature
      # 走镜像下载，国内网络可用；签名校验保证完整性
      url       = "https://ghfast.top/https://github.com/$Repo/releases/download/$tag/$setupName"
    }
  }
}
$updateFile = Join-Path $assetDir "update.json"
$json = $payload | ConvertTo-Json -Depth 6
[IO.File]::WriteAllText($updateFile, $json, (New-Object Text.UTF8Encoding($false)))
Write-Host "==> 已生成 $updateFile" -ForegroundColor Cyan

$uploads = @(
  (Join-Path $assetDir $setupName),
  (Join-Path $assetDir "$setupName.sig"),
  $updateFile
)
if ($Portable) { $uploads += $portableExe; $uploads += $portableZip }

if ($DryRun) {
  Write-Host "==> DryRun，将上传以下文件：" -ForegroundColor Yellow
  $uploads | ForEach-Object { Write-Host "    $_" }
  Write-Host "==> 目标 Release：$tag（$Repo）"
  return
}

# ── 5. 取得 GitHub 凭据 ─────────────────────────────────────
$token = $env:GITHUB_TOKEN
if (-not $token) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = "git"
  $psi.Arguments = "credential fill"
  $psi.RedirectStandardInput = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $psi.UseShellExecute = $false
  $proc = [System.Diagnostics.Process]::Start($psi)
  $proc.StandardInput.Write("protocol=https`nhost=github.com`n`n")
  $proc.StandardInput.Close()
  $credOut = $proc.StandardOutput.ReadToEnd()
  $proc.WaitForExit()
  $line = ($credOut -split "`n") | Where-Object { $_ -like "password=*" }
  if ($line) { $token = ($line -replace '^password=', '').Trim() }
}
if (-not $token) {
  throw "未找到 GitHub 凭据：请先 git 登录（凭据管理器保存 token），或临时设置 `$env:GITHUB_TOKEN（需要 repo 权限，用来创建 Release 并上传附件）。"
}

$headers = @{
  Authorization = "token $token"
  Accept        = "application/vnd.github+json"
  "User-Agent"  = "craftlingo-release"
}

# ── 6. 创建或复用 Release ───────────────────────────────────
$release = $null
try {
  $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$tag" -Headers $headers -Method Get
  Write-Host "==> Release $tag 已存在，复用" -ForegroundColor Yellow
}
catch {
  $code = $_.Exception.Response.StatusCode.value__
  if ($code -ne 404) { throw "查询 Release 失败：$($_.Exception.Message)" }
}

if (-not $release) {
  $bodyObj = @{
    tag_name   = $tag
    name       = "方块译匠 · CraftLingo $tag"
    body       = $(if ($Notes) { $Notes } else { "方块译匠 · CraftLingo $tag" })
    draft      = $false
    prerelease = $false
  }
  $bodyBytes = [Text.Encoding]::UTF8.GetBytes(($bodyObj | ConvertTo-Json))
  $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases" -Headers $headers -Method Post -Body $bodyBytes -ContentType "application/json; charset=utf-8"
  Write-Host "==> 已创建 Release $tag" -ForegroundColor Green
}

# ── 7. 上传附件（同名覆盖） ─────────────────────────────────
$existing = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/$($release.id)/assets" -Headers $headers -Method Get
foreach ($file in $uploads) {
  $name = Split-Path $file -Leaf
  $old = $existing | Where-Object { $_.name -eq $name }
  if ($old) {
    Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/assets/$($old.id)" -Headers $headers -Method Delete | Out-Null
  }
  $uploadHeaders = @{
    Authorization  = "token $token"
    "User-Agent"   = "craftlingo-release"
    "Content-Type" = "application/octet-stream"
  }
  Invoke-RestMethod -Uri "https://uploads.github.com/repos/$Repo/releases/$($release.id)/assets?name=$name" `
    -Headers $uploadHeaders -Method Post -InFile $file | Out-Null
  Write-Host "==> 已上传 $name" -ForegroundColor Green
}

Write-Host ""
Write-Host "发布完成：$($release.html_url)" -ForegroundColor Green
Write-Host "客户端更新检查地址：https://github.com/$Repo/releases/latest/download/update.json"
