# rpi-btw

`/btw` 打开独立的旁路聊天区：Markdown 回答、独立滚动、连续追问，不中断主任务，也不把旁路问答写入主会话。

> 需要 **rpi 0.6.0 或更高版本**（SideQuestion protocol v2）。仅更新插件无法获得新的聊天视图；插件会先检查协议，旧宿主明确提示升级，不静默退回无历史的一次性请求。

## 安装

```sh
rpi update
rpi install rpi-btw
```

安装后重启 rpi，使宿主和插件均加载新版本。

## 用法

```text
/btw
/btw 这里为什么选择内存会话，而不是 fork？
/btw status
/btw cancel
/btw hide
/btw show
/btw new 换个话题，解释一下生命周期
/btw clear
/btw help
```

- `/btw` 或 `/btw show`：打开或恢复旁路聊天区；没有历史时显示空输入框。
- `/btw <问题>`：创建或延续当前旁路线程；生成中提交的问题进入队列，明确反馈 queued。
- **追问直接在 BTW 输入框输入，按 Enter 提交，不需要反复输入 `/btw`。**
- `PageUp/PageDown` 滚动旁路历史，`Ctrl+Home/Ctrl+End` 到顶部/最新内容。用户向上浏览时，新输出不会强制拉到底部。
- 宽屏为右侧旁路、左侧主会话；点击切换输入焦点，滚轮作用于指针所在窗格。窄于 100 列或 regular 模式时使用旁路全屏视图。
- `Esc` 或 `/btw hide`：隐藏视图但不取消任务。重新 `/btw` 可恢复。
- 在旁路获得焦点时按 `Ctrl+C`，或执行 `/btw cancel`：只取消旁路当前生成和待执行队列，保留已完成问答；不取消主任务。
- `/btw new <问题>`：取消旧线程并丢弃历史，开始新话题；`/btw clear` 取消并清空线程与视图。
- `/btw status`：返回最近请求状态、待执行数量和完整当前可用答案。不再需要依赖此命令绕过面板预览截断。
- `/btw help` 显示帮助。仅精确匹配保留子命令；`new` 后须有问题。

## 隔离与上下文

推理由宿主管理，继承每次提交时当前生效的模型、provider 私有绑定和 thinking 配置；排队请求在提交时固定绑定。不重读默认配置，不启动第二个 `rpi` 进程，凭据不经插件参数传递。

新旁路线程只接收主会话**已提交用户/助手文本**的最近片段（最多 64 KiB）。该参考片段在线程内固定。它看不到未完成的流式消息，也不复制工具调用、工具结果、图片、thinking 或自定义元数据。

后续追问带上当前旁路线程中成功完成的问答。失败或取消的部分答案可展示，但不作为成功回答加入后续模型历史。历史仅保存在内存中，不持久化；达到上限后按完整问答轮裁剪，并显示 recent history only 提示。

旁路没有工具权限，不能检查文件、编辑代码或执行命令。主会话 transcript、steering/follow-up 队列、工具集合和模型设置保持不变。命令反馈只用于 TUI 展示，不是 `SendMessage` 或 `AppendEntry`。旁路答案不会自动反馈给主模型。

主任务正常结束不取消旁路。会话切换/关闭、扩展卸载/reload 会终止旁路请求并清理历史、队列与视图；旧任务的迟到更新不能恢复已清理的线程。

## 限制

- 问题 1–20,000 UTF-8 bytes，单次请求默认超时 120 秒。
- 单次答案最多 256 KiB、8192 个文本增量，超限明确失败，不静默截断。
- 同一线程串行执行，待执行队列最多 8 条；超限明确拒绝新增。
- 已完成历史最多 16 轮且问答文本合计不超过 1 MiB。
- 每宿主最多保留 16 个 owner，最多 8 个运行/清理中的工作线程；同一 DLL 的不同注册隔离。
- 取消不是强杀 provider，实际后台驱动退出之前仍占用执行容量。
- 无交互 UI 的宿主不展示视图，仍可查询状态。

## 本地构建与验证

插件继续使用 action 25，不改变 C ABI vtable 或工作区 SDK pin。新版命令会先请求 `capabilities` 并要求 `protocolVersion >= 2`，确保旧宿主不会先启动请求再假装支持追问。

```sh
# rpi-package
cargo test -p rpi-btw
cargo build -p rpi-btw
```

配套宿主实现已随 rpi 0.6.0 发布。在宿主源码仓库中可运行定向验证：

```sh
cargo test -p rpi-cli --lib side_questions
cargo test -p rpi-tui --lib scroll_view
cargo test -p rpi-extensions --lib side_questions
```

真实动态库验证：将 `RPI_TEST_BTW_LIBRARY` 设为实际插件 DLL/so/dylib 的绝对路径，再在宿主仓库执行：

```sh
cargo test -p rpi-cli --test btw_package
```

Windows 库名为 `rpi_btw.dll`，Linux 为 `librpi_btw.so`，macOS 为 `librpi_btw.dylib`。未设置环境变量时集成测试跳过。测试使用本地假 provider，无需真实 API key。

源码构建或发布成功不代表已更新当前运行的宿主或已安装插件。实际使用前需同时更新宿主和插件，重启后再进行真实终端交互验收。
