# 布布 · Boubou

鸣鸟（Mingbird）的 Rust 继任者：面向本地小模型的自主 agent。保留鸣鸟的两条核心思路：

- **极简前置**：系统提示 + 9 个核心工具 schema 约 1250 tokens，1B–4B 的本地模型也能直接驱动。
- **渐进式披露**：高级工具用 `enable_tools` 按需启用；技能只列名字，用到才加载全文。

名字来自南非黑鵙（Southern Boubou，*Laniarius ferrugineus*），一种锈红色、会二重唱的非洲丛鵙。
*ferrugineus* 即「锈色」。

## 状态

核心移植中。工具协议、配置、技能、Ollama 客户端、五环安全门、工具执行器、全部纯守护逻辑和 agent 主循环（finish 门禁、反循环阶梯、上下文压缩、检查点/会话/转录文件）已移植并有测试。`boubou run <模型> <任务文件> <工作目录>` 已能对本地 Ollama 端到端跑通；写出的会话文件可被鸣鸟直接读取。网页搜索/抓取（抓取缓存与鸣鸟共用）、长期记忆、`batch_tools` 网络并行、MCP（`rmcp`，stdio 与 streamable HTTP，工具按类别扁平化为 `服务器.工具名`）已完成。`boubou doctor` 会探测全部 MCP 服务器；`boubou tool <工具名> '<json>'` 可单独调试任一内置或 MCP 工具。搜索后端默认 Bing → DuckDuckGo → 百度（`AGENT_SEARCH_BACKENDS` 可覆盖）。`boubou serve` 启动本地网页界面（沿用鸣鸟的页面与 `/api/*` 接口，agent 进程内运行，支持越界确认弹窗与中止）；`cargo run -p boubou-app` 以原生窗口打开同一界面。并行派发已移植,默认关闭:在 `config.json` 里设 `"parallel": {"enabled": true, "child_model": "<模型>"}` 后,计划中足够多的简单单文件条目会派给沙箱子进程并行完成(`AGENT_CHILD_SANDBOX=1`,default-deny 工具面,只能写子目录,审计日志,严重违规 exit 53);`boubou plan` 显示本机容量。下一步是语音输入、`restart_ollama` 与云端后端。
详见 [docs/PORTING.md](docs/PORTING.md)。

## 与鸣鸟的兼容

数据目录不变（`~/.ollama_agent`，或 `BOUBOU_HOME` / `MINGBIRD_HOME`）：同一份 `config.json`、
`mcp.json`、`skills/` 与会话记录。工具 schema 与鸣鸟发送的 JSON 完全一致，提示词与基准数据可直接沿用。

## 构建

```sh
cargo build --release                 # 核心 + 命令行 boubou
cargo build --release -p boubou-app   # Tauri 桌面壳（较重）
cargo test
```

许可证：Apache-2.0，与鸣鸟相同。
