#requires -Version 7.0
function Test-KnownDpapiFailures([string]$Text, [int]$ExitCode) {
    $known = @(
        'auth::tests::encrypted_roundtrip_and_corrupt_state',
        'auth::tests::failed_confirmation_preserves_saved_state_and_can_cancel',
        'auth::tests::verified_session_saves_only_after_check_and_detects_switch'
    )
    if ($ExitCode -ne 101 -or $Text -notmatch 'test result: FAILED\. \d+ passed; 3 failed; 0 ignored; 0 measured; 0 filtered out;' -or
        $Text -match '(?m)^error: (?!test failed, to rerun pass `--lib`)') { return $false }
    $failed = @([regex]::Matches($Text, '(?m)^test (\S+) \.{3} FAILED\s*$') | ForEach-Object { $_.Groups[1].Value })
    $blocks = @([regex]::Matches($Text, '(?ms)^---- (\S+) stdout ----\r?\n(.*?)(?=^---- |^failures:|\z)'))
    if ($failed.Count -ne 3 -or $blocks.Count -ne 3) { return $false }
    foreach ($name in $known) {
        if (@($failed | Where-Object { $_ -ceq $name }).Count -ne 1) { return $false }
        $block = @($blocks | Where-Object { $_.Groups[1].Value -ceq $name })
        if ($block.Count -ne 1 -or $block[0].Groups[2].Value -notmatch 'called `Result::unwrap\(\)` on an `Err` value: Protection') { return $false }
    }
    return $true
}

function Invoke-PublicExportVerification([string]$Directory, [string]$StateDirectory, [string]$Key, [switch]$AllowDpapiSandboxFailures) {
    $oldTarget = $env:CARGO_TARGET_DIR
    $PSNativeCommandUseErrorActionPreference = $false
    $generated = @('node_modules', 'dist', 'src-tauri/gen', 'src-tauri/.runtime', '.runtime')
    foreach ($relative in $generated) {
        if (Test-Path -LiteralPath (Join-Path $Directory $relative)) { throw "验证目录应为全新导出：$relative" }
    }
    try {
        $env:CARGO_TARGET_DIR = Join-Path $StateDirectory "$Key-target"
        Push-Location $Directory
        try {
            foreach ($step in @('ci', 'build')) {
                $log = Join-Path $StateDirectory "$Key.npm-$step.log"
                Write-Host "验证：npm $step"
                if ($step -eq 'ci') { & npm.cmd ci 2>&1 | Tee-Object -FilePath $log | Out-Host }
                else { & npm.cmd run build 2>&1 | Tee-Object -FilePath $log | Out-Host }
                if ($LASTEXITCODE -ne 0) { throw "npm $step 验证失败；未提交。日志：$log" }
            }
            Push-Location (Join-Path $Directory 'src-tauri')
            try {
                $log = Join-Path $StateDirectory "$Key.cargo-test.log"
                Write-Host '验证：cargo test --locked'
                & cargo.exe test --locked 2>&1 | Tee-Object -FilePath $log | Out-Host
                $code = $LASTEXITCODE
                if ($code -eq 0) { return 'Passed' }
                if ($AllowDpapiSandboxFailures -and (Test-KnownDpapiFailures ([IO.File]::ReadAllText($log)) $code)) {
                    Write-Host '仅三项已知 DPAPI Protection 沙箱失败，按显式开关放行。'
                    return 'PassedWithKnownDpapiSandboxFailures'
                }
                throw "cargo test 验证失败；未提交。日志：$log"
            } finally { Pop-Location }
        } finally { Pop-Location }
    } finally {
        $env:CARGO_TARGET_DIR = $oldTarget
        # Only freshly generated, fixed paths in the checked isolated export are removed.
        foreach ($relative in $generated) {
            $path = [IO.Path]::GetFullPath((Join-Path $Directory $relative))
            Assert-ExportPath $path
            if (Test-Path -LiteralPath $path) {
                $null = @(Get-AllExportFiles $path)
                Remove-Item -LiteralPath $path -Recurse -Force
            }
        }
    }
}
