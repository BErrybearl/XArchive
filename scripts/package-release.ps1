#requires -Version 7.0
[CmdletBinding()]
param(
    [string]$OutputDirectory = '.local/release',
    [switch]$SkipBuild,
    [switch]$Zip = $true,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))
$env:PATH = (Join-Path $env:USERPROFILE '.cargo/bin') + [IO.Path]::PathSeparator + $env:PATH
function Assert-LocalPath([string]$Path) {
    $full = [IO.Path]::GetFullPath($Path)
    if (-not $full.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "发布路径必须位于本 worktree 内：$full"
    }
    $cursor = $full
    while ($cursor -and $cursor -ne $repo) {
        if (Test-Path -LiteralPath $cursor) {
            if ((Get-Item -LiteralPath $cursor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "拒绝链接或重解析路径：$cursor"
            }
        }
        $cursor = Split-Path $cursor -Parent
    }
    return $full
}
function Get-PeInfo([string]$Path) {
    $stream = [IO.File]::OpenRead($Path)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5a4d) { throw 'EXE 没有有效 MZ 头。' }
        $stream.Position = 0x3c
        $offset = $reader.ReadUInt32()
        if ($offset + 94 -gt $stream.Length) { throw 'EXE 的 PE 头越界。' }
        $stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x00004550) { throw 'EXE 没有有效 PE 签名。' }
        $machine = $reader.ReadUInt16()
        $stream.Position = $offset + 24
        $magic = $reader.ReadUInt16()
        if ($magic -notin 0x10b, 0x20b) { throw 'EXE 可选头格式不受支持。' }
        $stream.Position = $offset + 24 + 68
        $subsystem = $reader.ReadUInt16()
        if ($subsystem -ne 2) { throw "EXE 不是 Windows GUI 子系统（实际 $subsystem）。" }
        return [pscustomobject]@{ Subsystem = $subsystem; Machine = ('0x{0:x4}' -f $machine) }
    } finally { $reader.Dispose(); $stream.Dispose() }
}
Push-Location $repo
$stage = $null
$zipStage = $null
try {
    $config = Get-Content -LiteralPath 'src-tauri/tauri.conf.json' -Raw -Encoding utf8 | ConvertFrom-Json
    $manifest = Get-Content -LiteralPath 'src-tauri/Cargo.toml' -Raw -Encoding utf8
    $section = [regex]::Match($manifest, '(?ms)^\[package\]\s*\r?\n(.*?)(?=^\[|\z)').Groups[1].Value
    $version = [regex]::Match($section, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
    $name = [regex]::Match($section, '(?m)^name\s*=\s*"([^"]+)"').Groups[1].Value
    if (-not $version -or $version -ne $config.version -or $version -notmatch '^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$') {
        throw 'Cargo.toml 与 tauri.conf.json 版本必须一致且为有效版本号。'
    }
    if ($env:CARGO_TARGET_DIR -or $env:CARGO_BUILD_TARGET) { throw '请清除 CARGO_TARGET_DIR/CARGO_BUILD_TARGET 后按仓库默认目标打包。' }
    $out = Assert-LocalPath $(if ([IO.Path]::IsPathRooted($OutputDirectory)) { $OutputDirectory } else { Join-Path $repo $OutputDirectory })
    $destination = Assert-LocalPath (Join-Path $out "XArchive-$version")
    $zipPath = Assert-LocalPath "$destination.zip"
    $singleExe = Assert-LocalPath (Join-Path $out "XArchive-$version.exe")
    $rootSums = Assert-LocalPath (Join-Path $out 'SHA256SUMS.txt')
    foreach ($path in @($singleExe, $rootSums, $zipPath)) {
        if ((Test-Path -LiteralPath $path) -and -not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "发布文件路径不是普通文件：$path" }
    }
    $allowed = @('XArchive.exe', '使用说明.md', 'THIRD_PARTY_NOTICES.md', 'LICENSE', 'SHA256SUMS.txt')
    if ((Test-Path -LiteralPath $destination) -or (Test-Path -LiteralPath $zipPath) -or (Test-Path -LiteralPath $singleExe)) {
        if (-not $Force) { throw '同版本输出已存在；拒绝覆盖。如确需重新打包，请使用 -Force。' }
        if (Test-Path -LiteralPath $destination) {
            if (-not (Test-Path -LiteralPath $destination -PathType Container)) { throw '发布目录被同名文件占用。' }
            foreach ($item in (Get-ChildItem -LiteralPath $destination -Force)) {
                if ($item.PSIsContainer -or $item.Name -notin $allowed -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                    throw "同版本目录包含非发布文件，-Force 也拒绝删除：$($item.Name)"
                }
            }
        }
        if ((Test-Path -LiteralPath $zipPath) -and -not (Test-Path -LiteralPath $zipPath -PathType Leaf)) { throw 'zip 路径不是普通文件。' }
    }
    # Generate before compiling: the EXE embeds this exact checkout's notices.
    & (Join-Path $PSScriptRoot 'generate-notices.ps1')
    if (-not $SkipBuild) {
        & npm.cmd run tauri:build -- -- --locked
        if ($LASTEXITCODE -ne 0) { throw 'release 构建失败，未生成发布包。' }
    } else { Write-Host '跳过构建：调用者须已在本 worktree 成功运行 npm.cmd run tauri:build -- -- --locked。' }
    $metaRaw = & cargo.exe metadata --manifest-path src-tauri/Cargo.toml --format-version 1 --no-deps --locked --offline
    if ($LASTEXITCODE -ne 0) { throw '无法读取构建产物目录。' }
    $meta = ($metaRaw -join "`n") | ConvertFrom-Json
    $exe = Assert-LocalPath (Join-Path $meta.target_directory "release/$name.exe")
    if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) { throw "找不到 release EXE：$exe" }
    $pe = Get-PeInfo $exe
    New-Item -ItemType Directory -Path $out -Force | Out-Null
    $stage = Assert-LocalPath (Join-Path $out ('.package-' + [guid]::NewGuid().ToString('N')))
    New-Item -ItemType Directory -Path $stage | Out-Null
    Copy-Item -LiteralPath $exe -Destination (Join-Path $stage 'XArchive.exe')
    foreach ($file in @('使用说明.md', 'THIRD_PARTY_NOTICES.md')) {
        Copy-Item -LiteralPath (Join-Path $repo $file) -Destination (Join-Path $stage $file)
    }
    $projectLicense = if (Test-Path -LiteralPath (Join-Path $repo 'LICENSE')) { Join-Path $repo 'LICENSE' } else { Join-Path $repo 'src-tauri/licenses/MIT.txt' }
    Copy-Item -LiteralPath $projectLicense -Destination (Join-Path $stage 'LICENSE')
    $sums = foreach ($file in $allowed | Where-Object { $_ -ne 'SHA256SUMS.txt' }) {
        $hash = (Get-FileHash -LiteralPath (Join-Path $stage $file) -Algorithm SHA256).Hash.ToLowerInvariant()
        "$hash  $file"
    }
    [IO.File]::WriteAllText((Join-Path $stage 'SHA256SUMS.txt'), ($sums -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
    # Only replace a directory that was verified to contain our allowlisted files.
    if (Test-Path -LiteralPath $destination) {
        Assert-LocalPath $destination | Out-Null
        $existingFiles = @(Get-ChildItem -LiteralPath $destination -Force)
        foreach ($item in $existingFiles) {
            if ($item.PSIsContainer -or $item.Name -notin $allowed -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '输出目录在打包期间发生变化，拒绝覆盖。' }
        }
        foreach ($item in $existingFiles) { Remove-Item -LiteralPath $item.FullName }
        Remove-Item -LiteralPath $destination
    }
    Move-Item -LiteralPath $stage -Destination $destination
    $stage = $null
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath (Assert-LocalPath $zipPath) }
    if ($Zip) {
        $zipStage = Assert-LocalPath (Join-Path $out ('.package-' + [guid]::NewGuid().ToString('N') + '.zip'))
        Compress-Archive -LiteralPath $destination -DestinationPath $zipStage
        Move-Item -LiteralPath $zipStage -Destination $zipPath
        $zipStage = $null
    }
    $finalExe = Get-Item -LiteralPath (Join-Path $destination 'XArchive.exe')
    $finalHash = (Get-FileHash -LiteralPath $finalExe.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    Copy-Item -LiteralPath $finalExe.FullName -Destination (Assert-LocalPath $singleExe) -Force
    if ((Get-FileHash -LiteralPath $singleExe -Algorithm SHA256).Hash.ToLowerInvariant() -ne $finalHash) { throw '单 EXE 副本摘要不一致。' }
    # Retain checksum entries for other versions already present in this release directory.
    $releaseSums = foreach ($item in (Get-ChildItem -LiteralPath $out -File | Where-Object { $_.Name -match '^XArchive-\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?\.(exe|zip)$' } | Sort-Object Name)) {
        Assert-LocalPath $item.FullName | Out-Null
        "$((Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $($item.Name)"
    }
    [IO.File]::WriteAllText($rootSums, ($releaseSums -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
    Write-Host "主产物（单 EXE）：$singleExe"
    Write-Host "发布目录：$destination"
    Write-Host "XArchive.exe：$($finalExe.Length) 字节；Windows GUI 子系统 $($pe.Subsystem)；Machine $($pe.Machine)"
    Write-Host "SHA-256：$finalHash"
    Write-Host "主产物摘要清单：$rootSums"
    Write-Host '启动前置条件：匹配 EXE 架构的 Windows 10/11、WebView2 Runtime；浏览器登录需 Edge 或 Chrome；程序及数据目录可写。'
    if ($Zip) { Write-Host "ZIP：$zipPath" }
} finally {
    if ($stage -and (Test-Path -LiteralPath $stage)) {
        Assert-LocalPath $stage | Out-Null
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
    if ($zipStage -and (Test-Path -LiteralPath $zipStage)) { Remove-Item -LiteralPath (Assert-LocalPath $zipStage) }
    Pop-Location
}
