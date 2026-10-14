#requires -Version 7.0
[CmdletBinding()]
param(
    # Exact owner/repo entries only. No network lookup is performed.
    [string[]]$PublicRepositoryAllowlist = @(),
    [string]$OutputPath = '.local/scan/history-report.md'
)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Split-Path $PSScriptRoot -Parent))
$output = [IO.Path]::GetFullPath((Join-Path $repo $OutputPath))
if (-not $output.StartsWith((Join-Path $repo '.local') + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
    throw '报告必须写入本 worktree 的 .local/ 内。'
}
# Reject filesystem links before accessing tracked paths or creating the report.
function Assert-LocalPath([string]$Path) {
    if (-not $Path.StartsWith($repo + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) { throw '路径越出本 worktree。' }
    $probe = $Path
    while ($probe -and $probe -ne $repo) {
        if (Test-Path -LiteralPath $probe) {
            if ((Get-Item -LiteralPath $probe -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '拒绝读取或写入重解析路径。' }
        }
        $probe = Split-Path $probe -Parent
    }
}
Assert-LocalPath $output
$public = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($entry in $PublicRepositoryAllowlist) {
    if ($entry -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$') { throw '公开仓库白名单须使用 owner/repo 格式。' }
    [void]$public.Add(($entry -replace '\.git$', ''))
}

function Start-Git([string[]]$Arguments) {
    $info = [Diagnostics.ProcessStartInfo]::new('git.exe')
    $info.WorkingDirectory = $repo
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.StandardOutputEncoding = [Text.UTF8Encoding]::new($false)
    foreach ($arg in (@('--no-pager', '-c', 'core.quotePath=false', '-c', 'color.ui=false') + $Arguments)) { $info.ArgumentList.Add($arg) }
    $process = [Diagnostics.Process]::Start($info)
    # Drain stderr asynchronously to avoid pipe deadlock. Never echo raw Git output.
    [pscustomobject]@{ Process = $process; ErrorTask = $process.StandardError.ReadToEndAsync() }
}
function Complete-Git($Run) {
    $Run.Process.WaitForExit()
    $null = $Run.ErrorTask.GetAwaiter().GetResult()
    $code = $Run.Process.ExitCode
    $Run.Process.Dispose()
    if ($code -ne 0) { throw "Git 扫描命令失败（退出码 $code）；报告未更新。" }
}
function Read-Git([string[]]$Arguments) {
    $run = Start-Git $Arguments
    $result = $run.Process.StandardOutput.ReadToEnd()
    Complete-Git $run
    $result
}
function Escape-Cell([string]$Value) {
    $Value.Replace('&', '&amp;').Replace('<', '&lt;').Replace('>', '&gt;').Replace('|', '&#124;').Replace('`', '&#96;').Replace("`r", ' ').Replace("`n", ' ')
}
function Decode-GitPath([string]$Value) {
    if ($Value.StartsWith('"')) {
        # Git's quoted paths can contain C escapes and octal UTF-8 bytes.
        $inner = $Value.Substring(1, $Value.Length - 2)
        $bytes = [Collections.Generic.List[byte]]::new()
        for ($i = 0; $i -lt $inner.Length; $i++) {
            if ($inner[$i] -eq '\' -and $i + 1 -lt $inner.Length) {
                $i++
                if ($inner.Substring($i) -match '^([0-7]{3})') {
                    $bytes.Add([Convert]::ToByte($Matches[1], 8)); $i += 2; continue
                }
                $ch = switch ($inner[$i]) { 't' { "`t" }; 'n' { "`n" }; 'r' { "`r" }; default { [string]$inner[$i] } }
                $bytes.AddRange([Text.Encoding]::UTF8.GetBytes($ch))
            } else { $bytes.AddRange([Text.Encoding]::UTF8.GetBytes([string]$inner[$i])) }
        }
        $Value = [Text.Encoding]::UTF8.GetString($bytes.ToArray())
    }
    if ($Value -eq '/dev/null') { return $null }
    $Value -replace '^[ab]/', ''
}

. (Join-Path $PSScriptRoot 'sensitive-rules.ps1')
$rules = @(Get-SensitiveRules)
foreach ($rule in $rules) { $rule.Regex = [regex]::new($rule.Pattern, [Text.RegularExpressions.RegexOptions]::None, [TimeSpan]::FromSeconds(2)) }
$findings = [Collections.Generic.List[object]]::new()
$seen = [Collections.Generic.HashSet[string]]::new()
function Scan-Line([string]$Text, [string]$Commit, [string]$File, [int]$Number, [string]$Side) {
    foreach ($rule in $rules) {
        foreach ($match in $rule.Regex.Matches($Text)) {
            $value = $match.Groups['value'].Value
            if ($rule.Name -eq '未列入公开白名单的 GitHub 链接' -and $public.Contains(($value.TrimEnd('.') -replace '\.git$', ''))) { continue }
            if ($rule.Name -eq 'IP:端口代理地址') {
                $address = $value -replace ':\d+$', ''
                $parsed = $null
                if (-not [Net.IPAddress]::TryParse($address.Trim('[', ']'), [ref]$parsed)) { continue }
                $port = [int]($value -replace '^.*:', '')
                if ($port -gt 65535) { continue }
            }
            # Same merge diff can appear for multiple parents; keep each location once.
            $key = "$Commit`0$File`0$Number`0$Side`0$($rule.Name)`0$($match.Index)`0$value"
            if (-not $seen.Add($key)) { continue }
            $suggestion = '需处理'
            $reason = '需人工确认并清理公开内容'
            if ($rule.Secret) {
                $snippet = $value.Substring(0, [Math]::Min(4, $value.Length)) + "…（长度 $($value.Length)）"
                $reason = '凭据候选；如真实有效应撤销或轮换，再由负责人决定历史处理'
                if ($rule.Name -eq 'X 会话凭据' -and $value -match '^([0-9a-fA-F])\1+$' -and ($File -match '(?i)(test|fixture|src-tauri/src/)' -or $Text -match '(?i)(test|example|dummy|fake|synthetic)')) {
                    $suggestion = '可接受（待确认）'; $reason = '重复字符合成值，仍须核对测试上下文'
                } elseif ($rule.Name -eq 'Cookie 头' -and $value -match '^f["''].*\{[^{}\r\n]+\}' -and ($value -replace '\{[^{}\r\n]+\}', '') -notmatch '[A-Za-z0-9_%+-]{24,}') {
                    $suggestion = '可接受（待确认）'; $reason = '疑似字符串插值代码，仍须确认没有混入凭据字面值'
                } elseif ($rule.Name -eq 'Cookie 头' -and $value -notmatch '(?i)(auth_token|ct0|session|token)\s*=|[A-Za-z0-9_-]{24,}') {
                    $suggestion = '可接受（待确认）'; $reason = '疑似头名称、变量或短示例；仍须人工确认'
                }
            } else {
                switch ($rule.Name) {
                    '邮箱地址' {
                        $parts = $value.Split('@'); $snippet = $parts[0].Substring(0, 1) + '***@' + $parts[1]
                        if ($value -match '(?i)@(?:[^@]+\.)?(example\.(com|org|net)|test|invalid)$') { $suggestion = '可接受（待确认）'; $reason = '保留的示例域名' }
                        elseif ($File -match '(?i)(LICENSE|LICENCE|COPYING|NOTICE|license-templates/)') { $suggestion = '可接受（待确认）'; $reason = '疑似上游版权联系信息，应保留合法署名' }
                        elseif ($File -eq '[提交作者]' -or $File -eq '[提交者]') { $reason = '公开前确认个人邮箱；可由负责人决定替换为 noreply 地址' }
                    }
                    'Windows 用户路径' { $snippet = '<Windows 用户目录，已脱敏>'; $reason = '本机路径，公开文档宜改为通用位置' }
                    '未列入公开白名单的 GitHub 链接' { $snippet = 'github.com/<owner>/<repo>（未在白名单）'; $reason = '离线无法判定公开性；确认后加入白名单或清理私有链接' }
                    'IP:端口代理地址' {
                        $snippet = '<IP 已脱敏>:' + ($value -replace '^.*:', '')
                        if ([Net.IPAddress]::IsLoopback($parsed) -or $address -match '^(?:0\.0\.0\.0|192\.0\.2\.|198\.51\.100\.|203\.0\.113\.|2001:db8:)') { $suggestion = '可接受（待确认）'; $reason = '回环、通配监听或文档保留地址；核对用途' }
                        else { $reason = '代理或监听地址候选；核对是否个人网络配置' }
                    }
                }
            }
            $findings.Add([pscustomobject]@{ Rule = $rule.Name; Commit = $Commit; File = $File; Line = $Number; Side = $Side; Snippet = $snippet; Suggestion = $suggestion; Reason = $reason; High = ($rule.Secret -and $suggestion -eq '需处理') })
        }
    }
}

$commits = @( (Read-Git @('rev-list', '--all')).Trim() -split '\r?\n' | Where-Object { $_ })
$run = Start-Git @('log', '--all', '--full-history', '-m', '--root', '--no-renames', '--no-ext-diff', '--no-textconv', '--unified=0', '--format=@@COMMIT %H%n@@AUTHOR %ae%n@@COMMITTER %ce', '-p', '--')
$commit = ''; $oldFile = $null; $newFile = $null; $oldLine = 0; $newLine = 0; $inHunk = $false
while ($null -ne ($line = $run.Process.StandardOutput.ReadLine())) {
    if ($line -match '^@@COMMIT ([0-9a-f]{40,64})$') { $commit = $Matches[1]; $inHunk = $false; continue }
    if ($line.StartsWith('@@AUTHOR ')) { Scan-Line $line.Substring(9) $commit '[提交作者]' 1 '元数据'; continue }
    if ($line.StartsWith('@@COMMITTER ')) { Scan-Line $line.Substring(12) $commit '[提交者]' 1 '元数据'; continue }
    if ($line.StartsWith('diff --git ')) { $inHunk = $false; $oldFile = $null; $newFile = $null; continue }
    if (-not $inHunk -and $line.StartsWith('--- ')) { $oldFile = Decode-GitPath $line.Substring(4); continue }
    if (-not $inHunk -and $line.StartsWith('+++ ')) { $newFile = Decode-GitPath $line.Substring(4); continue }
    if ($line -match '^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@') { $oldLine = [int]$Matches[1]; $newLine = [int]$Matches[2]; $inHunk = $true; continue }
    if ($inHunk -and $line.StartsWith('+')) { Scan-Line $line.Substring(1) $commit $newFile $newLine '新增'; $newLine++ }
    elseif ($inHunk -and $line.StartsWith('-')) { Scan-Line $line.Substring(1) $commit $oldFile $oldLine '删除（父版本行号）'; $oldLine++ }
    elseif ($inHunk -and $line.StartsWith(' ')) { $oldLine++; $newLine++ }
}
Complete-Git $run
$tracked = @((Read-Git @('ls-files', '-z')) -split "`0" | Where-Object { $_ })
$scanned = 0; $missing = 0; $binary = 0
foreach ($file in $tracked) {
    $path = [IO.Path]::GetFullPath((Join-Path $repo $file))
    Assert-LocalPath $path
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { $missing++; continue }
    $bytes = [IO.File]::ReadAllBytes($path)
    # Binary files are counted, not interpreted as text (same as Git's text diffs).
    if ([Array]::IndexOf($bytes, [byte]0) -ge 0) { $binary++; continue }
    $text = [Text.Encoding]::UTF8.GetString($bytes)
    $number = 0
    foreach ($line in ($text -split '\r?\n')) { $number++; Scan-Line $line 'WORKTREE' $file $number '当前跟踪文件' }
    $scanned++
}
$report = [Collections.Generic.List[string]]::new()
$report.Add('# 提交历史敏感信息扫描（离线、脱敏）')
$report.Add("`n扫描时间：$([DateTimeOffset]::Now.ToString('yyyy-MM-dd HH:mm:ss zzz'))。可达提交：$($commits.Count)；当前跟踪文本文件：$scanned；已删除：$missing；二进制跳过：$binary。")
$report.Add('范围：本仓库全部 refs 的文本补丁（含根提交、合并的各父差异及删除行）、提交作者/提交者邮箱、当前工作树跟踪文件。删除行号对应父版本；元数据行号 1 是字段位置。合并重复位置已去重；同一值在不同提交和新增/删除侧仍分别计数。未扫描不可达对象、二进制内容、未跟踪文件或提交消息。')
$report.Add('分类仅为建议；测试文件不自动免责。所有凭据类仅显示前 4 位及匹配值长度，不附上下文。私有仓库无法离线判定，未列入白名单的链接均列为待核对；链接、路径和 IP 均脱敏。')
$report.Add("公开仓库白名单条目：$($public.Count)（通过 -PublicRepositoryAllowlist 显式传入）。")
$report.Add("`n总命中：$($findings.Count)；需处理：$(@($findings | Where-Object Suggestion -eq '需处理').Count)；可接受（待确认）：$(@($findings | Where-Object Suggestion -ne '需处理').Count)；高风险凭据候选：$(@($findings | Where-Object High).Count)。")
$report.Add("`n## 规则统计`n`n| 规则 | 命中 | 需处理 | 可接受（待确认） | 高风险候选 |`n|---|---:|---:|---:|---:|")
foreach ($rule in $rules) {
    $group = @($findings | Where-Object Rule -eq $rule.Name)
    $report.Add("| $($rule.Name) | $($group.Count) | $(@($group | Where-Object Suggestion -eq '需处理').Count) | $(@($group | Where-Object Suggestion -ne '需处理').Count) | $(@($group | Where-Object High).Count) |")
}
$report.Add("`n## 高风险项摘要`n")
$highGroups = @($findings | Where-Object High | Group-Object Rule, Snippet)
if (-not $highGroups.Count) { $report.Add('未发现需处理的凭据候选。') }
foreach ($group in $highGroups) {
    $item = $group.Group[0]
    $report.Add("- $(Escape-Cell $item.Rule)：$(Escape-Cell $item.Snippet)；$($group.Count) 处。首处：$($item.Commit)，$(Escape-Cell $item.File):$($item.Line)。")
}
foreach ($rule in $rules) {
    $report.Add("`n## $($rule.Name)`n`n| 提交 | 文件 | 行号 | 位置 | 脱敏片段 | 初步分类 | 理由 |`n|---|---|---:|---|---|---|---|")
    foreach ($item in ($findings | Where-Object Rule -eq $rule.Name)) {
        $report.Add("| $($item.Commit) | $(Escape-Cell $item.File) | $($item.Line) | $($item.Side) | $(Escape-Cell $item.Snippet) | $($item.Suggestion) | $($item.Reason) |")
    }
}
Assert-LocalPath $output
[void][IO.Directory]::CreateDirectory((Split-Path $output -Parent))
[IO.File]::WriteAllText($output, ($report -join "`n") + "`n", [Text.UTF8Encoding]::new($false))
# Console output is deliberately limited to counts and the report's relative path.
Write-Host "扫描完成：$($commits.Count) 个提交、$scanned 个跟踪文本文件；$($findings.Count) 处命中，高风险候选 $(@($findings | Where-Object High).Count) 处。"
Write-Host "脱敏报告：$([IO.Path]::GetRelativePath($repo, $output))"
