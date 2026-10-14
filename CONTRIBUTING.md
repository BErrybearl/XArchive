# 参与贡献 / Contributing

反馈问题时，请说明应用版本、Windows 版本、预期行为及最小复现步骤，使用合成样本并移除个人信息。安全问题按 [SECURITY.md](SECURITY.md) 私下报告。

在 Windows 上使用 Rust stable MSVC（1.90+）、Node.js 24+、Microsoft C++ Build Tools、Windows SDK 和 WebView2 Runtime 构建与检查：

```powershell
npm.cmd ci
npm.cmd run build
npm.cmd run test:ui
npm.cmd run test:storage
node scripts/check-commands.mjs
cargo.exe test --manifest-path src-tauri/Cargo.toml --locked
cargo.exe fmt --manifest-path src-tauri/Cargo.toml -- --check
npm.cmd run tauri:build
```

Rust 测试使用隔离的合成数据。DPAPI 测试需要可正常使用的 Windows 当前用户加密环境；出现环境失败时应如实报告，不得关闭凭据保护。脚本需要 PowerShell 7。构建后运行 `scripts/package-release.ps1 -SkipBuild -Zip`，在 `.local/release/` 生成许可清单与便携发布包。

请保持改动聚焦，并说明相关验证。不提交真实归档、登录状态或凭据；不接受用于大规模抓取、绕过访问限制或再分发归档内容的功能。本贡献政策不改变 MIT 许可证。

For bug reports, provide versions, expected behavior and a minimal synthetic reproduction. Use the commands above to build and test on Windows. Keep changes focused and report relevant checks. Do not submit real archives, login state or credentials. DPAPI environment failures should be reported without disabling protection. We do not accept features for large-scale scraping, bypassing access restrictions or redistribution. See SECURITY.md for private vulnerability reports. This contribution policy does not alter the MIT License.
