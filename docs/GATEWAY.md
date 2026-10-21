# Qoder 本地模型网关（1.7.4）

桌面端参考 WorkBuddy API 的侧栏、卡片和请求抽屉层级，保留 Qoder Switch 的森林绿、多语言、浅深主题、账号切换、会话和统计功能。每日积分领取独立成页。

## 使用

1. 安装并登录对应地区的 Qoder；在「账号池」添加或采集账号。
2. 「接入设置」选择国内版或国际版，保留默认「Qoder 原生签名」传输并保存。
3. 「模型目录」同步目录；目录可见不代表所有模型和档位都已实测。
4. 启动服务，在「接入设置」复制本地 API Key 和客户端配置。
5. Claude Code 使用 `http://127.0.0.1:24818`；Codex 和通用 OpenAI 客户端使用 `http://127.0.0.1:24818/v1`。

1.7.1 起，模型列表和「复制 ID」使用官方显示名称（例如 `GLM-5.3-Flash`）。网关将名称映射到对应 Qoder 内部 ID；旧版 `gfmodel` 等调用继续有效，无需修改已保存的客户端配置。重名时附内部 ID 消除歧义，JSON/SSE 响应保持客户端请求的模型名称。

模型 ID 必须来自所选地区目录。模型名称、思考档位、图片权限与额度由 Qoder 决定，不自动替换模型。

### Claude Code

```bash
export ANTHROPIC_BASE_URL="http://127.0.0.1:24818"
export ANTHROPIC_AUTH_TOKEN="<本工具的本地 API Key>"
export ANTHROPIC_MODEL="<目录中的模型 ID>"
claude
```

### Codex

将 provider 合并到用户自己的 `~/.codex/config.toml`，不要覆盖已有配置：

```toml
model = "<目录中的模型 ID>"
model_provider = "qoder_gateway"

[model_providers.qoder_gateway]
name = "Qoder Gateway"
base_url = "http://127.0.0.1:24818/v1"
wire_api = "responses"
env_key = "QODER_GATEWAY_API_KEY"
```

在启动 Codex 的终端设置 `QODER_GATEWAY_API_KEY` 为本工具的本地 Key。

## 协议与边界

- `GET /v1/models`：所选地区的已同步目录。
- `POST /v1/chat/completions`：JSON/SSE，文本、图片、function 工具、用量。
- `POST /v1/messages`：system、文本、图片、客户端工具调用及结果、思考内容和原生 SSE 事件转换。
- `POST /v1/messages/count_tokens`：本地估算，响应头 `x-token-count-method: estimate`，不调用模型。
- `POST /v1/responses`：无状态完整历史、instructions、文本/图片、function、namespace、custom 工具，JSON/SSE。
- Messages / Responses / Models 提供根路径和 `/v1/v1` 兼容别名。

custom 工具通过单字段 JSON function 包装传给 Qoder，输出还原 custom_tool_call 与 custom_tool_call_output，保留换行、命名空间和 call_id。语法描述保留在说明中，Qoder 不提供相同的 grammar 硬约束，不能保证所有模型的语法遵从率。custom 输入 delta 在完整包装到齐后发送，普通文本和 function 参数仍逐块传输。

不支持托管 web_search/file_search、后台响应、previous_response_id/服务端存储链、跨提供商加密推理历史或 Responses WebSocket。实际加密内容明确报错，不伪造签名。外部客户端执行工具，网关只负责模型通信。

## Qoder 传输

默认使用安装包内的原生签名组件准备推理请求，由 Rust Reqwest 实际发起 HTTP/SSE。国内默认 `gateway.qoder.com.cn`，国际默认 `api2.qoder.sh`，地区账号隔离。

macOS 需要安装对应 Qoder（本轮检查 0.4.4 / worker 1.1.66）。使用其 Electron Node 模式加载本机协议组件，不启动 agent、执行工具或修改官方安装文件；Node.js 是可选后备运行时。安装包不携带 Qoder 官方运行时。派生模块缓存位于 `~/.qoder-switch/runtime-cache/`，版本不兼容时明确报错。

Model Server HTTP 为高级选项，需要账号支持该渠道并配置其官方地址；本轮直接 Bearer 入口返回 401，因此未设为默认可用链路。

网关不主动刷新或改写原登录态，过期时在账号池刷新或重新登录。账号可单独启停，每账号最多 2 并发，支持轮询和临期优先；401/403 短暂隔离，429 冷却。一次请求失败不换账号自动重试。`X-Account-ID` 可指定账号；`X-Session-ID` / `X-Conversation-ID` 可保持会话标识，未指定时每请求生成独立标识。

## 数据与运行

- 仅监听 127.0.0.1，独立本地 Key；拒绝带 Origin 的网页跨域请求。
- 官方令牌仅在后端使用，Key 文件在 Unix 创建为 0600。
- 配置、Key、模型缓存：`~/.qoder-switch/gateway/`。
- 最近 200 条请求元数据仅保留在内存，退出清空，不保存用户正文、生成内容、工具参数或认证头。
- Token 页默认展示「反代实际」：按地区、时间范围统计输入/输出、日趋势、账号/模型分布和请求明细；「本地估算」独立切换。
- 1.7.2 起反代请求数值及元数据写入 `~/.qoder-switch/gateway/usage.jsonl`（0600），重启及清空运行日志不删除统计历史，不保存正文、凭据或错误文本。页面每 5 秒刷新。未返回 usage 的明细显示 `—`，总量仅汇总已知部分；缓存已包含在输入内，避免重复相加。旧版内存中已丢失的记录无法补回。
- 连接 15 秒、总超时 180 秒；输入、转换后和签名后正文最多 32 MiB，聚合响应最多 16 MiB，单 SSE 事件最多 2 MiB。
- 停止服务等待活动请求结束，客户端断开取消上游并释放并发槽。

## 验证

```bash
cargo test --manifest-path src-tauri/Cargo.toml
node --check ui/app.js
node --check ui/gateway-shell.js
node --check ui/gateway.js
```

33 项 Rust 测试通过（包含官方名称与旧内部 ID 在三协议 JSON/SSE 下的兼容回归），新增持久化重读、去重、地区隔离、跨日时区和缺失用量测试；覆盖三协议 JSON/SSE、鉴权、count_tokens、namespace/custom 工具往返、UTF-8 分片、异常和截断流、额度信封、模型及参数保留。离线 HTTP 测试使用虚构账号和本地模拟上游，不读取真实账号。

2026-10-21，真实国内账号 + DeepSeek-Flash（dfmodel）验证：Messages 工具调用→结果回传→流式最终回复，以及 Responses custom 工具→结果回传→流式最终回复均通过。最后一轮每请求上限 128 输出 tokens，共 4 次模型请求，脱敏结果见 `docs/evidence/gateway-smoke.json`。这是网关协议验证，不等同于所有 Claude Code/Codex 版本、插件、模型和图片输入均实测。国际推理与全模型覆盖另行验证。

界面验收使用 `scripts/preview-gateway.mjs` 的虚构账号，预览脚本不进入应用前端资源。`python3 scripts/smoke-gateway.py` 只检查目录和估算接口；`--live` 会发起最多 4 次、每次最多 128 输出 tokens 的真实闭环测试，仅在需要生产验证时运行。

## 来源

Messages、Responses 和 namespace 转换模块适配自用户指定的本地 WorkBuddy-Api。Qoder 签名/传输、custom 工具适配、桌面网关界面与接入在本项目实现，未复制 WorkBuddy 设备 SDK 或官方传输。

官方协议依据：
- https://learn.chatgpt.com/docs/config-file/config-advanced
- https://platform.claude.com/docs/en/build-with-claude/streaming

## 1.7.3 账号池

账号池采用分区卡片：身份、积分余量、操作及反代开关；国内/国际版独立筛选，支持名称、备注、邮箱、手机号及 UID 搜索，提供紧凑视图。凭证刷新、备注与删除收进更多菜单。积分通过现有 `credits` 查询，手动刷新当前地区或单账号，按订阅和资源包真实汇总展示，未查询显示待查询；本次会话缓存用于重新渲染，退出应用后重新查询。没有逐积分包数据时不虚构包数或到期时间。

界面验证使用离线虚构账号，检查搜索、地区、紧凑视图、更多菜单、积分刷新、当前账号、反代开关与并发显示，以及浅色/深色布局。

## 1.7.4 API Key 管理

「接入设置 → 本地 API Key」新增编辑与随机生成。随机生成仅产生草稿，保存密钥后才写入磁盘并立即替换认证密钥，取消不修改。允许 32–256 位 ASCII 可见字符，不接受空白、换行与非 ASCII 字符。密钥仍通过 0600 临时文件原子替换保存，重启继续使用已保存密钥；写入失败保留当前值。更换后需更新 Claude/Codex 等客户端，已通过认证的进行中请求不被中断。

34 项 Rust 测试通过，包括本地 HTTP 新旧 Key 认证、Bearer/x-api-key 两种请求头、生成随机性、非法输入拒绝及磁盘保存。界面以离线演示网关验证编辑、随机草稿、取消、校验失败和保存成功；未修改用户实际网关密钥。
