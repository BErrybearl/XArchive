#requires -Version 7.0
[CmdletBinding()]
param(
    [string]$Output = '.local/public/XArchive',
    [Parameter(Mandatory)][ValidateNotNullOrEmpty()][string]$CopyrightHolder,
    [string]$AuthorName,
    [string]$AuthorEmail,
    [switch]$Force,
    [switch]$SkipVerify,
    [switch]$AllowDpapiSandboxFailures
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))
$local = Join-Path $repo '.local'
$destination = [IO.Path]::GetFullPath($(if ([IO.Path]::IsPathRooted($Output)) { $Output } else { Join-Path $repo $Output }))

function Assert-ExportPath([string]$Path) {
    # Export and replacement are restricted to isolated local state in this checkout.
    if (-not $Path.StartsWith($local + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw '输出必须位于当前 worktree 的 .local/ 子目录内。'
    }
    $probe = $Path
    while ($probe -ne $repo) {
        if ((Test-Path -LiteralPath $probe) -and
            ((Get-Item -LiteralPath $probe -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw '拒绝链接或重解析输出路径。' }
        $probe = Split-Path $probe -Parent
    }
}
function Assert-SourcePath([string]$Path) {
    if (-not $Path.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw '源文件越界。' }
    $probe = $Path
    while ($probe -ne $repo) {
        if ((Get-Item -LiteralPath $probe -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '源文件路径含链接或重解析点。' }
        $probe = Split-Path $probe -Parent
    }
}
function Get-SourceFiles([string]$Directory) {
    Assert-SourcePath $Directory
    foreach ($item in (Get-ChildItem -LiteralPath $Directory -Force)) {
        Assert-SourcePath $item.FullName
        if ($item.PSIsContainer) {
            if ($item.Name -in @('.git', '.local', '.runtime', 'node_modules', 'target', 'dist')) { continue }
            Get-SourceFiles $item.FullName
        } else { $item }
    }
}
function Get-AllExportFiles([string]$Directory) {
    foreach ($item in (Get-ChildItem -LiteralPath $Directory -Force)) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '已有导出含链接，拒绝替换。' }
        if ($item.PSIsContainer) { Get-AllExportFiles $item.FullName } else { $item }
    }
}
function Invoke-ExportGit([string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new('git.exe')
    $info.WorkingDirectory = $destination
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    # Avoid inherited worktree paths, identities, signing, hooks and Git templates.
    foreach ($key in @($info.Environment.Keys | Where-Object { $_ -like 'GIT_*' })) { [void]$info.Environment.Remove($key) }
    $info.Environment['GIT_CONFIG_NOSYSTEM'] = '1'
    $info.Environment['GIT_CONFIG_GLOBAL'] = $emptyConfig
    $info.Environment['GIT_TERMINAL_PROMPT'] = '0'
    foreach ($arg in (@('-c', ('core.hooksPath=' + $emptyTemplate), '-c', 'commit.gpgSign=false', '-c', 'core.autocrlf=false', '-c', 'core.safecrlf=false') + $Arguments)) { $info.ArgumentList.Add($arg) }
    $process = [Diagnostics.Process]::Start($info)
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $result = $stdout.GetAwaiter().GetResult()
    $null = $stderr.GetAwaiter().GetResult()
    $code = $process.ExitCode
    $process.Dispose()
    if ($code -ne 0) { throw "导出仓库的 Git 操作失败，退出码 $code。" }
    $result.Trim()
}

Assert-ExportPath $destination
if ($CopyrightHolder -match '[\r\n<>\x00]' -or [string]::IsNullOrWhiteSpace($CopyrightHolder)) { throw '版权人须为非空的单行名称。' }
if ($CopyrightHolder -cne 'BErrybearl') { throw '本项目版权人固定为 BErrybearl。' }
if ($AuthorName -match '[\r\n<>\x00]' -or $AuthorEmail -match '[\r\n<>\x00]') { throw '作者信息含非法字符。' }
if ($AuthorName -and $AuthorName -cne 'BErrybearl') { throw '本项目作者名固定为 BErrybearl。' }
if ($AuthorEmail -and $AuthorEmail -notmatch '^[A-Za-z0-9][A-Za-z0-9+._-]*@users\.noreply\.github\.com$') { throw '作者邮箱必须使用 GitHub noreply 地址（以 @users.noreply.github.com 结尾）。' }
$commit = -not [string]::IsNullOrWhiteSpace($AuthorName) -and -not [string]::IsNullOrWhiteSpace($AuthorEmail)

# Enumerate an extension allowlist in narrow source directories, never copy whole trees.
$copies = [ordered]@{}
foreach ($spec in @(
    @{ Directory = 'src'; Extensions = @('.ts', '.svelte', '.css') },
    @{ Directory = 'src-tauri/src'; Extensions = @('.rs', '.sql') },
    @{ Directory = 'src-tauri/tests'; Extensions = @('.rs', '.sql', '.json', '.html', '.js', '.md') },
    @{ Directory = 'src-tauri/licenses'; Extensions = @('.txt') },
    @{ Directory = 'src-tauri/src/platform/fixtures'; Extensions = @('.json', '.html', '.js') },
    @{ Directory = 'src-tauri/capabilities'; Extensions = @('.json') },
    @{ Directory = 'src-tauri/icons'; Extensions = @('.png', '.ico', '.icns') },
    @{ Directory = 'checks'; Extensions = @('.mjs') },
    @{ Directory = 'scripts/license-templates'; Extensions = @('.txt', '.json', '.md') }
)) {
    foreach ($file in @(Get-SourceFiles (Join-Path $repo $spec.Directory))) {
        if ($file.Extension -notin $spec.Extensions) { continue }
        $relative = [IO.Path]::GetRelativePath($repo, $file.FullName).Replace('\', '/')
        $copies[$relative] = $file.FullName
    }
}
foreach ($relative in @(
    'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock', 'src-tauri/build.rs', 'src-tauri/tauri.conf.json',
    'src-tauri/src/platform/defaults.json', 'package.json', 'package-lock.json', 'tsconfig.json',
    'vite.config.ts', 'svelte.config.js', 'index.html', 'THIRD_PARTY_NOTICES.md', '.gitignore', '.gitattributes',
    'scripts/package-release.ps1', 'scripts/generate-notices.ps1', 'scripts/collect-frontend-notices.mjs',
    'scripts/check-environment.mjs', 'scripts/check-commands.mjs', 'scripts/scan-history.ps1',
    'scripts/sensitive-rules.ps1', 'scripts/scan-public-tree.ps1', 'scripts/public-scan-policy.json', 'scripts/prepare-public.ps1',
    'scripts/check-public-references.mjs', 'scripts/verify-public-export.ps1'
)) {
    $source = Join-Path $repo $relative
    Assert-SourcePath $source
    $copies[$relative] = $source
}
foreach ($name in @('README.md', 'README.en.md', 'LICENSE', 'SECURITY.md', 'CONTRIBUTING.md', '使用说明.md')) {
    $source = Join-Path $repo "public/$name"
    # An exported repository can export its own root documents again.
    if (-not (Test-Path -LiteralPath $source)) { $source = Join-Path $repo $name }
    Assert-SourcePath $source
    $copies[$name] = $source
}
$screenshot = Join-Path $repo 'docs/screenshot-overview.png'
if (Test-Path -LiteralPath $screenshot) {
    Assert-SourcePath $screenshot
    $copies['docs/screenshot-overview.png'] = $screenshot
}

$stateDirectory = Join-Path $local 'public-export-state'
if ($destination -eq $stateDirectory -or $destination.StartsWith($stateDirectory + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
    $stateDirectory.StartsWith($destination + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw '输出不得与导出记录目录重叠。' }
Assert-ExportPath $stateDirectory
[void][IO.Directory]::CreateDirectory($stateDirectory)
$keyBytes = [Text.Encoding]::UTF8.GetBytes($destination.ToUpperInvariant())
$key = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($keyBytes)).Substring(0, 24)
$receiptPath = Join-Path $stateDirectory "$key.json"
$reportPath = Join-Path $stateDirectory "$key.scan.json"
Assert-ExportPath $receiptPath
Assert-ExportPath $reportPath
$emptyTemplate = Join-Path $stateDirectory 'empty-git-template'
$emptyConfig = Join-Path $stateDirectory 'empty-git-config'
Assert-ExportPath $emptyTemplate
Assert-ExportPath $emptyConfig
[void][IO.Directory]::CreateDirectory($emptyTemplate)
if (@(Get-ChildItem -LiteralPath $emptyTemplate -Force).Count) { throw 'Git 空模板目录被占用。' }
if (Test-Path -LiteralPath $emptyConfig) {
    if ((Get-Item -LiteralPath $emptyConfig).Length -ne 0) { throw 'Git 空配置文件被占用。' }
} else { [IO.File]::WriteAllText($emptyConfig, '') }

if (Test-Path -LiteralPath $destination) {
    if (-not $Force) { throw '输出已存在；使用 -Force 重建尚未修改的脚本导出。' }
    if (-not (Test-Path -LiteralPath $receiptPath -PathType Leaf)) { throw '-Force 只允许替换本脚本记录的导出。' }
    $receipt = Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json -AsHashtable
    if ($receipt.destination -ne $destination) { throw '导出记录路径不符。' }
    $previousFiles = @(Get-AllExportFiles $destination)
    foreach ($file in $previousFiles) {
        $relative = [IO.Path]::GetRelativePath($destination, $file.FullName).Replace('\', '/')
        if ($relative.StartsWith('.git/')) { continue }
        if (-not $receipt.files.ContainsKey($relative) -or
            (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash -ne $receipt.files[$relative]) {
            throw '-Force 拒绝删除已修改或额外文件（包括依赖、构建结果和运行数据）。'
        }
    }
    $gitPath = Join-Path $destination '.git'
    if (Test-Path -LiteralPath $gitPath) {
        if (-not (Test-Path -LiteralPath $gitPath -PathType Container)) { throw '拒绝替换 git worktree。' }
        if ((Invoke-ExportGit @('remote')).Length -gt 0 -or (Invoke-ExportGit @('rev-list', '--all', '--count')) -ne '1') {
            throw '已有导出包含 remote 或额外历史，拒绝替换。'
        }
    }
    # All ancestors, file contents and absolute containment were checked above.
    Assert-ExportPath $destination
    Remove-Item -LiteralPath $destination -Recurse -Force
}
[void][IO.Directory]::CreateDirectory($destination)
$receipt = @{ destination = $destination; files = @{} }
foreach ($relative in $copies.Keys) {
    $target = Join-Path $destination $relative
    [void][IO.Directory]::CreateDirectory((Split-Path $target -Parent))
    Copy-Item -LiteralPath $copies[$relative] -Destination $target
    if ($relative -eq 'LICENSE') {
        $text = [IO.File]::ReadAllText($target).Replace('<COPYRIGHT_HOLDER>', $CopyrightHolder)
        [IO.File]::WriteAllText($target, $text, [Text.UTF8Encoding]::new($false))
    }
    $receipt.files[$relative] = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash
}
[IO.File]::WriteAllText($receiptPath, ($receipt | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
. (Join-Path $PSScriptRoot 'scan-public-tree.ps1')
$scan = Invoke-PublicTreeScan $destination
[IO.File]::WriteAllText($reportPath, ($scan | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
$files = @(Get-PublicFiles $destination)
$size = ($files | Measure-Object -Property Length -Sum).Sum
Write-Host "文件：$($files.Count)；总大小：$size 字节。"
foreach ($rule in $scan.Counts.Keys) { Write-Host "$rule : $($scan.Counts[$rule])" }
Write-Host "已核对的非敏感匹配：$($scan.Reviewed.Count)；扫描报告位于 .local/public-export-state/。"
Write-Host "上游许可邮箱例外：$($scan.UpstreamEmailExceptions.Count) 处（文件、行号与精确邮箱见扫描报告）。"
if (-not $scan.Passed) {
    foreach ($finding in $scan.Findings) { Write-Host "$($finding.File):$($finding.Line) [$($finding.Rule)]" }
    throw '公开扫描未通过；保留文件树供修正，未初始化 Git。'
}
& node (Join-Path $destination 'scripts/check-public-references.mjs') $destination
if ($LASTEXITCODE -ne 0) { throw '导出源码引用检查失败；未提交。' }
$verification = 'Skipped'
if (-not $SkipVerify) {
    . (Join-Path $PSScriptRoot 'verify-public-export.ps1')
    $verification = Invoke-PublicExportVerification $destination $stateDirectory $key -AllowDpapiSandboxFailures:$AllowDpapiSandboxFailures
    # Build tools may create files: scan again before committing.
    $scan = Invoke-PublicTreeScan $destination
    [IO.File]::WriteAllText($reportPath, ($scan | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
    if (-not $scan.Passed) { throw '验证后公开扫描失败；未提交。' }
}
$hash = $null
if ($commit) {
    $null = Invoke-ExportGit @('init', '--initial-branch=main', "--template=$emptyTemplate", '.')
    $null = Invoke-ExportGit @('config', '--local', 'user.name', $AuthorName)
    $null = Invoke-ExportGit @('config', '--local', 'user.email', $AuthorEmail)
    $null = Invoke-ExportGit @('add', '--all')
    $null = Invoke-ExportGit @('commit', '-m', 'Initial public release')
    $hash = Invoke-ExportGit @('rev-parse', 'HEAD')
    if ((Invoke-ExportGit @('rev-list', '--all', '--count')) -ne '1' -or (Invoke-ExportGit @('remote')).Length -gt 0) { throw '导出仓库历史或 remote 验证失败。' }
    Write-Host "提交：$hash"
} else { Write-Host '未同时提供作者姓名和邮箱：仅生成文件树，未初始化或提交 Git。' }
[pscustomobject]@{ Files = $files.Count; Bytes = $size; ScanPassed = $scan.Passed; ReferencesPassed = $true; Verification = $verification; Commit = $hash }
