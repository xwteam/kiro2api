# Changelog

本文件记录项目的所有重要变更。格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [Unreleased]

## [0.1.1] - 2026-07-25

面板体验与账号导入的一批修复,以及文档/构建完善。

### Added

- **模型测试默认可用主 API-KEY**:未创建任何自建密钥时,模型测试页默认列出并使用「主 API Key」,开箱即可测通。
- **批量导入账号「逐条:添加 → 验活 → 过滤」**:导入 JSON 时逐个账号单独添加,添加后查一次余额(`getUsageLimits` 真打上游)做**验活**——查得到=有效保留,报错=失效自动回滚删除,过滤掉已失效账号。
- **导入去重**:按 `refreshToken` 跳过已在池中的账号,避免同一账号被重复导入产生两条抢同一轮换令牌的凭据(刷新时互相作废、浪费配额、增加上游风控)。
- **用户面板 `/user` 与全部页面的品牌 favicon**;多语言 README 顶部加入 128×128 品牌 logo 与 version 徽章;`scripts/set-version.sh` 一处改动同步 VERSION / Cargo.toml / 各语言徽章。

### Changed

- **批量导入不再一次性提交超大请求**:改回旧版逐条添加(每个请求都小),从根上避免大批量导入触发请求体积上限而报「发生错误」。
- **镜像构建改为交叉编译**:多阶段 Dockerfile 用 `$BUILDPLATFORM` 原生工具链交叉编译 arm64(替代 QEMU 模拟),多架构镜像构建显著提速。

### Fixed

- 修复未创建密钥时模型测试无可用密钥可选的问题。
- 修复批量导入 JSON 在账号较多时报「发生错误」(请求体积超限)。
- 修复用户面板 favicon 未随管理面板一并更新的问题。

## [0.1.0] - 2026-07-25

首个公开版本。多协议 AI 中转，后端为 Kiro（CodeWhisperer），统一提供 Claude 系模型。

### Added

- **四协议前端**：OpenAI Chat（`/v1/chat/completions`）、Anthropic Messages（`/v1/messages`，中枢母格式）、OpenAI Responses（`/v1/responses`）、Gemini 原生（`/v1beta/models/{m}:generateContent`）；每协议同时挂标准裸前缀与显式厂商前缀（`/openai/v1`、`/claude/v1`、`/gemini/v1beta`）。
- **完整能力**：每协议均支持流式（SSE）、函数调用（工具）真透传、图片输入（多模态）。
- **Kiro 账号池**：多账号轮询（`priority`/`balanced`）、每账号 RPM 限流、分级冷却；body-aware 失败分类（真凭据失效才永久禁用，配额/风控/限流冷却自愈）。
- **令牌自愈**：token 到期单飞刷新 + 原子落盘 `credentials.json`；三种交互式登录流（Builder ID 设备码 / IAM SSO 授权码 / 社交令牌）。
- **端点回退与跨账号重试**：Kiro IDE → CodeWhisperer → AmazonQ 按序回退；账号级失败跨账号重试；确定性请求错误（`INVALID_MODEL_ID`）不重试、不误伤账号，直接以 400 回明确说明。
- **统一鉴权闸**：`Authorization: Bearer` / `x-api-key` / `?token=` 常量时间比较；`adminApiKey` 保护 `/api/admin/*`，持有者以自身 API-KEY 访问 `/api/user/*`；`/health`、`/v1/ping` 不鉴权。
- **Web 管理面板 `/admin`**：仪表盘（运行时间/全局积分/系统信息/赞助卡/检查更新）、账号管理、API-KEY 管理、用量统计（含客户端 IP 与账号标签）、实时日志（SSE）、设置（负载均衡/鉴权密钥/集成示例/一键重启）；顶部运行状态、GitHub、重启、深浅色主题、5 语言 i18n。
- **用户面板 `/user`**：持有者以自身 API-KEY 登录，查看额度、累计用量与分页记录。
- **统计与缓存**：每日/账号用量统计、失败/限流日志、账号余额缓存（TTL）、动态模型清单缓存。
- **版本检查 / 更新 / 重启**：`GET /api/admin/check-update`（GitHub Release 比对）、`POST /api/admin/update`（返回更新命令）、`POST /api/admin/restart`（二次确认）。
- **交付**：多阶段 Docker 构建、非 root 运行（gosu）、多架构镜像（amd64/arm64）、健康检查、CI（tag 触发 GHCR 发布）。
