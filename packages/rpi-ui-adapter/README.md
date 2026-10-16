# rpi-ui-adapter

通过 `rpi --webui=<UI>` 接入现有 Web UI。首个适配器是 Open WebUI 0.12.2，
后续 UI 在这个包里新增适配器。rpi 核心、`rpi-acp`、Open WebUI 源码都无需修改。

```text
浏览器 → Open WebUI + 自动安装的 Pipe
                     ↓ UI 事件 ↔ ACP JSON-RPC
              rpi-ui-adapter 的鉴权 WebSocket
                     ↓ 原样转发 ACP / stdio
                  rpi --acp
                     ↓
              rpi --mode rpc → 模型和工具
```

这是 **UI Adapter**，不是另一个 ACP 实现，也不是 OpenAI API 网关。
WebSocket 是本扩展的传输封装，每帧一个 ACP JSON 对象；不宣称它是 ACP 标准远程传输。
工具始终由 rpi 执行。Open WebUI 接收状态、文本、思考和可展开工具详情，不会重复调用工具。
不需要 Computer，也不需要手动编辑 Python。

## 首次启动

本扩展尚未发布，当前从本工作区构建；发布后才可使用 `rpi install rpi-ui-adapter`。

```bash
cargo build --release --workspace
# 使用仓库的原生安装器安装目录中的扩展（含 rpi-ui-adapter 和 rpi-acp）
cargo run --release -p rpi-package-install -- install
```

已有正常配置模型的 rpi、已安装 `rpi-acp`，以及一个启用了 Functions 的 Open WebUI 服务。
在 Open WebUI 的账户设置里获取管理员 token。支持管理员登录 JWT；使用 API key 时，
该实例还需允许它访问 Functions 管理 API。

Linux / macOS，同一宿主机直接运行的 Open WebUI：

```bash
export OPENWEBUI_TOKEN='<Open WebUI 管理员 token>'
rpi --webui=openwebui --webui-url http://127.0.0.1:8080
```

PowerShell：

```powershell
$env:OPENWEBUI_TOKEN = '<Open WebUI 管理员 token>'
rpi --webui=openwebui --webui-url http://127.0.0.1:8080
```

也可使用 `--webui-token <TOKEN>`。扩展会自动创建或更新托管 Pipe、配置连接并启用模型
`rpi`（ID `rpi_ui_adapter`）。进入 Open WebUI 后选择该模型；保留其他模型和现有连接。
同名但不是本扩展托管的 Function 会报错，避免覆盖用户代码。

本地保存 UI 地址、适配地址、当前项目、权限策略和适配器的独立访问 token；
**不保存 Open WebUI 管理员 token**。配置目录为
`$RPI_CODING_AGENT_DIR/ui-adapter` 或 `~/.rpi/agent/ui-adapter`，Unix 配置文件权限为 `0600`。

在同一项目里，配置不变时再次启动：

```bash
rpi --webui=openwebui
```

首次安装、更新插件版本、更换项目、UI 地址或权限策略时，需要再次提供管理员 token。
设置 `OPENWEBUI_TOKEN` 后，每次启动都会自动刷新 Pipe；未提供 token 时复用已安装配置。
一个运行实例服务一个项目，同一个 Open WebUI 实例当前只注册一个 `rpi_ui_adapter` 模型。

## Open WebUI 在 Docker 中，rpi 在容器外

`--webui-url` 是 **rpi 访问 Open WebUI** 的地址；`--webui-adapter-url` 是
**Open WebUI 后端访问 rpi** 的地址。浏览器不直接连接适配器。

例如 Open WebUI 映射宿主机 5233 端口，rpi 在宿主机运行：

```bash
export OPENWEBUI_TOKEN='<管理员 token>'
rpi --webui=openwebui \
  --webui-url http://127.0.0.1:5233 \
  --webui-bind 0.0.0.0:5234 \
  --webui-adapter-url http://host.docker.internal:5234
```

Linux Docker 需要给 Open WebUI 容器添加
`--add-host=host.docker.internal:host-gateway`，或在 Compose 中添加
`extra_hosts: ["host.docker.internal:host-gateway"]`。WSL Docker 同样要确保该地址
指向运行 rpi 的 WSL 宿主，不能把容器内 `127.0.0.1` 当作宿主机。

Open WebUI 镜像内不需要 rpi、ACP 扩展、模型凭据或项目挂载。
它们全部留在适配器所在的机器，ACP 会话的 `cwd` 是启动 rpi 的当前目录。
跨机器连接使用可达地址和 TLS 反向代理（支持 WebSocket Upgrade）；不要通过公网明文传递 token。
适配端口拥有运行该项目工具的能力，只应向可信的 Open WebUI 后端开放。

## 参数

| 参数 | 环境变量 | 默认值 / 用途 |
| --- | --- | --- |
| `--webui=openwebui` | — | 选择 UI；未知 UI 明确报错 |
| `--webui-url` | `RPI_WEBUI_URL` | 首次必填，已有 Open WebUI 地址 |
| `--webui-token` | `OPENWEBUI_TOKEN` | 安装与更新时需要的管理员 token |
| `--webui-bind` | `RPI_WEBUI_BIND` | `127.0.0.1:5234` |
| `--webui-adapter-url` | `RPI_WEBUI_ADAPTER_URL` | UI 后端可达的 HTTP(S) 地址 |
| `--webui-permissions` | `RPI_WEBUI_PERMISSIONS` | `ask`、`bypass`、`deny`；默认 `ask` |

`ask` 将标准 ACP 权限请求映射到原生确认框；没有活动浏览器、超时或拒绝时取消该请求。
`bypass` 自动选择 ACP 的允许选项；`deny` 一律取消。切换示例：

```bash
rpi --webui=openwebui --webui-permissions bypass --webui-token "$OPENWEBUI_TOKEN"
```

权限策略仅处理 agent 实际发出的权限请求。请按 `rpi-acp` 的要求安装并启用 permission-gate；
没有产生请求的工具不会被 UI Adapter 凭空拦截。普通 `ask_user` 问题扩展暂未协商支持，
rpi-acp 会取消不支持的问题；不会把它误当作权限确认。

`--model`、`--provider`、`--extensions-dir` 会转交 ACP 子进程。
已有模型配置、鉴权、技能、工具和 ACP 历史存储均由 rpi 处理。
同一进程不能同时使用 `--webui` 与 `--acp`；适配器会自行启动 `rpi --acp`。

## 会话与展示

- 正文、思考分段流式输出；等待和工具进度通过 Open WebUI 原生 `status` 事件显示。
- 工具完成后输出可展开的输入/结果详情；这是只读展示，不伪造成待执行的 OpenAI `tool_calls`。
- 停止发送 `session/cancel`；连接关闭通过 stdio EOF 回收 ACP 和 RPC 子进程。
- 同一用户、同一聊天且完整对话前缀匹配时恢复原 ACP 会话；历史回放不会混入新回答。
- 编辑、重试或历史前缀变化时新建 ACP 会话，把当前分支的可见对话作为文本上下文带入；
  不复用错误分支，也不重放历史工具调用。这个路径不会复制旧会话隐藏的工具上下文。
- UI 侧的会话索引默认保存在 `$DATA_DIR/ui-adapter/sessions.sqlite3`，应随 Open WebUI 数据卷持久化。
  rpi 侧仍需持久化 ACP 历史目录；历史丢失时恢复会明确失败。
- 每个聊天串行执行，进程重启可恢复已完成会话；停止和失败的请求不复用可能已部分写入的会话。
- 当前仅支持文本；图像/音频会明确拒绝。自动标题、标签等后台任务不会触发有副作用的 agent。
  这些辅助任务可单独选择普通 LLM。

## 验证

```bash
cargo test -p rpi-ui-adapter --locked
cargo clippy -p rpi-ui-adapter --all-targets --locked -- -D warnings
# 在含有 Open WebUI 依赖的 Python 环境执行；无需模型调用
python -m unittest discover -s packages/rpi-ui-adapter/tests -v
```

可选真实宿主测试设置 `RPI_TEST_HOST`、`RPI_TEST_UI_PLUGIN`、`RPI_TEST_ACP_PLUGIN`，
分别指向 rpi、构建的 UI Adapter 动态库与 ACP 动态库。它会启动隔离实例，验证自动安装、
鉴权、真实 ACP initialize、保存后重启和未知 UI 错误，不修改用户的 rpi 配置。

参考：[Open WebUI Pipe](https://docs.openwebui.com/features/extensibility/plugin/functions/pipe/)、
[Open WebUI Events](https://docs.openwebui.com/features/extensibility/plugin/development/events/)。
