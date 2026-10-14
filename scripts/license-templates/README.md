# 标准许可全文回退

MIT、Apache-2.0、BSD-3-Clause、MPL-2.0 全文来自 SPDX 官方 license-list-data。固定提交来源和每个原始文件的 SHA-256 见 [sources.json](sources.json)；生成时验证摘要，不联网下载模板。

模板 `.txt` 通过 `.gitattributes` 固定为 LF，确保 Windows 检出后仍与原始摘要一致。校验发现仅 CRLF/LF 差异时会提示恢复 LF；内容差异仍按摘要不一致拒绝，不自动规范化或放宽校验。

仅在 crate 及其 Cargo workspace 没有许可全文时，依据该版本 Cargo 的 license 字段使用模板。支持列出的标识及明确的 OR/AND 组合；不推断斜杠、WITH 例外或自定义许可。OR 全部保留，不代替发布者选择分支。NOTICE、COPYRIGHT、LICENSE.spdx 元数据保留为附加声明，不单独当作许可全文。

模板里的版权年份、权利人等占位符照录，不擅自补写，不声称是 crate 原始版权声明。全文缺失计数为 0 不表示发布义务已审核：模板回退的版权署名、上游附件及源码提供义务仍需发布者复核。文件名匹配含 LICENSE/LICENCE/COPYING/UNLICENSE 及点、连字符、下划线变体；父目录仅在连续 Cargo manifest 链中发现 workspace 时搜索，避免将 registry 邻近包许可错误归属。

本次 7 个缺失版本均已离线检查其缓存目录（包括嵌套文件），没有许可文件；它们均使用标准模板回退，没有下载或执行 crate 代码。
