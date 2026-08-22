# Prompt：先实现共享 Rust Core

你负责截图器 UI 技术选型实验的共享 Core。

开始前完整阅读：

`00_DEMO_COMMON_SPEC.md`

这个 Core 会被两个独立 frontend 同时使用：

```text
capture-slint
capture-qml
```

因此你的首要目标是：

> 建立足够真实、足够干净、能够冻结的 Rust Core 与 Platform baseline，让后续两个 UI 实验只比较 UI 自身，而不是重复实现截图和领域逻辑。

---

# 1. 工作范围

你负责：

```text
crates/
├── capture-core
├── capture-platform-api
├── capture-windows
├── capture-linux
├── capture-annotation
├── capture-render
└── capture-actions

tools/
└── capture-cli
```

不要创建 Slint/QML UI。

---

# 2. 开始前先建立 Workspace 与文档

创建：

```text
docs/
├── architecture/
│   ├── module-boundaries.md
│   ├── coordinate-system.md
│   └── event-flow.md
└── adr/
```

首先把依赖方向写清：

```text
platform implementations
        ↓
platform-api
        ↓
capture-core
   ↙          ↘
annotation   actions
    ↓
 render
```

实际依赖可根据 Rust 类型所有权稍作调整，但必须保持：
- UI-neutral；
- platform-neutral core；
- platform-specific implementation 隔离。

---

# 3. Geometry First

先实现并测试：

- PhysicalPoint
- PhysicalSize
- PhysicalRect
- ScaleFactor
- MonitorInfo
- intersection
- clamp
- translate
- negative virtual coordinates

为 mixed-DPI 做明确的 coordinate mapper contract。

不要让 UI logical coordinate 成为 Core 真值。

---

# 4. Capture Backend

建立 trait。

Windows 实现要能真的抓屏。

优先研究：
- Windows.Graphics.Capture
- DXGI / D3D11
- 可维护 Rust crate 或适当 native bindings

允许为了 Demo 选择现有维护良好的 crate，但：
- 包装在 `capture-windows`；
- 不泄漏 crate 类型；
- 写 ADR 记录为什么；
- 记录正式项目是否建议继续用。

Linux 建立实际可编译 backend 路线。

至少保证后续两个 frontend 不需要自己做截图。

---

# 5. Snap Backend

Windows 至少做窗口级 candidate。

要求：
- visible top-level window；
- point hit testing；
- bounds；
- self-exclusion 接口；
- 可测试 candidate ranking。

将“排除本应用窗口”的机制做成 backend 可接收 frontend native window identity/opaque exclusion token 的形式，避免直接依赖某 UI toolkit。

---

# 6. Capture Session State

实现集中状态机。

输入建议：

```rust
pub enum CaptureCommand {
    Begin,
    FrameReady,
    PointerMoved(PhysicalPoint),
    BeginFreeSelection(PhysicalPoint),
    UpdateFreeSelection(PhysicalPoint),
    CommitSelection,
    MoveSelection(...),
    ResizeSelection(...),
    SelectTool(...),
    Undo,
    InvokeAction(ActionId),
    Cancel,
}
```

输出可以是：

```rust
pub enum CaptureEvent {
    StateChanged,
    SnapCandidateChanged,
    DocumentChanged,
    ActionRequested,
    Completed,
    Error,
}
```

不要把 UI 渲染细节写进事件。

---

# 7. Annotation

实现最小真实 document：

- PenStroke
- Rectangle
- undo

并提供 final flatten renderer。

为 renderer 建 golden tests。

UI preview 不属于这里，但 final output 属于这里。

---

# 8. Toolbar Placement

按共通规格实现并做 property/unit tests。

特别验证：
- tiny work area；
- negative monitor origin；
- selection larger than available placement；
- clamp。

---

# 9. Actions

实现：
- Copy payload/clipboard backend abstraction；
- Save PNG；
- Pin payload；
- Ask AI Stub payload。

如果 clipboard 的最终写入必须依赖 frontend event loop，则把职责清楚拆成：
- Core 生成 action payload；
- frontend shell 执行 toolkit-specific dispatch。

不要为了追求“所有东西都 Core 做”造成错误线程模型。

---

# 10. CLI

`capture-cli` 是冻结 Core 前必须通过的验收工具。

至少：

```text
capture-cli monitors
capture-cli capture-monitor <id> --output <png>
capture-cli candidates-at <x> <y>
capture-cli test-toolbar-placement ...
```

Windows 真机运行并记录结果。

---

# 11. 测试

至少：

- geometry unit tests；
- negative coordinate；
- toolbar placement；
- capture session transition；
- annotation undo；
- render golden image；
- snap ranking；
- platform pure logic tests。

能做 property test 的 geometry/placement 优先做。

---

# 12. Core 冻结报告

完成后创建：

`CORE_BASELINE_REPORT.md`

包含：

- crate graph；
- Windows capture 实现；
- Linux 实现状态；
- native dependencies；
- unsafe blocks；
- benchmark；
- known issues；
- Core API；
- 两个 UI frontend 需要如何接入；
- native window exclusion 如何传入；
- frozen commit hash。

然后建议创建 tag：

```text
demo-core-v1
```

不要继续实现 UI。

你的完成条件不是“截图器能用了”，而是：

> 两个独立 UI Agent 可以从完全相同的 Core API 开始工作。
