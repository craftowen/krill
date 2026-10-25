//! Read-only capture geometry and explicit image-to-desktop evidence.

use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::Command;

use super::{
    AffineTransform, CaptureMapping, DisplayGeometry, GeometryWitness, ObservationClock,
    ObservationTiming, Point, Rect, ScreenshotMetadata, WindowSummary, now_ms, png_dimensions,
    retention, run_jxa, run_managed, unique_token, write_private_new,
};

pub(super) fn clock() -> ObservationClock {
    #[cfg(unix)]
    let monotonic_ns = {
        let time = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        u64::try_from(time.tv_sec).ok().and_then(|seconds| {
            seconds
                .checked_mul(1_000_000_000)?
                .checked_add(u64::try_from(time.tv_nsec).ok()?)
        })
    };
    #[cfg(not(unix))]
    let monotonic_ns = None;
    ObservationClock {
        wall_time_ms: now_ms(),
        monotonic_ns,
        monotonic_clock: monotonic_ns.map(|_| "CLOCK_MONOTONIC; same boot only".to_owned()),
    }
}

pub(super) fn timing(started: ObservationClock) -> ObservationTiming {
    ObservationTiming {
        started,
        finished: clock(),
    }
}

fn valid_rect(rect: Rect) -> bool {
    [
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        rect.x + rect.width,
        rect.y + rect.height,
    ]
    .iter()
    .all(|v| v.is_finite())
        && rect.width > 0.0
        && rect.height > 0.0
}

fn contains_rect(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}

fn witness(
    win: &WindowSummary,
    mut displays: Vec<DisplayGeometry>,
) -> Result<GeometryWitness, String> {
    if !valid_rect(win.bounds) || displays.is_empty() || displays.len() > 16 {
        return Err("invalid or unavailable bounded display geometry".to_owned());
    }
    displays.sort_by_key(|display| display.id);
    if displays.windows(2).any(|pair| pair[0].id == pair[1].id)
        || displays.iter().any(|display| {
            !valid_rect(display.logical_bounds)
                || display.pixel_width == 0
                || display.pixel_height == 0
                || !display.backing_scale_factor.is_finite()
                || display.backing_scale_factor <= 0.0
                || !display.rotation_degrees.is_finite()
        })
    {
        return Err("ambiguous or invalid display facts".to_owned());
    }
    let mut geometry = GeometryWitness {
        revision: String::new(),
        coordinate_space: "desktop_logical_top_left".to_owned(),
        window_id: win.id.clone(),
        window_bounds: win.bounds,
        displays,
    };
    let bytes = serde_json::to_vec(&geometry).map_err(|e| e.to_string())?;
    let hash = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    geometry.revision = format!("geometry-fnv1a64:{hash:016x}");
    Ok(geometry)
}

/// No AX traversal, activation, input, clipboard access, or permission prompts.
const DISPLAY_GEOMETRY_JXA: &str = concat!(
    include_str!("capture/geometry.js"),
    "\nfunction run(argv) {return JSON.stringify(captureGeometryFacts(argv[0],argv[1]));}"
);

fn parse_geometry(raw: serde_json::Value, win: &WindowSummary) -> Result<GeometryWitness, String> {
    #[derive(serde::Deserialize)]
    struct Facts {
        bounds: Rect,
        displays: Vec<DisplayGeometry>,
    }
    let facts: Facts = serde_json::from_value(raw).map_err(|e| format!("display facts: {e}"))?;
    if facts.bounds != win.bounds {
        return Err("window moved during observation; observe again".to_owned());
    }
    witness(win, facts.displays)
}

pub(super) fn observation_geometry(
    value: &serde_json::Value,
    win: &WindowSummary,
) -> Result<GeometryWitness, String> {
    if value
        .get("geometryError")
        .is_some_and(|error| !error.is_null())
    {
        return Err("display/window facts unavailable during AX observation".to_owned());
    }
    let before = parse_geometry(
        value
            .get("geometryBefore")
            .cloned()
            .ok_or("display facts unavailable")?,
        win,
    )?;
    let after = parse_geometry(
        value
            .get("geometryAfter")
            .cloned()
            .ok_or("display facts unavailable")?,
        win,
    )?;
    if before != after {
        return Err("window/display geometry changed during observation".to_owned());
    }
    Ok(after)
}

pub(super) fn query_geometry(win: &WindowSummary) -> Result<GeometryWitness, String> {
    let raw = run_jxa(DISPLAY_GEOMETRY_JXA, &[&win.pid.to_string(), &win.id])?;
    parse_geometry(
        serde_json::from_str(&raw).map_err(|e| format!("display facts: {e}"))?,
        win,
    )
}

/// A window wholly on exactly one display is the only supported affine capture.
/// Cross-display/mirrored/offscreen crops require additional evidence, not an average scale.
fn mapping(width: u32, height: u32, geometry: &GeometryWitness) -> Result<CaptureMapping, String> {
    if width == 0 || height == 0 || !valid_rect(geometry.window_bounds) {
        return Err("capture dimensions unavailable".to_owned());
    }
    let mut candidates = geometry
        .displays
        .iter()
        .filter(|display| contains_rect(display.logical_bounds, geometry.window_bounds));
    let display = candidates
        .next()
        .ok_or("window is not contained by one display; mapping unknown")?;
    if candidates.next().is_some() || display.rotation_degrees != 0.0 {
        return Err("ambiguous or rotated display; mapping unknown".to_owned());
    }
    let bounds = geometry.window_bounds;
    let scale = display.backing_scale_factor;
    // Exact pixel extent must agree with the independently observed backing scale.
    // Do not infer a crop or resize merely because the PNG aspect ratio looks plausible.
    if !scale.is_finite()
        || scale <= 0.0
        || (bounds.width * scale - f64::from(width)).abs() > 0.001
        || (bounds.height * scale - f64::from(height)).abs() > 0.001
    {
        return Err("PNG extent disagrees with measured display scale; mapping unknown".to_owned());
    }
    let pixels = Rect {
        x: 0.0,
        y: 0.0,
        width: f64::from(width),
        height: f64::from(height),
    };
    Ok(CaptureMapping {
        source_pixels: pixels,
        crop_pixels: pixels,
        resize_scale: Point { x:1.0, y:1.0 },
        image_to_desktop: AffineTransform { xx:1.0 / scale, xy:0.0, yx:0.0,
            yy:1.0 / scale, tx:bounds.x, ty:bounds.y },
        display_id: display.id,
        evidence: "screencapture -l -o -a; PNG IHDR; stable CGWindow/NSScreen census before/after; no resize".to_owned(),
    })
}

fn directory() -> Result<PathBuf, String> {
    let dir =
        krill_proto::daemon::state_dir().ok_or("cannot resolve private capture state directory")?;
    let shots = dir.join("computer-captures-v2");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&shots).map_err(|e| e.to_string())?;
    let metadata = fs::symlink_metadata(&shots).map_err(|e| e.to_string())?;
    if !metadata.is_dir() {
        return Err("capture directory is not a real directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err("capture directory must be private and owned by this user".to_owned());
        }
    }
    Ok(shots)
}

pub(super) fn mock_screenshot(win: &WindowSummary) -> Result<ScreenshotMetadata, String> {
    // Real 1x1 PNG fixture. Synthetic pixels never assert a mapping to the mock window.
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    let started = clock();
    let shots = directory()?;
    let path = shots.join(format!("window-{}.png", unique_token()));
    write_private_new(&path, PNG)?;
    retention::prune(&shots, &path).map_err(|e| format!("capture retention: {e}"))?;
    let (width, height) = png_dimensions(PNG)?;
    Ok(ScreenshotMetadata {
        path: path.display().to_string(),
        width,
        height,
        scale_factor: 0.0,
        window_id: Some(win.id.clone()),
        logical_bounds: win.bounds,
        measured: false,
        mapping: None,
        timing: Some(timing(started)),
        geometry_revision: None,
    })
}

pub(super) fn live_screenshot(
    win: &WindowSummary,
    geometry: &GeometryWitness,
) -> Result<ScreenshotMetadata, String> {
    if std::env::var_os("KRILL_TEST_OFFLINE").is_some() {
        return mock_screenshot(win);
    }
    // Validate containment before acquiring any image.
    let scale = geometry
        .displays
        .iter()
        .find(|d| contains_rect(d.logical_bounds, win.bounds))
        .ok_or("capture mapping unknown")?
        .backing_scale_factor;
    if query_geometry(win)? != *geometry {
        return Err("display topology changed before capture".to_owned());
    }
    let shots = directory()?;
    let path = shots.join(format!("window-{}.png", unique_token()));
    // A private create_new reservation prevents overwriting any existing capture.
    write_private_new(&path, &[])?;
    let mut cmd = Command::new("/usr/sbin/screencapture");
    cmd.args(["-x", "-o", "-a", "-l", &win.id]).arg(&path);
    let started = clock();
    let finished = match run_managed(cmd) {
        Ok(finished) => finished,
        Err(error) => {
            fs::remove_file(&path).ok();
            return Err(error);
        }
    };
    let capture_timing = timing(started);
    if !matches!(finished.end, super::super::lifecycle::End::Exited(status) if status.success())
        || !finished.reaped
    {
        if finished.reaped {
            fs::remove_file(&path).ok();
            retention::prune(&shots, &path).ok();
        }
        return Err(format!(
            "screencapture failed: {:?}; reaped={}",
            finished.end, finished.reaped
        ));
    }
    let result = (|| {
        let after = query_geometry(win)?;
        if after != *geometry {
            return Err("window/display geometry changed during capture".to_owned());
        }
        let mut file = fs::File::open(&path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        let mut header = [0_u8; 24];
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        let (width, height) = png_dimensions(&header)?;
        let mapping = mapping(width, height, geometry)?;
        retention::prune(&shots, &path).map_err(|e| format!("capture retention: {e}"))?;
        Ok(ScreenshotMetadata {
            path: path.display().to_string(),
            width,
            height,
            scale_factor: scale,
            window_id: Some(win.id.clone()),
            logical_bounds: win.bounds,
            measured: true,
            mapping: Some(mapping),
            timing: Some(capture_timing),
            geometry_revision: Some(geometry.revision.clone()),
        })
    })();
    if result.is_err() {
        fs::remove_file(&path).ok();
        retention::prune(&shots, &path).ok();
    }
    result
}

#[cfg(test)]
mod tests;
