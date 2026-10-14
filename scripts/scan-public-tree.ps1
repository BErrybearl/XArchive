#requires -Version 7.0
[CmdletBinding()]
param([string]$Root = (Split-Path $PSScriptRoot -Parent))

# Dot-source this file to use the same scanner before git init.
. (Join-Path $PSScriptRoot 'sensitive-rules.ps1')

function Get-PublicFiles([string]$Directory) {
    foreach ($item in (Get-ChildItem -LiteralPath $Directory -Force)) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw '扫描拒绝链接或重解析路径。' }
        if ($item.PSIsContainer) {
            if ($item.Name -in @('.git', 'node_modules', 'target', 'dist', '.local', '.runtime')) { continue }
            Get-PublicFiles $item.FullName
        } else { $item }
    }
}

function Get-UpstreamLicenseBlocks([string]$Relative, [string]$Text) {
    # Only actual notice bodies and standalone license texts can carry exceptions.
    if ($Relative -ceq 'THIRD_PARTY_NOTICES.md') {
        $start = 1; $previousIndex = 0
        foreach ($block in [regex]::Matches($Text, '(?ms)^~~~~text\n(?<body>.*?)^~~~~[\t ]*$')) {
            $body = $block.Groups['body']
            $start += [regex]::Matches($Text.Substring($previousIndex, $body.Index - $previousIndex), '\n').Count
            $previousIndex = $body.Index
            [pscustomobject]@{
                Start = $start
                End = $start + [regex]::Matches($body.Value, '\n').Count - 1
                Sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($body.Value)))
            }
        }
    } elseif ($Relative -cmatch '^scripts/license-templates/[^/]+\.txt$') {
        [pscustomobject]@{
            Start = 1
            End = [regex]::Matches($Text, '\n').Count + 1
            Sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Text.Encoding]::UTF8.GetBytes($Text)))
        }
    }
}

function Invoke-PublicTreeScan([string]$Directory) {
    $policy = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'public-scan-policy.json') -Raw | ConvertFrom-Json -AsHashtable
    $rules = @(Get-SensitiveRules)
    $findings = [Collections.Generic.List[object]]::new()
    $reviewed = [Collections.Generic.List[object]]::new()
    $upstreamEmails = [Collections.Generic.List[object]]::new()
    $counts = [ordered]@{}
    foreach ($rule in $rules) {
        $rule.Regex = [regex]::new($rule.Pattern, [Text.RegularExpressions.RegexOptions]::None, [TimeSpan]::FromSeconds(2))
        $counts[$rule.Name] = 0
    }
    $binary = 0; $textFiles = 0
    foreach ($file in @(Get-PublicFiles $Directory)) {
        $relative = [IO.Path]::GetRelativePath($Directory, $file.FullName).Replace('\', '/')
        $bytes = [IO.File]::ReadAllBytes($file.FullName)
        if ([Array]::IndexOf($bytes, [byte]0) -ge 0) {
            # Only the explicitly allowed icons and single overview PNG can be binary.
            $screenshot = $relative -ceq 'docs/screenshot-overview.png' -and $bytes.Length -ge 8 -and
                [Convert]::ToHexString($bytes[0..7]) -eq '89504E470D0A1A0A'
            if ($relative -notmatch '^src-tauri/icons/[^/]+\.(png|ico|icns)$' -and -not $screenshot) {
                $findings.Add([pscustomobject]@{ File = $relative; Line = 0; Rule = '非图标二进制文件' })
            }
            $binary++; continue
        }
        $textFiles++
        $trustedNotice = $policy.upstreamFiles.ContainsKey($relative) -and
            (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash -eq $policy.upstreamFiles[$relative]
        $text = [Text.Encoding]::UTF8.GetString($bytes).Replace("`r`n", "`n")
        $licenseBlocks = @(Get-UpstreamLicenseBlocks $relative $text)
        $fileExceptions = @($policy.upstreamEmailExceptions | Where-Object { $_.file -ceq $relative })
        $lineNumber = 0
        foreach ($line in ($text -split '\n')) {
            $lineNumber++
            foreach ($rule in $rules) {
                foreach ($match in $rule.Regex.Matches($line)) {
                    $value = $match.Groups['value'].Value
                    $reason = $null
                    if ($rule.Name -eq '邮箱地址') {
                        # An address-looking substring in URI userinfo is not an email address.
                        $prefix = $line.Substring(0, $match.Index)
                        if ($prefix -match '[a-z][a-z0-9+.-]*://[^\s"''<>]*$') { $reason = 'URI 用户信息语法，非邮箱' }
                        elseif ($relative -ceq 'src-tauri/tests/e2e/mod.rs' -and
                            $line.Trim() -ceq ('"https://user' + [char]64 + 'pbs.twimg.com/image.jpg",')) { $reason = '拒绝 URI 用户信息的合成测试，非邮箱' }
                        elseif ($relative -eq 'src-tauri/tauri.conf.json') {
                            $iconName = 'icons/128x128' + [char]64 + '2x.png'
                            if ($value -eq $iconName -and $line.Trim() -eq ('"' + $iconName + '",')) { $reason = '现有高清图标文件名，非邮箱' }
                        }
                        else {
                            foreach ($exception in $fileExceptions) {
                                $exactEmail = $exception.localPart + [char]64 + $exception.domain
                                if ($value -cne $exactEmail) { continue }
                                $block = @($licenseBlocks | Where-Object {
                                    $lineNumber -ge $_.Start -and $lineNumber -le $_.End -and $_.Sha256 -eq $exception.licenseSha256
                                })
                                if ($block.Count) {
                                    $reason = '文件与精确邮箱匹配，且位于摘要核验的上游许可原文内'
                                    $upstreamEmails.Add([pscustomobject]@{ File = $relative; Line = $lineNumber; Email = $value; LicenseSha256 = $exception.licenseSha256 })
                                    break
                                }
                            }
                        }
                    } elseif ($rule.Name -eq '未列入公开白名单的 GitHub 链接') {
                        $repository = $value.TrimEnd('.') -replace '\.git$', ''
                        if ($trustedNotice) { $reason = '固定摘要的第三方声明来源' }
                        elseif ($repository -in $policy.publicRepositories) { $reason = '明确的公开项目链接' }
                    } elseif ($rule.Name -eq 'Cookie 头') {
                        # Exact header-name arrays contain no header values.
                        if ($line.Trim() -match '^for (?:name|key) in \[\x22cookie\x22, \x22authorization\x22, \x22x-csrf-token\x22\] \{$' -or
                            $line.Trim() -match '^for credential in \[\x22auth_token\x22, \x22ct0\x22, \x22Cookie\x22, \x22Bearer\x22, \x22AAAAA\x22\] \{$') { $reason = '测试中的头名称数组，非凭据值' }
                    } elseif ($rule.Name -eq 'IP:端口代理地址') {
                        # Tauri's fixed local development server is not a configured proxy.
                        if ($relative -eq 'src-tauri/tauri.conf.json' -and $value -eq ('127.0.0.1' + ':1420') -and
                            $line -match '"(?:devUrl|devCsp)"') { $reason = 'Tauri 本地开发服务器' }
                        elseif ($relative -eq 'src-tauri/src/platform/proxy/tests.rs' -and $value -match '^\[2001:db8::1\]:8080$') { $reason = 'IPv6 文档保留地址测试' }
                    }
                    if ($reason) {
                        $reviewed.Add([pscustomobject]@{ File = $relative; Line = $lineNumber; Rule = $rule.Name; Reason = $reason })
                    } else {
                        $counts[$rule.Name]++
                        # Never print matched secrets, addresses, URLs or source lines.
                        $findings.Add([pscustomobject]@{ File = $relative; Line = $lineNumber; Rule = $rule.Name })
                    }
                }
            }
        }
    }
    [pscustomobject]@{ Passed = ($findings.Count -eq 0); Counts = $counts; Findings = @($findings); Reviewed = @($reviewed); UpstreamEmailExceptions = @($upstreamEmails); TextFiles = $textFiles; BinaryAssets = $binary }
}

if ($MyInvocation.InvocationName -ne '.') {
    $ErrorActionPreference = 'Stop'
    $result = Invoke-PublicTreeScan ([IO.Path]::GetFullPath($Root))
    $result | ConvertTo-Json -Depth 8
    if (-not $result.Passed) { exit 1 }
}
