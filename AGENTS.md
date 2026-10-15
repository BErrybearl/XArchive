# XArchive 开发协作约定

## 项目简介与技术栈

XArchive 是个人 X 媒体归档桌面工具，支持手动全量归档、增量更新、旧库导入与本地导出。应用使用 Rust、Tauri 2、Svelte 5、TypeScript 和 SQLite，以 Windows 便携 EXE 和独立窗口运行。媒体采用可直接浏览的文件夹结构：A 按账号与月份，C 按账号再分类型，默认 C。

本仓库独立开发，不套用其他项目的目录、编号或流程。使用自然、简洁的中文说明改动与验证结果，只开展当前任务相关工作。

## 目录结构

- `src/`：Svelte 界面、TypeScript 调用接口、浏览器演示与样式。
- `src-tauri/src/`：Rust 应用、归档、存储、登录、导入、导出与平台实现；SQL 随模块保存。
- `src-tauri/tests/`：使用合成数据的本地 HTTP 集成测试。
- `src-tauri/capabilities/`、`src-tauri/icons/`、`src-tauri/licenses/`：Tauri 权限、图标与内嵌许可。
- `checks/`：前端、存储结构及公开源码引用检查。
- `scripts/`：环境检查、许可生成、公开扫描与发布打包。
- `docs/`：公开文档附件。
- `.runtime/`：隔离开发样本与测试临时数据；不提交。
- `.local/notes/`：个人开发记录、计划、验收记录与本地检查日志；不提交。
- `.local/release/`：本地发布产物；不提交。

## 开发、测试与打包

需要 Windows、Rust stable MSVC（1.90+）、Node.js 24+、Microsoft C++ Build Tools、Windows SDK 和 WebView2 Runtime；脚本使用 PowerShell 7。先将 Cargo 的 `bin` 目录加入 `PATH`。PowerShell 下使用 `npm.cmd`。

```powershell
npm.cmd ci
npm.cmd run doctor
npm.cmd run dev
npm.cmd run tauri:dev
```

按改动范围运行必要检查，交付时说明结果与环境限制：

```powershell
npm.cmd run build
npm.cmd run test:ui
npm.cmd run test:storage
node scripts/check-commands.mjs
node scripts/check-public-references.mjs
cargo.exe test --manifest-path src-tauri/Cargo.toml --locked
cargo.exe fmt --manifest-path src-tauri/Cargo.toml -- --check
pwsh -NoProfile -File scripts/scan-public-tree.ps1
git diff --check
```

`build` 包含 Svelte/TypeScript 检查。DPAPI 测试依赖 Windows 当前用户的加密环境；如遇沙箱环境失败，保留凭据保护与测试，记录具体失败，不将其他失败归入例外。

```powershell
npm.cmd run tauri:build -- -- --locked
pwsh -NoProfile -File scripts/package-release.ps1 -SkipBuild -Zip
```

默认构建产物为 `src-tauri/target/release/xarchive.exe`，打包输出到 `.local/release/`。若需要重新生成许可并构建、打包，可直接运行 `scripts/package-release.ps1 -Zip`。

## 数据安全与个人记录

- 开发与验证只使用 `.runtime/` 内的隔离合成样本；不使用用户真实数据目录、数据库或登录状态。端到端测试只请求其本地模拟服务。
- 凭据、Token、Cookie、浏览器资料、配置、数据库、媒体、日志、依赖及构建产物不进入 Git。保留 Windows DPAPI 保护，不在输出中暴露会话信息。
- 个人开发记录放在 `.local/notes/`，由 `.gitignore` 忽略。公开文档不包含本机绝对路径、个人邮箱或私有仓库链接。
- 公开扫描中邮箱只允许扫描策略核验的上游许可原文例外；不要扩大例外来容纳个人信息。
- 提交作者邮箱使用 GitHub noreply 地址（域名 `users.noreply.github.com`）。提交前检查暂存文件与作者身份，避免带入真实数据或个人记录。
- 未经当前任务授权，不提交、推送或发布版本；需要人工验收的窗口外观、前台操作与真实登录由维护者确认。

## 发布流程概要

保持 `package.json`、Cargo 与 Tauri 配置的版本一致；完成相关测试、公开扫描和人工验收，再生成第三方许可、构建 release 并打包。核对便携 EXE、可选 ZIP、许可文件与 SHA-256 清单，确认包内无运行数据。经维护者确认后提交、打标签并上传发布产物；当前未配置自动更新服务。使用说明见 `使用说明.md`，贡献约定见 `CONTRIBUTING.md`，安全问题按 `SECURITY.md` 报告。
