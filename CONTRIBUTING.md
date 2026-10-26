# 贡献指南 / Contributing

Cairn 采用 Rust 与 GPUI 构建 Windows NTFS 文件检索与磁盘分析工具。参与开发与提交变更请遵循以下工程约定与工作流程。

---

## 开发环境准备

- **操作系统**：Windows 10 / Windows 11 (x64)
- **Rust 工具链**：Rust 1.82+（请参考项目根目录的 `rust-toolchain.toml`）
- **打包依赖（可选）**：WiX 4.0 (`dotnet tool install --global wix`)
- **环境检查**：
  ```powershell
  cargo check --workspace --all-targets
  cargo test --workspace
  ```

---

## 架构原则与编码规范

本项目遵循以下核心工程约定：

1. **高内聚低耦合**：
   - 核心功能边界收敛在对应 crate 内部（`cairn-core`, `cairn-service`, `cairn-client`, `cairn-app`, `cairn-ntfs`）。
   - 跨模块协作通过清晰、稳定的强类型接口或 IPC 协议通信，杜绝循环依赖与跨层穿透。
2. **根因修复与不设冗余兼容层**：
   - 重构或替换旧方案时直接收敛到当前实现，不增加历史垫片兼容层；同步清理废弃代码与测试。
3. **真实边界防御**：
   - 在系统边界（Win32 API 调用、MFT 解析、命名管道 IPC、用户输入）进行必要且严格的校验与错误处理，不采用无依据的静默降级或吞噬错误。
4. **提交信息规范**：
   - 提交历史严格遵循 [Conventional Commits](https://www.conventionalcommits.org/) 规范：
     ```text
     <type>(<scope>): <description>
     
     示例:
       feat(core): implement USN journal transaction boundary recovery
       fix(app): resolve table row selection jump on scroll reconciliation
       perf(ntfs): streamline MFT record attribute decompression
       docs(arch): clarify IPC message frame sequencing
     ```

---

## Pull Request 流程

1. **分支策略**：
   - 从 `main` 分支切出具象的功能分支（例如：`feature/xxx` 或 `fix/xxx`）。
2. **有效验证**：
   - 确保通过本地全量单元测试与编译检查：
     ```powershell
     cargo test --workspace
     ```
3. **提交 PR**：
   - 仓库已内置 PR 模板。请按照要求填写：
     - **Summary**：简述变更逻辑与调用树/组件树示意。
     - **Evidence**：提供变更前后的对比证据（测试通过日志或截图）。
     - **Merge Danger**：评估风险门级（单向门/双向门）与影响面（Blast Radius）。
4. **审查与合入**：
   - 所有合入由维护者审查确认后，采用 Squash & Merge 策略合入 `main`。
