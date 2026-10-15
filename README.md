# XArchive

[![CI](https://github.com/BErrybearl/XArchive/actions/workflows/ci.yml/badge.svg)](https://github.com/BErrybearl/XArchive/actions/workflows/ci.yml)

[English](README.en.md)

个人 X 媒体归档桌面工具，使用 Rust、Tauri 2、Svelte/TypeScript 和 SQLite 构建，以独立窗口和 Windows 便携 EXE 运行。当前应用界面为简体中文。

## 功能

- 按账号手动执行全量归档与增量更新，保存图片、视频、GIF 和推文信息。
- 按内容、质量与 UTC 日期筛选；保存账号选项、任务进度和待重试项。
- 限流时停止，等待后手动继续；支持停止任务、检查结果和重试。
- 校验文件、管理账号数据、导入兼容旧库。
- 全量或增量导出本地归档，保护用户改动过的文件。
- 使用独立 Edge/Chrome 资料登录，支持系统或手动代理及连接诊断。
- 浅色、深色和跟随系统主题；可选数据位置，通过替换 EXE 更新。

## 截图

<!-- ![浅色主题概览页](docs/screenshot-overview.png) -->
截图待补充：仅展示一张浅色主题的主页（概览页），文件路径为 `docs/screenshot-overview.png`。当前不附图片。

## 安装与使用

单个 `XArchive.exe` 即可运行，开源许可可在应用内“关于”查看；Release 同时提供可选 ZIP。将 EXE 放到可写目录，或解压可选 ZIP 后运行 `XArchive.exe`。需要 Windows 10/11 和 Microsoft Edge WebView2 Runtime；浏览器登录还需要 Edge 或 Chrome。最终用户无需安装 Rust 或 Node.js。

创建数据目录、登录、添加账号后，先执行全量归档。操作、选项、导出、更新与备份见 [使用说明](使用说明.md)。本源码模板尚未配置托管发布入口或自动更新服务。

## 从源码构建

需要 Windows、Rust stable MSVC 工具链（Rust 1.90+）、Node.js 24+、含 C++ 桌面开发组件的 Microsoft C++ Build Tools、Windows SDK 和 WebView2 Runtime。请将 Cargo 加入 `PATH`。

```powershell
npm ci
npm run tauri:build
```

PowerShell 执行策略限制 npm 的 `.ps1` 启动器时，使用 `npm.cmd`。默认产物为 `src-tauri/target/release/xarchive.exe`；检查与打包命令见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## 数据与隐私

归档、设置、浏览器资料和加密登录状态仅存本机。保存的凭据由 Windows 当前用户的 DPAPI 加密；应用不会把归档或登录状态上传到开发者服务。执行操作时会向 X 发送认证请求，并从来源服务器下载媒体。专用浏览器资料也含敏感会话信息，请保护整个数据目录与备份；换机或换 Windows 用户通常需要重新登录。

## 免责声明

XArchive 是非官方项目，与 X Corp. 无隶属、背书或赞助关系。程序使用 X 网页内部接口，接口可能随时变化。使用本工具可能违反 X 服务条款，并导致账号被限制或封禁；风险由使用者承担。

本项目仅供个人备份与归档。请尊重内容版权与访问权限，不得用于再分发或大规模抓取。无法保证归档已删除或无权访问的内容。软件不提供担保，详见许可证。

## 致谢与许可

感谢 MIT 许可的 [CandyACE/x-downloader](https://github.com/CandyACE/x-downloader)，其原始下载器为旧版实现提供了基础。上游 MIT 声明及依赖许可保留在 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

Copyright (c) 2026 BErrybearl。XArchive 自身代码采用 [MIT 许可证](LICENSE)，第三方组件保留各自许可。
