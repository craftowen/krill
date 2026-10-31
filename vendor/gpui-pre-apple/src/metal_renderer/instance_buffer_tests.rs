use super::*;

fn frame_buffer(
    pool: &Arc<Mutex<InstanceBufferPool>>,
    device: &metal::Device,
    bytes: usize,
) -> InstanceBuffer {
    let mut writer = InstanceBufferWriter::new(device, pool, true);
    writer.allocate::<u8>(bytes).unwrap();
    writer.finish()
}

#[test]
fn instance_buffer_pool_retires_a_burst_after_sustained_small_frames() {
    objc2::rc::autoreleasepool(|_| {
        let device = MetalRenderer::create_device();
        let pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let now = Instant::now();
        let large = frame_buffer(&pool, &device, 8 * DEFAULT_INSTANCE_BUFFER_SIZE);
        let large_size = large.size;
        pool.lock().release_at(large, now);
        for elapsed in [0, 1, 4] {
            let small = frame_buffer(&pool, &device, 128);
            pool.lock()
                .release_at(small, now + Duration::from_secs(elapsed));
            assert_eq!(pool.lock().buffer_size, large_size);
        }
        let old_in_flight = frame_buffer(&pool, &device, 128);
        let small = frame_buffer(&pool, &device, 128);
        pool.lock()
            .release_at(small, now + INSTANCE_BUFFER_SHRINK_DELAY);
        assert_eq!(pool.lock().buffer_size, DEFAULT_INSTANCE_BUFFER_SIZE);
        pool.lock().release_at(
            old_in_flight,
            now + INSTANCE_BUFFER_SHRINK_DELAY + Duration::from_secs(1),
        );
        assert!(pool.lock().buffers.is_empty());
        assert_eq!(
            frame_buffer(&pool, &device, 128).size,
            DEFAULT_INSTANCE_BUFFER_SIZE
        );
    });
}

#[test]
fn instance_buffer_pool_reuses_capacity_through_repeated_large_frames() {
    objc2::rc::autoreleasepool(|_| {
        let device = MetalRenderer::create_device();
        let pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let now = Instant::now();
        let large = frame_buffer(&pool, &device, 8 * DEFAULT_INSTANCE_BUFFER_SIZE);
        let resource = large.metal_buffer.as_ptr();
        pool.lock().release_at(large, now);
        for cycle in 0..10 {
            for (seconds, bytes) in [(1, 128), (4, 8 * DEFAULT_INSTANCE_BUFFER_SIZE)] {
                let buffer = frame_buffer(&pool, &device, bytes);
                assert_eq!(buffer.metal_buffer.as_ptr(), resource);
                pool.lock()
                    .release_at(buffer, now + Duration::from_secs(cycle * 4 + seconds));
            }
        }
    });
}

#[test]
fn instance_buffer_pool_counts_every_chunk_of_a_large_frame() {
    objc2::rc::autoreleasepool(|_| {
        let device = MetalRenderer::create_device();
        let pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let now = Instant::now();
        let mut writer = InstanceBufferWriter::new(&device, &pool, true);
        writer
            .allocate::<u8>(DEFAULT_INSTANCE_BUFFER_SIZE - 256)
            .unwrap();
        writer.allocate::<u8>(512).unwrap();
        let large = writer.finish();
        assert!(large.size > DEFAULT_INSTANCE_BUFFER_SIZE);
        let large_size = large.size;
        pool.lock().release_at(large, now);
        // A small final chunk must not start the small-frame timer for this large frame.
        let small = frame_buffer(&pool, &device, 128);
        pool.lock()
            .release_at(small, now + INSTANCE_BUFFER_SHRINK_DELAY);
        assert_eq!(pool.lock().buffer_size, large_size);
    });
}

#[test]
fn instance_buffer_pool_shrinking_during_encoding_still_allows_growth() {
    objc2::rc::autoreleasepool(|_| {
        let device = MetalRenderer::create_device();
        let pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let now = Instant::now();
        let large = frame_buffer(&pool, &device, 4 * DEFAULT_INSTANCE_BUFFER_SIZE);
        pool.lock().release_at(large, now);
        let mut writer = InstanceBufferWriter::new(&device, &pool, true);
        let first_size = writer.current.size;
        let (first, _) = writer.allocate::<u8>(first_size - 256).unwrap();
        for elapsed in [Duration::ZERO, INSTANCE_BUFFER_SHRINK_DELAY] {
            let small = frame_buffer(&pool, &device, 128);
            pool.lock().release_at(small, now + elapsed);
        }
        assert_eq!(pool.lock().buffer_size, DEFAULT_INSTANCE_BUFFER_SIZE);
        let (next, _) = writer.allocate::<u8>(512).unwrap();
        assert_ne!(first.buffer.as_ptr(), next.buffer.as_ptr());
        let completed = writer.finish();
        assert!(completed.size > first_size);
        assert!(completed.frame_bytes > first_size);
    });
}

#[test]
fn instance_buffer_pool_shrinking_preserves_rendered_pixels() {
    objc2::rc::autoreleasepool(|_| {
        let pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let mut renderer = MetalRenderer::new_headless(pool.clone());
        let bounds = Bounds {
            origin: Point::default(),
            size: size(ScaledPixels(16.0), ScaledPixels(16.0)),
        };
        let mut scene = Scene::default();
        scene.insert_primitive(gpui::Quad {
            order: 0,
            bounds,
            content_mask: ContentMask { bounds },
            background: gpui::rgb(0x3377aa).into(),
            border_color: gpui::transparent_black(),
            corner_radii: Default::default(),
            border_widths: Default::default(),
            border_style: Default::default(),
        });
        scene.finish();
        let viewport = size(DevicePixels(16), DevicePixels(16));
        let before = renderer.render_scene_to_image(&scene, viewport).unwrap();
        assert_eq!(
            *before.get_pixel(8, 8),
            image::Rgba([0x33, 0x77, 0xaa, 255])
        );
        let now = Instant::now();
        let large = frame_buffer(&pool, &renderer.device, 8 * DEFAULT_INSTANCE_BUFFER_SIZE);
        pool.lock().release_at(large, now);
        for elapsed in [Duration::ZERO, INSTANCE_BUFFER_SHRINK_DELAY] {
            let small = frame_buffer(&pool, &renderer.device, 128);
            pool.lock().release_at(small, now + elapsed);
        }
        assert_eq!(pool.lock().buffer_size, DEFAULT_INSTANCE_BUFFER_SIZE);
        let after = renderer.render_scene_to_image(&scene, viewport).unwrap();
        assert_eq!(before, after);
    });
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "real Metal buffer retention and phys_footprint; no GUI required"]
fn instance_buffer_pool_returns_burst_memory_to_the_os() {
    fn footprint() -> u64 {
        let output = std::process::Command::new("/usr/bin/footprint")
            .args(["-f", "bytes", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .split_once("Footprint: ")
            .unwrap()
            .1
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    let device = MetalRenderer::create_device();
    let pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
    let large = objc2::rc::autoreleasepool(|_| {
        let mut writer = InstanceBufferWriter::new(&device, &pool, true);
        let (_, bytes) = writer.allocate::<u8>(64 << 20).unwrap();
        for byte in bytes {
            byte.write(0x5a);
        }
        writer.finish()
    });
    // A test-only negative control must fail the numeric retention check.
    let _retained =
        std::env::var_os("KRILL_TEST_RETAIN_BURST_BUFFER").map(|_| large.metal_buffer.clone());
    pool.lock().release(large);
    let burst = footprint();
    let deadline = Instant::now() + INSTANCE_BUFFER_SHRINK_DELAY + Duration::from_secs(5);
    while pool.lock().buffer_size != DEFAULT_INSTANCE_BUFFER_SIZE {
        objc2::rc::autoreleasepool(|_| {
            let small = frame_buffer(&pool, &device, 128);
            pool.lock().release(small);
        });
        assert!(Instant::now() < deadline, "oversized pool did not shrink");
        std::thread::sleep(Duration::from_millis(100));
    }
    objc2::rc::autoreleasepool(|_| {
        let small = frame_buffer(&pool, &device, 128);
        pool.lock().release(small);
    });
    // Kernel accounting can lag behind the Metal resource's final release.
    std::thread::sleep(Duration::from_secs(2));
    let settled = footprint();
    eprintln!("Metal burst footprint: {burst} -> {settled} bytes");
    assert!(
        burst.saturating_sub(settled) >= 32 << 20,
        "oversized Metal cache did not return physical memory: {burst} -> {settled}"
    );
}
