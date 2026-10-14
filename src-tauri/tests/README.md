# 本地 HTTP 端到端集成测试

`e2e/mod.rs` 串起真实 reqwest 客户端、GraphQL 解析、归档引擎、并发媒体下载、SQLite、文件存储和导出。`e2e/server.rs` 使用标准库 `TcpListener` 绑定 `127.0.0.1:0`，生成合成 GraphQL/CDN 响应；没有假 Transport、外网请求或新增依赖。

为让可注入地址严格局限于 `cfg(test)`，测试由 `archive` 的测试模块加载，随库测试执行，并非独立的 Cargo `--test` 目标：

```powershell
# 先将 Rust 工具链的 bin 目录加入 PATH。
cargo.exe test --manifest-path src-tauri/Cargo.toml --locked --lib archive::e2e
cargo.exe test --manifest-path src-tauri/Cargo.toml --locked
cargo.exe fmt --manifest-path src-tauri/Cargo.toml -- --check
```

每个用例独享服务器、随机端口和 `.runtime/e2e-tests/` 下的临时数据/导出目录，结束自动清理。不读取已有归档、登录状态、Windows 系统代理或用户配置，不开应用窗口。只有 429 用例等待本地服务器给出的真实 reset 时刻（约 4 秒）；其余用例将 API 请求间隔注入为零，不依赖耗时阈值。

## 场景与断言

| 用例 | 核对内容 |
|---|---|
| `full_archive_and_incremental_use_real_http_and_bound_the_empty_tail` | Viewer、UserByScreenName、UserMedia、UserTweets 均实际发送 HTTP；两页媒体含重复置顶、转推原帖、多码率视频、GIF，两页时间线含纯文字。随后十个空页均返回新游标，有限结束。全量为 8 条唯一推文、5 个实体/来源；14 页共处理 10 条含跨接口重复的推文。最高码率被下载，HLS/低码率未请求，图片为 `name=orig`，转推保留原作者。限流头进入 Session，完成基线正确。无新内容增量只查各两页且无 CDN 请求；增加推文后增量只新增 1 份，成为 9 条推文、6 个实体/来源。 |
| `stopping_after_a_committed_page_resumes_from_the_durable_cursor` | 第一页提交后停止，保存 `m2`、3 条推文/3 份媒体，没有完成标记；使用新 Session 和新数据库连接续跑，从 `m2` 开始，不重下首屏媒体，最终 8 条推文/5 份媒体。 |
| `http_429_saves_checkpoint_and_blocks_requests_until_the_reset` | 第二页真实返回 429 和限流头；立即停止，不自动重试，保留 `m2` 与已存媒体；reset 前手动重跑仍不发送媒体分页请求；等待实际 reset 后继续，从 `m2` 完成，限流信息和完成标记均核对。 |
| `failed_cdn_downloads_retry_then_export_and_preserve_modified_files` | CDN 返回 404、声明 4096 字节却只发送 7 字节的截断体、302 重定向。首轮三项进入持久待重试，部分字节没有成为文件/实体。第二轮全量先处理持久重试：截断成功，404/302 达到两次上限进入 failed；重定向落点从未被请求。最终 11 条推文、6 个实体/来源、2 项终态失败、无 pending。随后全量导出 6 份，逐份核对内容、长度、SHA-256、来源作者、JSON/CSV 引用和文件数；等长修改一个导出文件，增量导出保留修改，报告 `modified`，索引仅引用剩余 5 份有效文件，6 个媒体文件保留，归档库不变，导出无 HTTP 请求。 |
| `explicit_proxy_routes_api_and_cdn_through_real_reqwest` | 通过现有显式代理解析和 reqwest custom proxy 发起完整归档；服务器作为本地 HTTP 代理接收 API/CDN 的绝对 URL 请求，数量结果同正常全量。 |
| `system_proxy_bypass_routes_api_and_cdn_directly` | 注入系统代理快照，代理指向关闭端口，绕过列表包含 `127.0.0.1`；完整归档成功，请求使用直连的相对路径，证明 API/CDN 共用绕过逻辑。 |
| `local_media_allowlist_is_per_client_and_production_policy_stays_strict` | 测试客户端仅允许注入的精确回环 origin；其他端口、凭据、片段、域名混淆及非 HTTPS 生产 URL 拒绝；生产白名单仍仅接受原有 HTTPS 媒体主机，生产校验拒绝本地 HTTP URL。 |

模拟服务器逐请求检查 GraphQL 的合成 Authorization/Cookie/CSRF、variables、features 和 fieldToggles；所有 CDN 请求均检查不携带认证头。客户端保持禁止重定向及正常证书校验。每次关键运行后检查数据库计数、实际文件数量、完整性报告和临时文件残留。

## 生产代码改动与发现

- 将归档入口的凭据/客户端构造抽出为私有 `run_with`，测试与生产共用 Viewer 校验、目标绑定、扫描、检查点、重试和结束路径。测试只注入合成凭据、客户端和请求间隔，不测试 DPAPI 文件存取。
- HTTP/媒体客户端复用原有构建与 ProxyPlan 路由；基础 URL、媒体 origin 和系统代理快照注入均仅在 `cfg(test)` 编译。媒体校验经 transport 方法传入并发 worker，生产实现仍调用原 `allowed`，默认 URL、主机白名单、端口/凭据/片段限制和重定向策略不变。

### 生产缺陷修复

无。本轮 7 项真实 HTTP 集成测试未暴露需要修复的生产缺陷；以上改动仅提供测试接入并复用原有路径，没有修改归档、重试或导出策略。

测试自身首次编译发现计数辅助函数直接将 SQLite 整数读为 `u64` 不受当前 rusqlite 支持，已改为读取 `i64` 后检查转换；这不涉及生产代码修复。

## 检查结果（2026-10-15）

| 检查 | 结果 |
|---|---|
| `cargo test --locked --lib archive::e2e -- --nocapture` | **7/7 通过**，0 忽略，运行约 4 秒。 |
| `cargo test --locked` | **163 通过、3 失败**，0 忽略、0 筛除；失败均为下列已知 DPAPI `Protection` 沙箱问题。退出码 101。 |
| 仅排除三项 DPAPI 后 `cargo test --locked -- --skip …` | **163 通过、0 失败、3 筛除**，二进制测试和文档测试也通过，退出码 0。 |
| `cargo fmt -- --check` | 通过。 |
| `git diff --check` | 通过。 |

命令均使用 `--manifest-path src-tauri/Cargo.toml`。三项已知失败未修改、未标记忽略；完整测试仍实际运行它们，额外排除重跑仅用于完成后续测试目标：

```text
auth::tests::encrypted_roundtrip_and_corrupt_state
auth::tests::failed_confirmation_preserves_saved_state_and_can_cancel
auth::tests::verified_session_saves_only_after_check_and_detects_switch
```

可复现的排除命令：

```powershell
cargo.exe test --manifest-path src-tauri/Cargo.toml --locked -- `
  --skip auth::tests::encrypted_roundtrip_and_corrupt_state `
  --skip auth::tests::failed_confirmation_preserves_saved_state_and_can_cancel `
  --skip auth::tests::verified_session_saves_only_after_check_and_detects_switch
```

本地完整日志：[未筛除测试](../../.local/checks/e2e/cargo-test.log)、[仅排除三项 DPAPI](../../.local/checks/e2e/cargo-test-excluding-dpapi.log)。日志不提交。工具链仍有既有的用户目录路径无法规范化提示，项目无编译警告。未改前端或 `进展.md`，未提交或推送。

本组验证 HTTP 传输和持久化行为；媒体字节为合成载荷，不覆盖解码、真实 X 接口有效性、HTTPS 握手或 SOCKS/CONNECT 代理握手。交由 Claude 根据上述场景和断言指导最终验收。
