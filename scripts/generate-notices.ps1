#requires -Version 7.0
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$env:PATH = (Join-Path $env:USERPROFILE '.cargo/bin') + [IO.Path]::PathSeparator + $env:PATH
function Assert-LicenseTemplate([string]$Path, [string]$Id, [string]$ExpectedHash) {
    $actualHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($actualHash -eq $ExpectedHash) { return }
    # Latin1 round-trips every byte; only remove CR from CRLF pairs.
    $bytes = [IO.File]::ReadAllBytes($Path)
    $lfBytes = [Text.Encoding]::Latin1.GetBytes([Text.Encoding]::Latin1.GetString($bytes).Replace("`r`n", "`n"))
    $sha256 = [Security.Cryptography.SHA256]::Create()
    try { $lfHash = [BitConverter]::ToString($sha256.ComputeHash($lfBytes)).Replace('-', '') }
    finally { $sha256.Dispose() }
    if ($lfHash -eq $ExpectedHash) {
        throw "许可模板换行不一致：$Id（$Path）。文件含 CRLF，要求 LF；转换为 LF 后摘要匹配。请确认 .gitattributes 的 scripts/license-templates/*.txt text eol=lf 规则，并恢复模板的 LF 换行后重试；不能修改 sources.json 中的摘要来绕过校验。"
    }
    throw "许可模板摘要不一致：$Id（$Path）。预期 SHA-256：$ExpectedHash；实际：$actualHash。CRLF 转 LF 后仍不匹配，请核对模板内容及 sources.json 中的来源。"
}
# Only inspect ancestors inside a real Cargo workspace; never borrow a sibling
# registry crate's license or walk into a user/project parent directory.
function Get-CrateNotices($Package) {
    $dir = [IO.Path]::GetFullPath((Split-Path $Package.manifest_path -Parent))
    $roots = [Collections.Generic.List[string]]::new()
    $roots.Add($dir)
    $parent = Split-Path $dir -Parent
    while ($parent -and (Test-Path -LiteralPath (Join-Path $parent 'Cargo.toml') -PathType Leaf)) {
        $manifest = [IO.File]::ReadAllText((Join-Path $parent 'Cargo.toml'))
        if ($manifest -match '(?m)^\s*\[workspace(?:\.[^\]]+)?\]') { $roots.Add($parent) }
        $parent = Split-Path $parent -Parent
    }
    $files = @($roots | ForEach-Object {
        Get-ChildItem -LiteralPath $_ -File | Where-Object {
            $_.Name -match '^(LICENSE|LICENCE|COPYING|COPYRIGHT|NOTICE|UNLICENSE)([._-]|$)'
        }
    })
    if ($Package.license_file) {
        $extra = [IO.Path]::GetFullPath((Join-Path $dir $Package.license_file))
        $allowed = @($roots | Where-Object { $extra.StartsWith($_ + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) })
        if ($allowed.Count -and (Test-Path -LiteralPath $extra -PathType Leaf)) { $files += Get-Item -LiteralPath $extra }
    }
    $files = @($files | Sort-Object FullName -Unique)
    $notices = [Collections.Generic.List[object]]::new()
    foreach ($file in $files) {
        $notices.Add([pscustomobject]@{
            Name = $file.Name
            Source = '已安装 crate/workspace：' + [IO.Path]::GetRelativePath($dir, $file.FullName).Replace('\', '/')
            Text = [IO.File]::ReadAllText($file.FullName).TrimEnd()
        })
    }
    # SPDX metadata and NOTICE alone are not a license's full text.
    $hasText = @($files | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|UNLICENSE)([._-]|$)' -and $_.Extension -ne '.spdx' }).Count -gt 0
    if (-not $hasText -and $Package.license) {
        $expression = $Package.license.Trim()
        # Conservative grammar: no slash ambiguity, LicenseRef or WITH exceptions.
        if ($expression -match '^[A-Za-z0-9.()+ -]+$') {
            $ids = @($expression -split '\s+|[()]' | Where-Object { $_ -and $_ -notin @('OR', 'AND') } | Sort-Object -Unique)
            $sources = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'license-templates/sources.json') -Raw | ConvertFrom-Json -AsHashtable
            if ($ids.Count -and @($ids | Where-Object { -not $sources.ContainsKey($_) }).Count -eq 0) {
                foreach ($id in $ids) {
                    $template = Join-Path $PSScriptRoot "license-templates/$id.txt"
                    Assert-LicenseTemplate $template $id $sources[$id].sha256
                    $notices.Add([pscustomobject]@{
                        Name = "SPDX $id 标准全文"
                        Source = "[$id 标准模板]($($sources[$id].url))；依据此版本 Cargo license 字段；未选择 OR 分支。模板占位符照录，不代表该包原始版权声明。"
                        Text = [IO.File]::ReadAllText($template).TrimEnd()
                    })
                }
                $hasText = $true
            }
        }
    }
    [pscustomobject]@{ Notices = @($notices); HasText = $hasText }
}
Push-Location $repo
try {
    $hostInfo = & rustc.exe -vV
    if ($LASTEXITCODE -ne 0) { throw '无法读取 Rust 工具链。' }
    $target = ($hostInfo | Select-String '^host: (.+)$').Matches.Groups[1].Value
    if ($target -notmatch 'windows-msvc$') { throw '请使用 Windows MSVC 工具链生成本发布清单。' }
    $metadataArgs = @('metadata', '--manifest-path', 'src-tauri/Cargo.toml', '--format-version', '1', '--locked', '--filter-platform', $target)
    $raw = & cargo.exe @metadataArgs --offline
    $offline = $LASTEXITCODE -eq 0
    if (-not $offline) {
        Write-Warning '离线 metadata 失败，尝试使用锁定版本联网获取元数据。'
        $raw = & cargo.exe @metadataArgs
        if ($LASTEXITCODE -ne 0) { throw 'cargo metadata 失败，未更新清单。' }
    }
    $meta = ($raw -join "`n") | ConvertFrom-Json -Depth 100
    # tree uses the active target/features; metadata alone also includes unused lockfile packages.
    $treeArgs = @('tree', '--manifest-path', 'src-tauri/Cargo.toml', '--locked', '--target', $target, '--edges', 'normal,build', '--prefix', 'none', '--format', '{p}')
    if ($offline) { $treeArgs += '--offline' }
    $tree = & cargo.exe @treeArgs
    if ($LASTEXITCODE -ne 0) { throw 'cargo tree 失败，未更新清单。' }
    $compiled = @{}
    foreach ($line in $tree) {
        if ($line -match '^(\S+) v(\S+)') { $compiled["$($Matches[1])@$($Matches[2])"] = $true }
    }
    $rows = [Collections.Generic.List[object]]::new()
    $texts = [Collections.Generic.List[string]]::new()
    foreach ($pkg in ($meta.packages | Sort-Object name, version)) {
        if ($pkg.id -eq $meta.resolve.root -or -not $compiled.ContainsKey("$($pkg.name)@$($pkg.version)")) { continue }
        $license = if ($pkg.license) { $pkg.license } else { '待人工核对（无 license 字段）' }
        $collected = Get-CrateNotices $pkg
        $rows.Add([pscustomobject]@{ Ecosystem = 'Rust'; Name = $pkg.name; Version = $pkg.version; License = $license; Repository = $pkg.repository; NoticeCount = $(if ($collected.HasText) { 1 } else { 0 }) })
        foreach ($notice in $collected.Notices) {
            $texts.Add("### Rust / $($pkg.name) $($pkg.version) / $($notice.Name)`n`n来源：$($notice.Source)`n`n~~~~text`n$($notice.Text)`n~~~~")
        }
    }
    $frontRaw = & node.exe scripts/collect-frontend-notices.mjs
    if ($LASTEXITCODE -ne 0) { throw '前端许可收集失败，未更新清单。' }
    $front = ($frontRaw -join "`n") | ConvertFrom-Json -Depth 100
    foreach ($pkg in $front.packages) {
        $rows.Add([pscustomobject]@{ Ecosystem = $pkg.scope; Name = $pkg.name; Version = $pkg.version; License = $(if ($pkg.license) { $pkg.license } else { '待人工核对（无 license 字段）' }); Repository = $pkg.repository; NoticeCount = @($pkg.notices).Count })
        foreach ($notice in $pkg.notices) { $texts.Add("### 前端 / $($pkg.name) $($pkg.version) / $($notice.name)`n`n~~~~text`n$($notice.text.TrimEnd())`n~~~~") }
    }
    $output = Join-Path $repo 'THIRD_PARTY_NOTICES.md'
    $old = [IO.File]::ReadAllText($output)
    $marker = '<!-- GENERATED-DEPENDENCIES -->'
    $upstream = ($old -split [regex]::Escape($marker), 2)[0].TrimEnd()
    # Retain all original upstream text; embed MIT so the portable file is self-contained.
    if ($upstream -notmatch 'Copyright \(c\) 2024 xdl contributors') {
        throw 'THIRD_PARTY_NOTICES.md 缺少嵌入的上游 MIT 声明；请先恢复该声明。'
    }
    $lines = [Collections.Generic.List[string]]::new()
    $lines.Add($upstream)
    $lines.Add("`n$marker`n`n## 新版依赖清单`n")
    $lines.Add("由 scripts/generate-notices.ps1 从锁定依赖、本地许可文件及带来源的 SPDX 标准模板生成。Rust 目标：$target，默认 release 功能；用 metadata 与 cargo tree 的 normal/build 边交叉筛选，排除 dev/test 和其他平台依赖。含构建脚本与过程宏所需 crate；编译参与不等于所有代码都链接进 EXE。")
    $lines.Add('前端按 Vite 产物实际保留的模块筛选 package-lock.json 与 node_modules；生产依赖沿 dependencies 解析，不包含开发工具。若开发声明中的包有运行代码实际进入产物（如 Svelte runtime），单独标注为“前端运行时补充”。许可表达式按原字段分组，不替发布者选择 OR 分支。')
    $lines.Add('上游 MIT 全文已嵌入本文件。下列许可文本优先来自已安装包及其 workspace；缺失时回退 scripts/license-templates 中有固定来源及 SHA-256 的标准全文。模板占位符不代表该包原始版权声明，署名和上游附件仍需发布者核对。原生内嵌组件（如 libsqlite3-sys 的 SQLite）及许可附件完整性仍需在正式发布前人工复核，新版自身许可见根目录 LICENSE。')
    $missing = @($rows | Where-Object License -like '待人工核对*')
    $missingText = @($rows | Where-Object NoticeCount -eq 0)
    $lines.Add("`n### 需人工核对`n")
    $lines.Add("无 license 字段：$($missing.Count) 项。crate/workspace、license_file 及 SPDX 模板回退后仍未找到许可全文：$($missingText.Count) 项（不表示这些包没有许可）。")
    foreach ($row in ($rows | Sort-Object Ecosystem, Name, Version)) {
        $reasons = @()
        if ($row.License -like '待人工核对*') { $reasons += '无 license 字段' }
        if ($row.NoticeCount -eq 0) { $reasons += '未找到许可全文，需补核上游声明' }
        if ($row.License -match 'MPL-2\.0') { $reasons += '复核 MPL-2.0 对发布内容的要求' }
        if ($row.License -match ' AND ') { $reasons += '复核组合许可及声明完整性' }
        if ($row.License -match '/') { $reasons += '原字段使用斜杠，需人工确认许可表达式含义' }
        if ($row.Name -eq 'libsqlite3-sys') { $reasons += '复核内嵌 SQLite 原生源码声明，crate 字段不替代原生组件声明' }
        if ($reasons.Count) { $lines.Add("- $($row.Name) $($row.Version)：$($reasons -join '；')。") }
    }
    $lines.Add('可选许可 OR 分支及 Apache NOTICE 的最终保留范围由发布者核对；本脚本不自动选择许可分支。')
    foreach ($group in ($rows | Group-Object License | Sort-Object Name)) {
        $lines.Add("`n### $($group.Name)`n`n| 来源 | 名称 | 版本 | license 字段 | 仓库 |`n|---|---|---|---|---|")
        foreach ($row in ($group.Group | Sort-Object Ecosystem, Name, Version)) {
            $url = if ($row.Repository) { $row.Repository } else { '未提供' }
            $lines.Add("| $($row.Ecosystem) | $($row.Name) | $($row.Version) | $($row.License.Replace('|', '\|')) | $url |")
        }
    }
    $lines.Add("`n## 已安装依赖的许可与声明全文`n")
    foreach ($notice in $texts) { $lines.Add("`n$notice`n") }
    # Normalize line endings and trailing layout whitespace, preserving notice wording.
    $document = ($lines -join "`n").Replace("`r`n", "`n")
    $document = [regex]::Replace($document, '(?m)[\t ]+$', '').TrimEnd() + "`n"
    [IO.File]::WriteAllText($output, $document, [Text.UTF8Encoding]::new($false))
    Write-Host "已生成 THIRD_PARTY_NOTICES.md：Rust $(@($rows | Where-Object Ecosystem -eq 'Rust').Count) 项，前端 $($front.packages.Count) 项；无 license 字段 $($missing.Count) 项。"
    Write-Host "未找到许可全文：$($missingText.Count) 项；详见清单的“需人工核对”。"
    $missing | ForEach-Object { Write-Warning "$($_.Ecosystem) $($_.Name) $($_.Version)：需人工核对" }
} finally { Pop-Location }
