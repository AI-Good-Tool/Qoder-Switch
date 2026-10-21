# Qoder 反代与 WorkBuddy API 界面参考研究

> 后续已在 1.7.0 实现，本文保留初始研究判断。当前功能和验证边界见 [网关说明](GATEWAY.md)。

研究日期：2026-10-21。目标：参考本地 WorkBuddy-Api 的布局与样式，为 Qoder Switch 增加可供 Claude Code / CC GUI 和 Codex 使用的本地模型网关。

本轮为源码和安装包静态研究，没有读取账号凭据、调用官方模型或修改客户端配置；以下候选路径不代表已经连通。

## 结论与证据

当前项目为 Tauri 2 + Rust + 原生 HTML/JS，已有国内/国际账号管理、OAuth 登录、令牌刷新、积分、会话和统计功能，但尚无 HTTP 模型服务。当前同步 ureq 管理接口不能直接承担异步流式模型网关。

参考项目 `/Users/dalong/Documents/codeWork/other/WorkBuddy-Api` 为 Vue 3 + TypeScript + Arco Design、Tauri 2 + Tokio/Axum。其 `src-tauri/src/anthropic.rs`、`responses.rs`、`response_tools.rs` 包含 Messages/Responses 转换，`proxy.rs` 包含路由、鉴权、流式响应和离线测试。可以参考其协议设计与测试，但官方传输、设备 SDK 和业务错误码属于 WorkBuddy，不能直接套用于 Qoder。复制代码前需确认源项目授权及相关来源声明。

本机 Qoder 国际版静态版本为 0.4.4，worker runtime 为 1.1.66。安装包中发现：

- 国际 SDK：`/Applications/Qoder.app/Contents/Resources/app.asar.unpacked/node_modules/@qoder-ai/qoder-agent-sdk/dist/_worker/`。
- 国内 SDK：`/Applications/Qoder CN.app/Contents/Resources/app.asar.unpacked/node_modules/@qoder-ai/qoder-cn-agent-sdk/dist/_worker/`。
- 两者均包含 `proto/chat.proto`、`runtime-info.json`、`qoder-worker-runtime.obf.mjs`。
- `chat.proto` 明确定义 ChatCompletionRequest、ChatCompletionChunk、tools/tool_choice、reasoning、图片、usage、缓存 token、reasoning token 等字段；服务包含 ChatCompletion 和 ChatCompletionStream。
- 运行时存在 HTTP JSON/SSE 路径 `/model/v1/chat/completions`，以及 gRPC 方法 `/model.chat.ChatService/ChatCompletionStream`。
- HTTP 代码显式设置 Bearer、Accept: text/event-stream、Content-Type: application/json、X-Request-ID、X-Session-ID，并合并运行时生成的其他 requestHeaders。
- 模型服务 host 由运行时配置与 `QODER_MODEL_SERVER_HOST` 覆盖决定；静态默认表包含 `api2-v2.qoder.sh`。国内包也包含此默认表，不能据此断言国内请求实际发往该地址；运行时 URL 转换、区域和路由仍需追踪。
- 模型目录候选路径为 `/api/v2/model/list?Encode=1`，可能附带 outerProviders；结果经过解码并按 scene 选择。不能当作普通 OpenAI `/models` JSON 原样转发。
- 模型传输取 `security_oauth_token ?? access_token`，当前 qcore 账号库主要保存 device token 与 refreshToken。必须验证登录来源之间如何换取模型凭据；管理接口可访问不证明模型接口可访问。
- proto 中有 reasoning_item.encrypted_content 字段，仅证明协议存在，尚未证明具体模型、HTTP 链路或外部 Responses 历史支持该能力。

## 界面方案

沿用 WorkBuddy API 的信息层级：固定侧栏、页面标题与说明、顶部操作区、概览指标、卡片/表格、右侧详情抽屉。参考其 218px 侧栏、44px 导航项、细边框、低对比灰底和紧凑字号。保留 Qoder 当前森林绿 `#3c5d48`、浅深主题、多语言与时区设置。

建议导航：工作台、账号池、模型目录、请求记录、积分统计、Token 统计、会话管理、每日领取、接入设置。

| 页面 | 具体内容 |
| --- | --- |
| 工作台 | 服务开关、监听地址、可用账号数、活动请求数、近期错误、Claude/Codex 接入入口 |
| 账号池 | 国内/国际分栏、凭据与额度状态、代理启停、调度偏好；客户端当前账号和网关可用账号分别展示 |
| 模型目录 | 实际模型 ID、地区、工具/图片/推理能力及验证状态，不预填未经验证的模型别名 |
| 请求记录 | 协议、模型、账号、耗时、用量、HTTP/业务错误与 request ID；抽屉显示脱敏诊断 |
| 接入设置 | 端口、独立本地 Key、监听状态、并发与超时、Claude 与 Codex 配置示例 |

先沿用现有前端栈拆分样式和页面模块，即可实现参考布局。Vue/Arco 迁移作为独立工程决策，不是反代成立的前提。

## 网关结构

```text
Claude Code / CC GUI ── Messages ─┐
Codex ──────────────── Responses ─┼─ 本地鉴权 → 协议适配 → 账号池 → Qoder 传输 → 统一事件
通用客户端 ─── Chat Completions ─┘                              ↓
                                                  各协议 JSON / SSE 响应
```

建议新增独立模块：`qproxy`（服务生命周期和路由）、`qprotocol`（统一消息/事件类型）、`qanthropic`、`qresponses`、`qtransport`（Qoder 鉴权/模型目录/传输）、`qpool`（调度、冷却和并发）。复用 qcore 的账号和区域定义；管理请求的同步 I/O 应放入阻塞任务，不能持有全局锁等待网络。

第一阶段优先验证已发现的 HTTP/SSE 传输。若身份头、签名或凭据兑换必须依赖官方运行时，再评估受控 helper。直接调用 SDK 的高层 agent 会夹带系统提示、工具执行和内部重试，不适合作为透明模型转发；应寻找底层模型通信接口。gRPC 保留为已确认存在的候选传输，不同时建设两套链路。

## 下游协议要求

| 客户端 | 本地接口 | 关键验收 |
| --- | --- | --- |
| Claude Code / CC GUI | POST /v1/messages、/v1/messages/count_tokens | system、文本、图片、tool_use/tool_result、并行工具、SSE 块索引、stop_reason、usage |
| Codex | POST /v1/responses | instructions、input items、function_call/output、call_id、namespace、custom 工具、推理摘要、事件顺序和完成/中断状态 |
| 通用兼容客户端 | GET /v1/models、POST /v1/chat/completions | 模型列表、JSON/SSE、tools、reasoning、finish_reason、usage |

Codex 自定义 provider 的官方配置示例使用 `wire_api = "responses"`，所以仅提供 Chat Completions 不满足本项目目标。参考项目已支持 function namespace，但仍拒绝 custom 自由文本工具和实际加密 reasoning 历史，不能直接把它的 Responses 子集称为完整 Codex 支持。需要针对目标 Codex 客户端离线采集脱敏请求结构，再决定每类工具的可逆映射；不得静默丢弃工具、历史、参数或伪造加密状态。

Messages 应生成其原生 message_start、content_block_*、message_delta、message_stop 事件；工具参数允许跨块分片。Responses 应正确关联 item_id/output_index/call_id，保留事件顺序；上游中断时不能输出成功完成事件。count_tokens 若用本地估算必须明确标记估算。

第一版建议无状态传入完整历史；previous_response_id、服务端会话存储、托管工具、WebSocket 等分别声明能力并单独验收。

## 账号与运行约束

- 国内/国际账号隔离，先验证各自实际可用的模型和路由。
- 使用独立本地 API Key；下游调用方不接触 Qoder 官方凭据，前端状态也不返回官方 token。
- 默认监听 127.0.0.1；服务启动、停止、端口冲突和应用退出有明确状态。
- 单账号限制并发；401/403、429、额度不足分别归因，不能将模型权限失败一律判成账号过期。
- 显式模型选择保持不变；一次请求失败不自动换模型。已开始输出时不透明重放请求。
- 明确正文、SSE 帧、聚合结果、首字节、空闲和总时长上限；客户端断开要取消上游。
- 日志保留数值和允许展示的诊断字段，避免保存 prompt、生成正文、工具参数及认证头。
- Token 统计增加“反代实际 usage”来源，与当前“本地估算”明确区分。

## 实施顺序与完成标准

1. 继续静态追踪模型鉴权、地区 host、目录解码和必要身份头，形成可单独测试的 Qoder 传输契约。
2. 建立本地模拟上游与三协议离线测试：文本、图片、两轮工具调用、并行工具、错误 SSE、截断、取消、长上下文和跨地区隔离。
3. 实现本地网关、账号池和工作台/模型/请求/接入设置页面，保留已有切换、领取、会话和统计能力。
4. 在得到生产调用授权后，以明确账号、模型、请求次数与 token 预算验证 Qoder；HTTP 成功还要检查业务码和正常结束事件。
5. 使用真实 Claude Code 与 Codex 完成工具调用→结果回传→最终回复闭环，再报告对应客户端和版本的兼容范围。

本次完成研究，尚未实现或实测网关。首要不确定点是 Qoder 模型凭据与身份链路，其次是目标 Codex 的 custom 工具及 reasoning 历史兼容。

## 外部协议依据

- OpenAI Docs：自定义 provider、base_url、鉴权与 Responses 配置：https://learn.chatgpt.com/docs/config-file/config-advanced
- Claude 官方流式事件与工具参数增量：https://platform.claude.com/docs/en/build-with-claude/streaming
