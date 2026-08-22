//! `capture-cli` — the frozen-Core acceptance tool.
//!
//! This binary exists so the shared Core can be verified before any UI exists,
//! and so backend problems can be isolated from toolkit problems.

use arboard::{Clipboard, ImageData};
use capture_actions::{
    action_by_id, AskAiAction, CaptureAction, CopyAction, PinAction, SaveAction,
};
use capture_annotation::{
    Annotation, AnnotationTool, CaptureCommand, CaptureDocument, CaptureSession, Color, PenStroke,
    RectShape,
};
use capture_core::{ActionId, MonitorId, PhysicalPoint, PhysicalRect};
use capture_platform_api::{CaptureBackend, SnapBackend};
use capture_render::{checksum, flatten, save_png, RenderedImage};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(target_os = "linux")]
use capture_linux::LinuxPlatform;
#[cfg(windows)]
use capture_windows::WindowsPlatform;

#[derive(Parser)]
#[command(
    name = "capture-cli",
    version,
    about = "Acceptance tool for the frozen screenshot Core",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List connected monitors (physical bounds, scale factor, primary).
    Monitors,
    /// Capture a monitor's pixels and write a PNG.
    CaptureMonitor {
        #[arg(help = "stable monitor id from `monitors`, or its listed ordinal")]
        id: u64,
        #[arg(long, help = "output PNG; defaults to ./capture-monitor-<id>.png")]
        output: Option<PathBuf>,
    },
    /// Capture one atomic physical frame of the complete virtual desktop.
    CaptureVirtualDesktop {
        #[arg(long, help = "output PNG; defaults to ./capture-virtual-desktop.png")]
        output: Option<PathBuf>,
    },
    /// Report the snap candidates under a point.
    CandidatesAt {
        x: i32,
        y: i32,
        #[arg(long, help = "exclude the given HWND value (as u64)")]
        exclude: Option<u64>,
    },
    /// Run the toolbar-placement battery (or one placement) and print results.
    TestToolbarPlacement {
        #[arg(
            long,
            num_args = 4,
            allow_hyphen_values = true,
            value_name = "x y w h",
            help = "run one placement: selection"
        )]
        selection: Option<Vec<i32>>,
        #[arg(long, num_args = 2, value_name = "w h", help = "toolbar size")]
        toolbar: Option<Vec<u32>>,
        #[arg(long, help = "preferred gap (default 8)")]
        gap: Option<u32>,
    },
    /// Flatten a synthetic annotated document to PNG, print dims + checksum.
    RenderTest {
        #[arg(long, default_value = "render-test.png")]
        output: PathBuf,
    },
    /// Drive the real session flow (capture → select → annotate → render).
    SessionTest {
        #[arg(default_value_t = 0)]
        monitor: u64,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Copy: produce the PNG payload; optionally write to the OS clipboard.
    Copy {
        #[arg(default_value_t = 0)]
        monitor: u64,
        #[arg(long)]
        clipboard: bool,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Save: write the flattened document to a PNG using the shared Save action.
    Save { monitor: u64, output: PathBuf },
    /// Pin: produce the Pin payload (frontend would open the Pin window).
    Pin { monitor: u64, output: PathBuf },
    /// AskAI: produce the payload to hand to an external consumer (stub).
    AskAi { monitor: u64, output: PathBuf },
    /// Validate the Core API version.
    SelfTest,
}

/// Hold the concrete platform chosen at compile time.
enum HostPlatform {
    #[cfg(windows)]
    Windows(WindowsPlatform),
    #[cfg(target_os = "linux")]
    Linux(LinuxPlatform),
}

impl HostPlatform {
    fn capture(&self) -> &dyn CaptureBackend {
        match self {
            #[cfg(windows)]
            HostPlatform::Windows(p) => p.capture_backend(),
            #[cfg(target_os = "linux")]
            HostPlatform::Linux(p) => p.capture_backend(),
        }
    }

    fn snap(&self) -> &dyn SnapBackend {
        match self {
            #[cfg(windows)]
            HostPlatform::Windows(p) => p.snap_backend(),
            #[cfg(target_os = "linux")]
            HostPlatform::Linux(p) => p.snap_backend(),
        }
    }
}

fn make_host() -> Result<HostPlatform, String> {
    #[cfg(windows)]
    {
        return WindowsPlatform::new()
            .map(HostPlatform::Windows)
            .map_err(|error| error.to_string());
    }
    #[cfg(target_os = "linux")]
    {
        return Ok(HostPlatform::Linux(LinuxPlatform::new()));
    }
    #[allow(unreachable_code)]
    Err("capture-cli has no backend for this target".to_string())
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Monitors => cmd_monitors(),
        Command::CaptureMonitor { id, output } => cmd_capture(id, output),
        Command::CaptureVirtualDesktop { output } => cmd_capture_virtual_desktop(output),
        Command::CandidatesAt { x, y, exclude } => cmd_candidates(x, y, exclude),
        Command::TestToolbarPlacement {
            selection,
            toolbar,
            gap,
        } => cmd_test_toolbar((selection, toolbar, gap)),
        Command::RenderTest { output } => cmd_render_test(output),
        Command::SessionTest { monitor, output } => cmd_session_test(monitor, output),
        Command::Copy {
            monitor,
            clipboard,
            output,
        } => cmd_copy(monitor, clipboard, output),
        Command::Save { monitor, output } => cmd_save(monitor, output),
        Command::Pin { monitor, output } => cmd_pin(monitor, output),
        Command::AskAi { monitor, output } => cmd_ask_ai(monitor, output),
        Command::SelfTest => cmd_self_test(),
    }
}

fn cmd_monitors() -> Result<(), String> {
    let host = make_host()?;
    let backends = host.capture();
    let caps = backends.capabilities();
    println!(
        "capabilities: multi_monitor={} per_monitor_dpi={} virtual_desktop={} capture_window={}",
        caps.multi_monitor, caps.per_monitor_dpi, caps.capture_virtual_desktop, caps.capture_window
    );
    let monitors = backends.monitors().map_err(|e| format!("{e}"))?;
    println!("monitors: {}", monitors.len());
    for (ordinal, m) in monitors.iter().enumerate() {
        println!(
            "  [ordinal={ordinal} id={}] {} bounds=({},{} {}x{}) work_area=({},{} {}x{}) scale={} primary={}",
            m.id.index(),
            m.name,
            m.bounds.origin.x,
            m.bounds.origin.y,
            m.bounds.size.width,
            m.bounds.size.height,
            m.work_area.origin.x,
            m.work_area.origin.y,
            m.work_area.size.width,
            m.work_area.size.height,
            m.scale_factor.get(),
            m.is_primary
        );
    }
    Ok(())
}

fn cmd_capture(id: u64, output: Option<PathBuf>) -> Result<(), String> {
    let host = make_host()?;
    let backend = host.capture();
    let monitor_id = resolve_monitor_id(backend, id)?;
    let frame = backend
        .capture_monitor(monitor_id)
        .map_err(|e| format!("{e}"))?;
    frame.validate().map_err(|e| format!("{e}"))?;
    let frame = frame.to_rgba8().map_err(|error| error.to_string())?;
    let image = RenderedImage::new(frame.width, frame.height, frame.pixels.to_vec());
    let path = output.unwrap_or_else(|| PathBuf::from(format!("./capture-monitor-{id}.png")));
    save_png(&path, &image).map_err(|e| format!("{e}"))?;
    println!(
        "captured monitor {id}: {}x{} origin=({},{}) format=RGBA8 -> {}",
        frame.width,
        frame.height,
        frame.origin.x,
        frame.origin.y,
        path.display()
    );
    Ok(())
}

fn cmd_capture_virtual_desktop(output: Option<PathBuf>) -> Result<(), String> {
    let host = make_host()?;
    let frame = host
        .capture()
        .capture_virtual_desktop()
        .map_err(|error| error.to_string())?;
    frame.validate().map_err(|error| error.to_string())?;
    let frame = frame.to_rgba8().map_err(|error| error.to_string())?;
    let image = RenderedImage::new(frame.width, frame.height, frame.pixels.to_vec());
    let path = output.unwrap_or_else(|| PathBuf::from("./capture-virtual-desktop.png"));
    save_png(&path, &image).map_err(|error| error.to_string())?;
    println!(
        "captured virtual desktop: {}x{} origin=({},{}) -> {}",
        frame.width,
        frame.height,
        frame.origin.x,
        frame.origin.y,
        path.display()
    );
    Ok(())
}

fn cmd_candidates(x: i32, y: i32, exclude: Option<u64>) -> Result<(), String> {
    let host = make_host()?;
    let snap = host.snap();
    if let Some(hwnd) = exclude {
        snap.set_excluded_window(Some(capture_core::SnapExclusionToken::new(hwnd)));
    }
    let point = PhysicalPoint::new(x, y);
    let cands = snap.candidates_at(point).map_err(|e| format!("{e}"))?;
    println!("candidates-at ({x},{y}): {}", cands.len());
    for c in &cands {
        println!(
            "  id={} z_order={} kind={:?} bounds=({},{},{}x{}) label={:?}",
            c.id.get(),
            c.z_order,
            c.kind,
            c.bounds.origin.x,
            c.bounds.origin.y,
            c.bounds.size.width,
            c.bounds.size.height,
            c.label
        );
    }
    Ok(())
}

fn cmd_test_toolbar(args: (Option<Vec<i32>>, Option<Vec<u32>>, Option<u32>)) -> Result<(), String> {
    let (sel, tb, gap) = args;
    if let (Some(sel), Some(tb)) = (sel.clone(), tb.clone()) {
        if sel.len() != 4 || tb.len() != 2 {
            return Err("--selection needs 4 ints, --toolbar needs 2 ints".to_string());
        }
        let selection = PhysicalRect::new(
            PhysicalPoint::new(sel[0], sel[1]),
            capture_core::PhysicalSize::new(sel[2] as u32, sel[3] as u32),
        );
        let toolbar = capture_core::PhysicalSize::new(tb[0], tb[1]);
        let work_area = PhysicalRect::new(
            PhysicalPoint::new(0, 0),
            capture_core::PhysicalSize::new(1920, 1040),
        );
        let p = capture_core::place_toolbar(selection, toolbar, work_area, gap.unwrap_or(8));
        println!(
            "placement: rect=({},{},{}x{}) reason={:?}",
            p.rect.origin.x, p.rect.origin.y, p.rect.size.width, p.rect.size.height, p.reason
        );
        return Ok(());
    }
    // No explicit args → run the share of the battery that mirrors unit tests.
    let mut all_ok = true;
    let mut check = |name: &str, ok: bool, msg: String| {
        println!("  [{}] {name}: {msg}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            all_ok = false;
        }
    };

    let work = || {
        PhysicalRect::new(
            PhysicalPoint::new(0, 0),
            capture_core::PhysicalSize::new(1920, 1040),
        )
    };
    let tb = |w: u32, h: u32| capture_core::PhysicalSize::new(w, h);

    // below in middle
    {
        let sel = PhysicalRect::new(
            PhysicalPoint::new(400, 300),
            capture_core::PhysicalSize::new(800, 300),
        );
        let p = capture_core::place_toolbar(sel, tb(320, 40), work(), 8);
        check(
            "below_middle",
            p.reason == capture_core::ToolbarPlacementReason::Below
                && p.rect.origin.y == sel.bottom() + 8,
            format!("{:?}", p),
        );
    }
    // above at bottom
    {
        let sel = PhysicalRect::new(
            PhysicalPoint::new(400, 900),
            capture_core::PhysicalSize::new(800, 100),
        );
        let p = capture_core::place_toolbar(sel, tb(320, 40), work(), 8);
        check(
            "above_bottom",
            p.reason == capture_core::ToolbarPlacementReason::Above,
            format!("{:?}", p),
        );
    }
    // inside bottom for full-screen
    {
        let sel = PhysicalRect::new(
            PhysicalPoint::new(0, 0),
            capture_core::PhysicalSize::new(1920, 1040),
        );
        let p = capture_core::place_toolbar(sel, tb(320, 40), work(), 8);
        check(
            "inside_fullscreen",
            p.reason == capture_core::ToolbarPlacementReason::InsideBottom,
            format!("{:?}", p),
        );
    }
    // tiny selection
    {
        let sel = PhysicalRect::new(
            PhysicalPoint::new(960, 500),
            capture_core::PhysicalSize::new(2, 2),
        );
        let p = capture_core::place_toolbar(sel, tb(320, 40), work(), 8);
        check(
            "tiny_below",
            p.reason == capture_core::ToolbarPlacementReason::Below,
            format!("{:?}", p),
        );
    }
    // negative-origin monitor
    {
        let wa = PhysicalRect::new(
            PhysicalPoint::new(-1920, 0),
            capture_core::PhysicalSize::new(1920, 1040),
        );
        let sel = PhysicalRect::new(
            PhysicalPoint::new(-1600, 400),
            capture_core::PhysicalSize::new(600, 200),
        );
        let p = capture_core::place_toolbar(sel, tb(320, 40), wa, 8);
        let in_bounds = p.rect.origin.x >= wa.origin.x
            && p.rect.right() <= wa.right()
            && p.rect.origin.y >= wa.origin.y
            && p.rect.bottom() <= wa.bottom();
        check(
            "negative_origin_inside",
            p.reason == capture_core::ToolbarPlacementReason::Below && in_bounds,
            format!("{:?}", p),
        );
    }
    if all_ok {
        println!("toolbar placement battery: ALL PASS");
        Ok(())
    } else {
        Err("toolbar placement battery: FAILURES".to_string())
    }
}

fn cmd_render_test(output: PathBuf) -> Result<(), String> {
    let frame = make_test_frame(96, 64, 0x3C);
    let mut doc = CaptureDocument::new(
        Arc::new(frame),
        PhysicalRect::new(
            PhysicalPoint::new(8, 8),
            capture_core::PhysicalSize::new(80, 48),
        ),
    );
    doc.annotations.push(Annotation::Rectangle(RectShape::new(
        PhysicalRect::new(
            PhysicalPoint::new(12, 12),
            capture_core::PhysicalSize::new(40, 24),
        ),
        Color::BLUE,
        3,
        Some(Color::new(255, 0, 0, 128)),
    )));
    doc.annotations.push(Annotation::Pen(PenStroke {
        color: Color::YELLOW,
        thickness: 4,
        points: vec![
            PhysicalPoint::new(20, 20),
            PhysicalPoint::new(60, 20),
            PhysicalPoint::new(60, 50),
        ],
    }));
    let image = flatten(&doc).map_err(|e| format!("{e}"))?;
    save_png(&output, &image).map_err(|e| format!("{e}"))?;
    println!(
        "render-test: {}x{} checksum=0x{:X} -> {}",
        image.width,
        image.height,
        checksum(&image),
        output.display()
    );
    Ok(())
}

fn cmd_session_test(monitor: u64, output: Option<PathBuf>) -> Result<(), String> {
    let host = make_host()?;
    let backend = host.capture();
    let mut session = CaptureSession::new();
    session.apply(CaptureCommand::Begin);
    let monitor_id = resolve_monitor_id(backend, monitor)?;
    let frame = backend
        .capture_monitor(monitor_id)
        .map_err(|e| format!("{e}"))?;
    session.apply(CaptureCommand::FrameReady(frame));
    // Commit the whole monitor as the selection.
    session.apply(CaptureCommand::CommitSelection);
    // Draw a small annotation via the session.
    let sel_center = match session.state() {
        capture_annotation::CaptureSessionState::Editing(ed) => ed.document.crop.center(),
        _ => return Err("session did not reach Editing".to_string()),
    };
    session.apply(CaptureCommand::SelectTool(AnnotationTool::Rectangle));
    session.apply(CaptureCommand::BeginAnnotation(sel_center));
    session.apply(CaptureCommand::UpdateAnnotation(PhysicalPoint::new(
        sel_center.x + 80,
        sel_center.y + 60,
    )));
    session.apply(CaptureCommand::EndAnnotation);

    let document = match session.state() {
        capture_annotation::CaptureSessionState::Editing(ed) => ed.document.clone(),
        _ => return Err("session did not reach Editing".to_string()),
    };
    let image = flatten(&document).map_err(|e| format!("{e}"))?;
    let path = output.unwrap_or_else(|| PathBuf::from("./session-test.png"));
    save_png(&path, &image).map_err(|e| format!("{e}"))?;
    let lat = session
        .timing()
        .capture_latency()
        .map(|d| format!("{d:?}"))
        .unwrap_or_else(|| "n/a".to_string());
    println!(
        "session-test: monitor={monitor} -> {}x{} annotations={} checksum=0x{:X} capture_latency={} -> {}",
        image.width,
        image.height,
        document.annotations.len(),
        checksum(&image),
        lat,
        path.display()
    );
    Ok(())
}

fn cmd_copy(monitor: u64, clipboard: bool, output: Option<PathBuf>) -> Result<(), String> {
    let host = make_host()?;
    let backend = host.capture();
    let monitor_id = resolve_monitor_id(backend, monitor)?;
    let frame = backend
        .capture_monitor(monitor_id)
        .map_err(|e| format!("{e}"))?;
    let doc = doc_from_frame(frame)?;
    let outcome = CopyAction.invoke(&doc).map_err(|e| format!("{e}"))?;
    let payload = match outcome {
        capture_actions::ActionOutcome::Png(p) => p,
        _ => return Err("copy expected a Png payload".to_string()),
    };
    let path = output.unwrap_or_else(|| PathBuf::from("./copy-payload.png"));
    std::fs::write(&path, &payload.png_bytes).map_err(|e| format!("{e}"))?;
    println!(
        "copy: payload {}x{} {} bytes PNG -> {}",
        payload.width,
        payload.height,
        payload.png_bytes.len(),
        path.display()
    );
    if clipboard {
        // Demonstrate clipboard dispatch: write raw RGBA.
        let image = flatten(&doc).map_err(|e| format!("{e}"))?;
        match set_clipboard(image) {
            Ok(()) => println!("copy: clipboard -> OK (RGBA {})", path.display()),
            Err(e) => println!("copy: clipboard -> unavailable ({e}); payload still written"),
        }
    }
    Ok(())
}

fn cmd_save(monitor: u64, output: PathBuf) -> Result<(), String> {
    let host = make_host()?;
    let backend = host.capture();
    let monitor_id = resolve_monitor_id(backend, monitor)?;
    let frame = backend
        .capture_monitor(monitor_id)
        .map_err(|e| format!("{e}"))?;
    let doc = doc_from_frame(frame)?;
    let outcome = SaveAction::new(&output)
        .invoke(&doc)
        .map_err(|e| format!("{e}"))?;
    match outcome {
        capture_actions::ActionOutcome::Saved(p) => println!("save: -> {}", p.display()),
        _ => return Err("save expected a Saved outcome".to_string()),
    }
    Ok(())
}

fn cmd_pin(monitor: u64, output: PathBuf) -> Result<(), String> {
    let host = make_host()?;
    let backend = host.capture();
    let monitor_id = resolve_monitor_id(backend, monitor)?;
    let frame = backend
        .capture_monitor(monitor_id)
        .map_err(|e| format!("{e}"))?;
    let doc = doc_from_frame(frame)?;
    let outcome = PinAction.invoke(&doc).map_err(|e| format!("{e}"))?;
    match outcome {
        capture_actions::ActionOutcome::Pin(p) => {
            std::fs::write(&output, &p.png_bytes).map_err(|e| format!("{e}"))?;
            println!(
                "pin: payload {}x{} -> {}",
                p.width,
                p.height,
                output.display()
            );
        }
        _ => return Err("pin expected a Pin payload".to_string()),
    }
    Ok(())
}

fn cmd_ask_ai(monitor: u64, output: PathBuf) -> Result<(), String> {
    let host = make_host()?;
    let backend = host.capture();
    let monitor_id = resolve_monitor_id(backend, monitor)?;
    let frame = backend
        .capture_monitor(monitor_id)
        .map_err(|e| format!("{e}"))?;
    let doc = doc_from_frame(frame)?;
    let outcome = AskAiAction.invoke(&doc).map_err(|e| format!("{e}"))?;
    match outcome {
        capture_actions::ActionOutcome::AskAi(p) => {
            std::fs::write(&output, &p.png_bytes).map_err(|e| format!("{e}"))?;
            println!(
                "ask-ai: payload {}x{} (stub) -> {}",
                p.width,
                p.height,
                output.display()
            );
        }
        _ => return Err("ask-ai expected an AskAi payload".to_string()),
    }
    Ok(())
}

fn cmd_self_test() -> Result<(), String> {
    let host = make_host()?;
    let caps = host.capture().capabilities();
    println!("capture-cli self-test");
    println!("  core api version: {}", capture_core::CORE_API_VERSION);
    println!("  capture capabilities: {caps:?}");
    match action_by_id(ActionId::COPY) {
        Some(a) => println!("  action copy present: {}", a.id()),
        None => return Err("copy action missing".to_string()),
    }
    println!("  OK");
    Ok(())
}

fn resolve_monitor_id(backend: &dyn CaptureBackend, requested: u64) -> Result<MonitorId, String> {
    let monitors = backend.monitors().map_err(|error| error.to_string())?;
    if let Some(monitor) = monitors
        .iter()
        .find(|monitor| monitor.id.index() == requested)
    {
        return Ok(monitor.id);
    }
    if let Some(monitor) = monitors.get(requested as usize) {
        return Ok(monitor.id);
    }
    let available = monitors
        .iter()
        .enumerate()
        .map(|(ordinal, monitor)| format!("{ordinal}:{}", monitor.id.index()))
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "monitor {requested} not found; available ordinal:id values: {available}"
    ))
}

fn doc_from_frame(frame: capture_core::CapturedFrame) -> Result<CaptureDocument, String> {
    frame.validate().map_err(|error| error.to_string())?;
    let frame = frame.to_rgba8().map_err(|error| error.to_string())?;
    let bounds = frame.bounds();
    Ok(CaptureDocument::new(Arc::new(frame), bounds))
}

fn make_test_frame(w: u32, h: u32, fill: u8) -> capture_core::CapturedFrame {
    capture_core::CapturedFrame::new(
        vec![fill; (w * h * 4) as usize].into(),
        w,
        h,
        w * 4,
        PhysicalPoint::new(0, 0),
        capture_core::PixelFormat::Rgba8,
    )
}

fn set_clipboard(image: RenderedImage) -> Result<(), String> {
    let mut cb = Clipboard::new().map_err(|e| format!("{e}"))?;
    cb.set_image(ImageData {
        width: image.width as usize,
        height: image.height as usize,
        bytes: std::borrow::Cow::Owned(image.pixels),
    })
    .map_err(|e| format!("{e}"))
}
