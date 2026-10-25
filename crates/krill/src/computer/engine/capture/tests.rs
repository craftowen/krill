use super::*;

// These fixtures use exactly representable integer/half-integer affine coefficients.
fn assert_exact(actual: f64, expected: f64) {
    assert_eq!(actual.to_bits(), expected.to_bits());
}

fn window(bounds: Rect) -> WindowSummary {
    WindowSummary {
        id: "42".to_owned(),
        index: 1,
        title: "fixture".to_owned(),
        app_name: "fixture".to_owned(),
        bundle_id: None,
        pid: 7777,
        bounds,
        is_focused: true,
        is_main: true,
        is_minimized: false,
    }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn displays() -> Vec<DisplayGeometry> {
    vec![
        DisplayGeometry {
            id: 1,
            logical_bounds: rect(0.0, 0.0, 1440.0, 900.0),
            pixel_width: 2880,
            pixel_height: 1800,
            backing_scale_factor: 2.0,
            rotation_degrees: 0.0,
        },
        DisplayGeometry {
            id: 2,
            logical_bounds: rect(-1920.0, -120.0, 1920.0, 1080.0),
            pixel_width: 1920,
            pixel_height: 1080,
            backing_scale_factor: 1.0,
            rotation_degrees: 0.0,
        },
    ]
}

#[test]
fn mixed_display_layout_uses_containing_display_and_negative_desktop_origin() {
    let win = window(rect(-1800.0, -80.0, 800.0, 600.0));
    let geometry = witness(&win, displays()).unwrap();
    let image = mapping(800, 600, &geometry).unwrap();
    assert_eq!(image.display_id, 2);
    let transform = image.image_to_desktop;
    assert_exact(transform.tx, -1800.0);
    assert_exact(transform.ty, -80.0);
    assert_exact(transform.xx, 1.0);
    assert_eq!(image.source_pixels, image.crop_pixels);
    assert_eq!(image.resize_scale, Point { x: 1.0, y: 1.0 });
    assert_exact(transform.xx * 800.0 + transform.tx, -1000.0);
}

#[test]
fn retina_affine_is_explicit_and_matches_both_pixel_axes() {
    let win = window(rect(100.0, 80.0, 800.0, 600.0));
    let geometry = witness(&win, displays()).unwrap();
    let image = mapping(1600, 1200, &geometry).unwrap();
    assert_eq!(image.display_id, 1);
    assert_exact(image.image_to_desktop.xx, 0.5);
    assert_exact(image.image_to_desktop.yy, 0.5);
    assert_exact(
        image.image_to_desktop.xx * 1600.0 + image.image_to_desktop.tx,
        900.0,
    );
    assert_exact(
        image.image_to_desktop.yy * 1200.0 + image.image_to_desktop.ty,
        680.0,
    );
    assert!(mapping(1600, 1199, &geometry).is_err());
    // A plausible aspect ratio does not prove a resize or backing scale.
    assert!(mapping(800, 600, &geometry).is_err());
}

#[test]
fn spanning_displays_offscreen_and_mirrored_geometry_fail_closed() {
    let span = witness(&window(rect(-50.0, 20.0, 200.0, 100.0)), displays()).unwrap();
    assert!(mapping(400, 200, &span).is_err());
    let offscreen = witness(&window(rect(1400.0, 20.0, 200.0, 100.0)), displays()).unwrap();
    assert!(mapping(400, 200, &offscreen).is_err());
    let mut mirrored = displays();
    let mut second = mirrored[0].clone();
    second.id = 3;
    mirrored.push(second);
    let geometry = witness(&window(rect(100.0, 80.0, 800.0, 600.0)), mirrored).unwrap();
    assert!(mapping(1600, 1200, &geometry).is_err());
}

#[test]
fn moved_windows_and_display_scale_layout_pixels_change_revision() {
    let win = window(rect(100.0, 80.0, 800.0, 600.0));
    let base = witness(&win, displays()).unwrap();
    let mut moved = win.clone();
    moved.bounds.x += 1.0;
    assert_ne!(base, witness(&moved, displays()).unwrap());
    assert_ne!(base.revision, witness(&moved, displays()).unwrap().revision);
    for change in 0..3 {
        let mut changed = displays();
        match change {
            0 => changed[1].backing_scale_factor = 2.0,
            1 => changed[1].logical_bounds.y += 10.0,
            _ => changed[1].pixel_width += 1,
        }
        assert_ne!(base.revision, witness(&win, changed).unwrap().revision);
    }
    let mut reordered = displays();
    reordered.reverse();
    assert_eq!(base, witness(&win, reordered).unwrap());
}

#[test]
fn invalid_and_unavailable_geometry_never_authorizes_mapping() {
    let win = window(rect(0.0, 0.0, 10.0, 10.0));
    assert!(witness(&win, vec![]).is_err());
    for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let mut bad = displays();
        bad[0].backing_scale_factor = invalid;
        assert!(witness(&win, bad).is_err());
        let mut bad_win = win.clone();
        bad_win.bounds.width = invalid;
        assert!(witness(&bad_win, displays()).is_err());
    }
    let mut duplicate = displays();
    duplicate.push(duplicate[0].clone());
    assert!(witness(&win, duplicate).is_err());
    let mut too_many = vec![displays()[0].clone(); 17];
    for (index, display) in too_many.iter_mut().enumerate() {
        display.id = u32::try_from(index).unwrap();
    }
    assert!(witness(&win, too_many).is_err());
}

#[test]
fn timing_is_bracketed_by_monotonic_clock_and_wall_time_is_separate() {
    let value = timing(clock());
    #[cfg(unix)]
    {
        assert!(value.finished.monotonic_ns.unwrap() >= value.started.monotonic_ns.unwrap());
        assert_eq!(
            value.started.monotonic_clock.as_deref(),
            Some("CLOCK_MONOTONIC; same boot only")
        );
    }
    let json = serde_json::to_value(value).unwrap();
    assert!(json["started"]["wallTimeMs"].is_number());
    assert!(json["finished"]["wallTimeMs"].is_number());
}

#[test]
fn old_screenshot_metadata_has_no_implicit_mapping_or_timestamp() {
    let old = serde_json::json!({"path":"fixture.png","width":1600,"height":1200,
        "scaleFactor":2.0,"windowId":"42","logicalBounds":rect(100.0,80.0,800.0,600.0),"measured":true});
    let shot: ScreenshotMetadata = serde_json::from_value(old).unwrap();
    assert!(shot.mapping.is_none());
    assert!(shot.timing.is_none());
    assert!(shot.geometry_revision.is_none());
}

#[test]
fn png_ihdr_length_and_positive_dimensions_are_required() {
    let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    header.extend_from_slice(&1_u32.to_be_bytes());
    header.extend_from_slice(&1_u32.to_be_bytes());
    for length in 0..header.len() {
        assert!(png_dimensions(&header[..length]).is_err());
    }
    assert_eq!(png_dimensions(&header).unwrap(), (1, 1));
    header[11] = 12;
    assert!(png_dimensions(&header).is_err());
    header[11] = 13;
    header[19] = 0;
    assert!(png_dimensions(&header).is_err());
}

#[test]
fn observation_requires_two_complete_matching_witnesses() {
    let win = window(rect(100.0, 80.0, 800.0, 600.0));
    let facts = serde_json::json!({"bounds":win.bounds,"displays":displays()});
    let mut observation = serde_json::json!({"geometryBefore":facts,"geometryAfter":facts});
    assert!(observation_geometry(&observation, &win).is_ok());
    observation["geometryAfter"]["displays"][0]["backingScaleFactor"] = serde_json::json!(1.0);
    assert!(observation_geometry(&observation, &win).is_err());
    observation["geometryAfter"] = serde_json::Value::Null;
    assert!(observation_geometry(&observation, &win).is_err());
    assert!(observation_geometry(&serde_json::json!({}), &win).is_err());
    observation["geometryAfter"] = facts;
    observation["geometryBefore"]["bounds"]["x"] = serde_json::json!(101.0);
    assert!(observation_geometry(&observation, &win).is_err());
}

proptest::proptest! {
    #[test]
    fn measured_affine_roundtrips_pixel_edges(
        x in 0_u32..500, y in 0_u32..200, width in 1_u32..800, height in 1_u32..600,
    ) {
        let bounds = rect(f64::from(x),f64::from(y),f64::from(width),f64::from(height));
        let geometry = witness(&window(bounds),displays()).unwrap();
        let mapped = mapping(width*2,height*2,&geometry).unwrap();
        let affine = mapped.image_to_desktop;
        proptest::prop_assert!((affine.xx * f64::from(width*2) + affine.tx - (bounds.x+bounds.width)).abs()<0.000_001);
        proptest::prop_assert!((affine.yy * f64::from(height*2) + affine.ty - (bounds.y+bounds.height)).abs()<0.000_001);
    }

    #[test]
    fn png_header_parser_handles_arbitrary_bounded_bytes(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(),0..256)) {
        if let Ok((width,height)) = png_dimensions(&bytes) {
            proptest::prop_assert!(width>0 && height>0);
        }
    }
}
