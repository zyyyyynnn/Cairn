<!--
PR 标题规范 (Conventional Commits):
  <type>(<scope>): <short description in present tense>
  示例:
    feat(core): support USN journal fast forward recovery
    fix(app): prevent virtual table blank page on rapid scrolling
    perf(ntfs): optimize MFT record parsing throughput
    refactor(service): isolate debounce accumulator state
    docs(spec): update IPC protocol timing requirements
-->

## Summary

<!-- 简要概括变更动机与核心逻辑。推荐附上结构图、diff-sketch 或调用树/组件树 -->

```text
<!-- 示例: 调用树、组件树或算法伪代码 -->
```

## Evidence

- **Before:** <!-- 变更前：错误截图、失败的测试输出或复现日志 -->
- **After:** <!-- 变更后：正常截图、通过的有效测试运行或性能基准输出 -->

## Merge Danger

- **Door:** <!-- two-way (低风险、易回退) 或 one-way (含破坏性操作、不可逆状态变更) -->
- **Blast Radius:** <!-- 影响面评估，如：isolated, core-engine, ipc-protocol, ui-layout -->
- **Ramifications:** <!-- 潜在外部影响、降级方案或回退策略 -->

## Related Issues

<!-- 关联的 Issue，例如: Closes #12, Fixes #34 -->
