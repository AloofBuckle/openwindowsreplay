# 仓库换行符规范与受控迁移

本文定义 RustReplay 仓库的换行符策略、首次受控迁移流程和后续贡献规则。目标是消除跨 Linux/Windows 工作树产生的全仓行尾噪声，同时保证规范化提交不包含任何功能、格式或二进制内容变化。

## 背景

仓库历史内容在 Git index 中基本使用 LF，但当前工作树同时存在 LF、CRLF 和 mixed 三种状态。由于仓库此前没有 `.gitattributes`，Git 会受检出环境和编辑器设置影响，将大量受跟踪文件报告为修改。

在首次规范化前，必须确认：

- 暂存区为空；
- 没有需要保留但尚未记录的未跟踪文件；
- `git diff --ignore-space-at-eol --exit-code` 返回成功；
- 当前差异仅来自行尾，不包含源码、文档、配置或二进制语义变化。

如果任何一项不满足，应停止规范化，先隔离和处理真实改动。

## 仓库策略

Git index 中的文本文件统一规范化为 LF。工作树检出规则如下：

| 文件类型 | 工作树换行 | 说明 |
| --- | --- | --- |
| Rust、TOML、Markdown、JSON、HLSL、C/C++、头文件、日志、许可证 | LF | 跨平台源码和文档统一使用 LF |
| PowerShell、批处理和命令脚本 | CRLF | 保持 Windows 原生脚本兼容性 |
| DLL、EXE、LIB、PDB、归档、图像、音视频、磁盘 sidecar | 不转换 | 明确标记为二进制，禁止文本过滤 |

仓库策略由根目录 `.gitattributes` 定义，不依赖开发机的全局 `core.autocrlf` 设置。即使 PowerShell 文件在 Windows 工作树中显示为 CRLF，其 Git index 内容仍按文本规范化为 LF。

## 首次受控迁移

首次迁移必须作为独立维护变更执行，不得同时进行代码格式化、重构、依赖升级或功能开发。

### 1. 记录迁移前状态

记录当前分支、提交、暂存区、未跟踪文件和行尾状态：

```bash
git branch --show-current
git rev-parse HEAD
git status --short
git diff --cached --quiet
git diff --ignore-space-at-eol --exit-code
git ls-files --others --exclude-standard
git ls-files --eol
```

迁移前的普通 binary diff 应保存在仓库外作为临时恢复材料。迁移流程不得删除未跟踪文件、忽略文件或 `target/` 构建目录。

### 2. 应用属性和换行策略

先新增本文件，再新增根目录 `.gitattributes`。随后仅对 Git 已识别为文本的受跟踪文件进行机械换行转换：普通文本写为 LF，Windows 脚本写为 CRLF。

该步骤只允许改变行尾字节，不允许调用会重排源码的格式化器，也不允许修改字符编码、末尾空行、缩进、字符串内容或文件权限。

### 3. 检查差异

规范化后必须执行：

```bash
git diff --check
git diff --ignore-space-at-eol --exit-code
git ls-files --eol
git status --short
```

验收要求：

- 除新文档和 `.gitattributes` 外，不存在语义 diff；
- 所有普通文本为 `w/lf`；
- PowerShell 文件为 `w/crlf`；
- 已标记的二进制文件保持 `-text`；
- 没有文件新增、删除、重命名或权限变化；
- 没有暂存、提交或推送未经确认的内容。

### 4. 新检出验证

在临时 detached worktree 或全新 clone 中重新检出同一提交，确认 `.gitattributes` 能独立重现预期状态：

```bash
git status --short
git ls-files --eol
cargo fmt --all -- --check
```

`cargo fmt --check` 只作为 Rust 文本完整性检查，不应在换行维护变更中写回格式。Windows 专用编译和硬件测试应继续在具备 oneVPL、NVENC、D3D11、WASAPI 与 Media Foundation 环境的构建机上执行。

## 后续贡献规则

- 新文本文件应遵守 `.gitattributes`，不得依赖个人编辑器自动猜测换行。
- 功能提交不得夹带全文件换行变化。
- 若 `git status` 再次出现大面积修改，应先用 `git ls-files --eol` 和忽略行尾 diff 判断原因。
- 修改 `.gitattributes` 必须作为独立维护变更，并在新 worktree 中验证。
- 不得对未知或二进制格式运行批量换行替换。
- Windows 脚本保持 CRLF；其余新增源码和文档保持 LF。

## 回滚原则

如果规范化检查发现任何非行尾变化，应立即停止，不提交当前结果。恢复时只处理本次明确修改的受跟踪文本和新增策略文件，不使用会删除未跟踪文件或覆盖未知用户改动的广域命令。
