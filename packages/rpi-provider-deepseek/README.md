# rpi-provider-deepseek

独立 DeepSeek provider 扩展。provider ID 保持 `deepseek`，原有同 ID 的
`auth.json`、`models.json` 和 `--provider deepseek` 可继续使用。
核心不再包含 DeepSeek 专用 provider、默认模型或环境变量规则；安装后由本包声明
模型目录、`DEEPSEEK_API_KEY` 和必须认证的策略。

支持文本 / 思考 SSE、工具调用与多轮 `reasoning_content` 回传、用量统计、HTTP 错误、
重试及中途取消。复用 `rpi-provider-runtime` / 通用 Chat Completions，不复制网络解析器。
目前只声明文本输入，未验证图片输入；未注册 OAuth，认证使用通用 API-key 命令。

## 安装与使用

当前源码依赖固定到 pi-rust 的 provider-package 实现提交，需要使用
[`feature-stream-porider` 分支宿主](https://github.com/bigfish1913/pi-rust/tree/feature-stream-porider)。
本包尚未发布 crates.io，相关核心依赖发布前设置 `publish = false`，先使用源码 link。
其他扩展的 SDK 依赖不变；本包的 SDK、协议与桥接库固定到同一核心提交，避免 ABI 类型混用。

在 rpi-package 仓库根目录，使用上述分支构建的 `rpi`：

```sh
rpi package link deepseek --path packages/rpi-provider-deepseek --debug
rpi --list-models deepseek
rpi auth login --provider deepseek
rpi auth check --provider deepseek --json
rpi --provider deepseek --model deepseek-flash
```

也可以设置当前进程的 `DEEPSEEK_API_KEY`，不需要重复 login。没有包时核心不会自动读取
此变量；`OPENAI_API_KEY` 不会借给 DeepSeek。`auth check` 只检查本地就绪状态，不验证服务端 key。
凭据顺序由宿主管理：显式 `--api-key` → auth.json → models.json → 包声明的专属 env。
卸载使用 `rpi package unlink deepseek`，API-key logout 不清除环境变量或 models.json。

默认目录按 `deepseek-v4-pro`、`deepseek-flash` 排序，保持原先默认 Pro 的行为。
端点为 `https://api.deepseek.com/v1`，目录上下文为 1,000,000 token，每次请求输出预设
8,192 token；后者是保守默认值，可通过 models.json 或请求 options 覆盖。
目录价格采用 2026-10-16 的峰时美元 / 百万 token 估值，非峰时账单可能较低，可覆盖 cost。

模型名、限制与价格依据 [首次调用](https://api-docs.deepseek.com/)、
[模型说明](https://api-docs.deepseek.com/quick_start/pricing/)。供应商调整时应同步更新本包目录。

`--thinking off` 显式发送 `thinking.type=disabled`；minimal / low 映射 low，
medium / high / xhigh 映射 high，max 映射 max。遵循宿主 options 约定：`None` 与 `Off`
均关闭思考；CLI 的默认 medium 映射 high，不依赖供应商隐含默认值。
工具请求保留所有历史 assistant 思考块，包括上一轮没有调用工具的回答，避免续轮 400。
规则依据 [思考模式与工具调用](https://api-docs.deepseek.com/guides/thinking_mode/)。

用户端点和请求头由宿主私有执行绑定提供，本包不读取进程 key 或改回固定运行时端点。
涉及包的 `/reload` 会重新绑定；移除当前包时保留旧会话并报错，先切换模型再卸载。

## 离线验证

在仓库根目录执行；必须先 build 真实动态库，缺少产物会失败，不静默跳过：

```sh
cargo build -p rpi-provider-deepseek
cargo test -p rpi-provider-deepseek
```

测试实际加载 DLL / so / dylib，覆盖：首个正文增量到达后服务端才继续发送的 SSE 门控、
中文 UTF-8 跨块、thinking 与 usage、响应未结束时取消、HTTP 401、唯一终态、思考开关，
以及工具调用 → 工具结果 → 回答 → 新用户轮次的完整历史回传。

宿主命令与凭据检查另外需要先构建上述分支的 CLI，再设置路径显式执行：

```powershell
$env:RPI_TEST_CLI = 'C:\path\to\pi-rust\target\debug\rpi.exe'
cargo test -p rpi-provider-deepseek --test host -- --ignored --nocapture
```

该测试使用隔离的临时配置与本地 SSE 服务，验证目录、禁用扩展、默认模型、认证来源、
OpenAI key 隔离、端点 / 请求头覆盖和实际 CLI 推理，不访问 DeepSeek 服务。

## 可选真实 API 验证

先在当前进程配置自己的 `DEEPSEEK_API_KEY`，再运行：

```sh
cargo test -p rpi-provider-deepseek --test streaming live_deepseek_extension_stream -- --ignored --exact --nocapture
```

此测试消耗额度，默认 ignored，最多请求 1,024 输出 token、不重试、超时 45 秒，
要求多个正文增量。不要将 key 写入源码、配置示例、日志或快照。

Rust 调用方从原核心入口迁到 `rpi_provider_deepseek::DeepSeekProvider::new(api_key)`；
该构造函数不读取环境变量。接入标准见
[Provider 接入规范](https://github.com/bigfish1913/pi-rust/blob/feature-stream-porider/docs/extensions/provider-integration.md)。
