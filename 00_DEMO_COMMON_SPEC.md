# 截图器 UI 技术选型 Demo：共通规格

## 1. 目的

本实验只解决一个问题：

> 对一款以 Rust 为核心、Windows + Linux 为主要平台、macOS 可后续接入的现代 Overlay 截图器，Slint 与 Qt Quick/QML 哪个更适合作为 UI 层？

实验必须尽量控制变量。

共享部分只实现一次：

```text
Rust Core
├── capture domain
├── selection domain
├── annotation document
├── toolbar placement
├── capture backend abstraction
├── snap backend abstraction
├── Windows platform backend
├── Linux platform skeleton/backend
├── image flatten/render
└── actions
```

只有这些是实验变量：

```text
Frontend
├── UI toolkit
├── UI adapter
├── image bridge
├── native window integration
├── multi-window lifecycle
└── UI rendering / interaction implementation
```

---

# 2. 目标交互

产品交互参考 PixPin、QQ、Snipaste 这一类“原地 Overlay 截图器”。

基本路径：

```text
Ctrl + Alt + A
        ↓
抓取当前桌面快照
        ↓
全屏冻结画面 + 暗色遮罩
        ↓
鼠标移动显示窗口吸附候选
        ↓
单击候选 / 自由拖拽
        ↓
确认选区
        ↓
选区附近出现浮动工具栏
        ↓
原地标注
        ↓
Copy / Save / Pin / Ask AI Stub / Cancel
```

不进入独立图片编辑窗口。

---

# 3. 单仓库结构

建议：

```text
capture-ui-eval/
├── Cargo.toml
├── crates/
│   ├── capture-core/
│   ├── capture-platform-api/
│   ├── capture-windows/
│   ├── capture-linux/
│   ├── capture-annotation/
│   ├── capture-render/
│   └── capture-actions/
│
├── tools/
│   └── capture-cli/
│
└── apps/
    ├── capture-slint/
    └── capture-qml/
```

其中：

- `crates/*` 和 `tools/capture-cli` 由 Core 会话完成；
- `apps/capture-slint` 由 Slint 会话完成；
- `apps/capture-qml` 由 QML 会话完成。

---

# 4. Core 冻结规则

Core 完成后打 tag，例如：

```text
demo-core-v1
```

两个 UI 实现必须从完全相同的 Core commit 开始。

UI Agent 默认只允许修改自己的：

```text
apps/capture-slint/**
```

或：

```text
apps/capture-qml/**
```

如果 Core API 阻碍合理实现：

1. 不要私自在自己的 branch 改出不同 Core；
2. 创建 `CORE_CHANGE_REQUEST.md`；
3. 说明：
   - 缺少什么；
   - 两个 frontend 是否都需要；
   - 为什么不能在 adapter 内解决；
   - 建议最小 API 改动；
4. Core 更新后两个 branch 同步到同一个新 tag。

---

# 5. Core 的真值模型

Core 不依赖任何 UI toolkit。

禁止 Core 类型出现：

```text
Slint Image / Model
QObject
QImage
QRect
QPoint
QML type
HWND
X11 Window
Wayland object
```

平台原生句柄只存在 platform crate 内部，或用 opaque ID 包装。

截图 Core 使用 physical pixel 坐标作为 canonical coordinate。

```rust
pub struct PhysicalPoint {
    pub x: i32,
    pub y: i32,
}

pub struct PhysicalSize {
    pub width: u32,
    pub height: u32,
}

pub struct PhysicalRect {
    pub origin: PhysicalPoint,
    pub size: PhysicalSize,
}

pub struct ScaleFactor(pub f64);
```

虚拟桌面 origin 必须允许负数。

---

# 6. Capture Frame

建议：

```rust
pub struct CapturedFrame {
    pub pixels: std::sync::Arc<[u8]>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub origin: PhysicalPoint,
    pub pixel_format: PixelFormat,
}
```

Demo 可统一 RGBA8。

UI frontend 必须自己实现：

```text
CapturedFrame
    ↓
Frontend image adapter
    ↓
Slint image / Qt image/texture
```

这部分是重要实验变量，不能在 Core 里替它们抹平。

---

# 7. Capture Backend

共享：

```rust
pub trait CaptureBackend: Send + Sync {
    fn capabilities(&self) -> CaptureCapabilities;

    fn monitors(
        &self
    ) -> Result<Vec<MonitorInfo>, CaptureError>;

    fn capture_monitor(
        &self,
        id: MonitorId
    ) -> Result<CapturedFrame, CaptureError>;
}
```

Demo 重点是 UI 比较，因此 CaptureBackend 不要求一开始就达到最终生产质量，但必须：

- 两个 frontend 完全共享；
- 不通过 Qt/Slint 自己的屏幕抓取 API 实现；
- 对 UI 暴露统一 `CapturedFrame`。

---

# 8. Smart Snap

共享：

```rust
pub trait SnapBackend: Send + Sync {
    fn capabilities(&self) -> SnapCapabilities;

    fn candidates_at(
        &self,
        point: PhysicalPoint
    ) -> Result<Vec<SnapCandidate>, SnapError>;
}
```

Demo 在 Windows 至少完成顶层窗口吸附：

- 可见窗口；
- point-under-window；
- 视觉边界；
- 排除截图器自身窗口；
- hover 高亮；
- 单击选中。

UI 只消费：

```rust
pub struct SnapCandidate {
    pub id: SnapCandidateId,
    pub bounds: PhysicalRect,
    pub kind: SnapKind,
    pub label: Option<String>,
}
```

---

# 9. Selection

Core 管状态：

```rust
pub enum CaptureSessionState {
    Idle,
    Preparing,
    Selecting(SelectionSession),
    Editing(EditorSession),
}
```

Selection 需要支持：

- hover snap candidate；
- free drag；
- move；
- 8 个 resize handles；
- commit；
- cancel。

UI 负责 pointer/keyboard 事件采集和视觉反馈。

不要把 capture session 做成散落在 UI 的十几个 bool。

---

# 10. Annotation

共享 document：

```rust
pub struct CaptureDocument {
    pub source: std::sync::Arc<CapturedFrame>,
    pub crop: PhysicalRect,
    pub annotations: Vec<Annotation>,
}
```

Demo 至少实现：

```rust
pub enum Annotation {
    Pen(PenStroke),
    Rectangle(RectShape),
}
```

并支持 Undo。

UI 可以自己画 editing preview。

最终 Copy/Save/Pin/Ask AI Stub 使用共享 `capture-render` flatten。

这样可以比较两套 UI 的实时绘制体验，同时保证最终图片算法相同。

---

# 11. Toolbar Placement

共享计算，不让两个 frontend 各写一套：

```rust
pub fn place_toolbar(
    selection: PhysicalRect,
    toolbar_size: PhysicalSize,
    work_area: PhysicalRect,
    preferred_gap: u32,
) -> ToolbarPlacement;
```

基本策略：

1. 选区下方；
2. 下方空间不足 → 上方；
3. 仍不足 → 选区内部靠下；
4. 必要时 clamp 到当前 work area。

需要单元测试：
- 选区贴底；
- 贴顶；
- 全屏选区；
- 极小选区；
- 负坐标显示器。

---

# 12. Actions

共享接口：

```rust
pub trait CaptureAction: Send + Sync {
    fn id(&self) -> &'static str;

    fn invoke(
        &self,
        document: &CaptureDocument
    ) -> Result<(), ActionError>;
}
```

Demo 至少：

- Copy
- Save
- Pin payload generation
- Ask AI Stub

Pin 的“生成最终图片”共享，但 Pin Window 本身是 UI frontend 的责任，因为多窗口/window flags 是技术栈比较内容。

Ask AI Stub 只需要证明：
- 当前图片能通过 Action 边界交给外部消费者；
- 不要求真实模型调用。

---

# 13. Core CLI

为了先验证共享部分，必须提供：

```text
capture-cli
```

至少支持类似：

```bash
capture-cli monitors
capture-cli capture-monitor 0 --output out.png
capture-cli candidates-at 1200 600
capture-cli toolbar-placement ...
```

目的：
- UI 尚不存在时就能测试 backend；
- 排除“UI 不工作所以误判 Core”的情况；
- 两个 frontend 出问题时容易定位。

---

# 14. UI Frontend 必须完成的功能

两个 frontend 功能完全一致：

## Overlay

- 无边框；
- 全屏；
- 置顶；
- frozen screenshot background；
- 暗色 mask；
- pointer interaction。

## Selection

- snap hover highlight；
- free drag；
- resize handles；
- move；
- commit/cancel。

## Toolbar

至少：
- Pen
- Rectangle
- Undo
- Copy
- Save
- Pin
- Ask AI
- Cancel

要求：
- 跟随 Core 给出的 placement；
- 图标按钮；
- hover/pressed feedback；
- 视觉干净；
- 不使用 toolkit 默认按钮简单堆起来糊弄。

## Pin

- 独立无边框窗口；
- always-on-top；
- drag；
- close。

---

# 15. 多显示器与 DPI

这是实验重点。

至少测试：

1. Windows 单屏 100%；
2. Windows 125% 或 150%；
3. 双屏；
4. 条件允许时测试混合 DPI；
5. 条件允许时让副屏位于主屏左边，验证 negative virtual coordinate。

记录：
- screenshot physical bounds；
- toolkit logical bounds；
- scale factor；
- coordinate conversion；
- selection 是否发生偏移；
- toolbar 是否定位正确。

---

# 16. Linux

目标是验证 UI toolkit 对 Linux desktop/windowing 的适应度，不要求 Demo 阶段把全部平台行为做到正式产品级。

至少做到：

- Linux 可构建；
- X11 可实际运行 Overlay；
- Wayland 路线能实际启动或给出最小技术验证；
- 明确记录 Wayland 下窗口定位/置顶/Portal 等影响。

共享 platform crate 可以先提供足够 Demo 使用的 Linux backend，但两个 frontend 必须使用同一个 backend。

---

# 17. 实验中不能共享的部分

这些必须分别实现，因为就是我们要比较的变量：

```text
Frontend window creation
transparent / frameless / topmost integration
frontend-native window handle
multiple-window lifecycle
Pin Window
image bridge
UI image upload
toolbar rendering
selection visual rendering
annotation preview rendering
animation
input dispatch adapter
first-present measurement
```

---

# 18. 统一性能打点

共享 Core 提供时间点：

```text
T0 global hotkey received
T1 captured frame ready
```

Frontend 提供：

```text
T2 frontend receives frame
T3 overlay show requested
T4 first usable/presented frame
```

至少报告：

```text
capture latency = T1 - T0
frontend adaptation = T3 - T2
hotkey-to-overlay ≈ T4 - T0
```

如果 T4 无法精确测量，说明测量方式和误差。

还记录：

- idle RSS；
- Overlay active RSS；
- 第一次与第二次打开差异；
- 4K frame 的 image copy 次数；
- frontend image conversion 次数；
- release build 体积。

---

# 19. Build / Tooling 指标

两个 frontend 分别记录：

- clean build；
- incremental build；
- 第一次环境准备；
- 日常开发主命令；
- IDE/LSP；
- CI complexity；
- packaging/deployment；
- 系统依赖；
- hand-written C++ LOC；
- build.rs LOC；
- CMake LOC；
- unsafe block；
- workaround 数量。

---

# 20. UI 技术评分

建议：

| 维度 | 权重 |
|---|---:|
| Overlay / 多屏 / DPI 稳定性 | 25 |
| Native window integration | 20 |
| UI 开发效率与表达力 | 15 |
| Image bridge / runtime 性能 | 15 |
| Rust 工程体验 | 10 |
| 构建 / CI / 部署 | 10 |
| 长期生态风险 | 5 |

Core 的截图性能不参与 Slint/QML 胜负，因为它是共享变量。

---

# 21. Snow Shot 的定位

Snow Shot 是研究参考，不是本实验的上游产品。

可重点研究当前仓库的：

```text
snow-crates/crates/snow-capture
snow-crates/crates/snow-ui-selector
snow_draw_engine_qt
```

仓库：

https://github.com/mg-chao/snow-apps

研究重点：
- Windows capture backend；
- window / element candidate；
- geometry；
- performance；
- drawing/document 思路。

是否复用具体代码，在正式项目里单独做许可证和质量审查。

---

# 22. 官方技术资料

## Slint
- https://docs.slint.dev/latest/docs/slint/guide/platforms/desktop/
- https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_winit/
- https://docs.slint.dev/latest/docs/rust/slint/struct.WindowHandle

## CXX-Qt
- https://kdab.github.io/cxx-qt/book/
- https://kdab.github.io/cxx-qt/book/concepts/build_systems.html

## Qt
- https://doc.qt.io/qt-6/qt.html
- https://doc.qt.io/qt-6/qtquick-window-example.html

## XDG Portal
- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html
- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html

## Windows
- https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscaptureitem
- https://learn.microsoft.com/en-us/windows/win32/winauto/entry-uiauto-win32

---

# 23. 完成标准

Core：
- CLI 可验证；
- tests 通过；
- 产生 frozen tag。

Slint：
- 完成功能；
- `TECH_REPORT_SLINT.md`。

QML：
- 完成功能；
- `TECH_REPORT_QML.md`。

最后人工 A/B 日用一段时间，再做技术选择。

不要仅根据 LOC 或 benchmark 单项决定。
