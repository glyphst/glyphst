use super::*;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const SIZE: u32 = 8;

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).ok()?;
    pollster::block_on(adapter.request_device(&Default::default())).ok()
}

macro_rules! require_device {
    () => {
        match device() {
            Some(device) => device,
            None => {
                eprintln!("no GPU adapter; skipping");
                return;
            }
        }
    };
}

fn solid_texture(device: &wgpu::Device, queue: &wgpu::Queue, rgba: [u8; 4]) -> wgpu::Texture {
    let pixels: Vec<u8> = rgba.repeat((SIZE * SIZE) as usize);
    texture_from(device, queue, &pixels, [SIZE, SIZE])
}

/// A texture of RGBA8 `pixels`, `size` wide and high.
fn texture_from(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pixels: &[u8],
    size: [u32; 2],
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size[0] * 4),
            rows_per_image: Some(size[1]),
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Draws `id` over a transparent target and returns its pixels.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    registry: &EffectRegistry,
    id: &str,
    from: &wgpu::Texture,
    to: &wgpu::Texture,
    progress: f32,
) -> Vec<u8> {
    render_at(device, queue, registry, id, from, to, progress, 1.0)
}

/// [`render`] drawn `opacity` opaque.
#[allow(clippy::too_many_arguments)]
fn render_at(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    registry: &EffectRegistry,
    id: &str,
    from: &wgpu::Texture,
    to: &wgpu::Texture,
    progress: f32,
    opacity: f32,
) -> Vec<u8> {
    let target = solid_texture(device, queue, [0, 0, 0, 0]);
    let target_view = target.create_view(&Default::default());
    let (from, to) = (
        from.create_view(&Default::default()),
        to.create_view(&Default::default()),
    );
    let def = registry.find(id).unwrap();
    let prepared = registry
        .prepare(
            device,
            id,
            &EffectInputs {
                from: &from,
                to: &to,
                resolution: [SIZE as f32; 2],
                pointer: None,
                time: 0.0,
                progress,
                direction: 1.0,
                params: def.default_params(),
                opacity,
            },
        )
        .unwrap();
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        registry.draw(&prepared, &mut pass);
    }
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(256 * SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(SIZE),
            },
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let mapped = buffer.get_mapped_range(..).unwrap();
    mapped
        .chunks_exact(256)
        .flat_map(|row| row[..(SIZE * 4) as usize].to_vec())
        .collect()
}

/// A transparent window's background: the solid colour, premultiplied
/// by the window's opacity.
#[test]
fn a_background_drawn_half_opaque_is_premultiplied() {
    let (device, queue) = require_device!();
    let registry = EffectRegistry::new(&device, FORMAT);
    let blank = solid_texture(&device, &queue, [0, 0, 0, 0]);
    let opaque = render_at(&device, &queue, &registry, SOLID, &blank, &blank, 0.0, 1.0);
    let half = render_at(&device, &queue, &registry, SOLID, &blank, &blank, 0.0, 0.5);
    assert_eq!(opaque[3], 255);
    assert!((126..=129).contains(&half[3]), "alpha {}", half[3]);
    for channel in 0..3 {
        let expected = f32::from(opaque[channel]) / 2.0;
        assert!((f32::from(half[channel]) - expected).abs() <= 1.5);
    }
}

#[test]
fn manifests_describe_effects_and_reject_bad_ones() {
    let def = parse_manifest(
        "x.wgsl",
        "// @effect id: wave\n// @effect name: Wave\n// @effect kind: background\n\
         // @effect animated: true\n// @effect param: speed float 0 3 0.5 Wave speed\n\
         // @effect param: on toggle 1\nfn effect() {}",
    )
    .unwrap();
    assert_eq!(def.kind, EffectKind::Background);
    assert!(def.animated);
    assert_eq!(def.params[0].label, "Wave speed");
    assert_eq!(def.params[1].kind, ParamKind::Toggle);
    assert_eq!(def.default_params(), [0.5, 1.0, 0.0, 0.0]);

    // Colours are sRGB packed in a float, exact and kept in range.
    let tint = parse_manifest(
        "t.wgsl",
        "// @effect id: t\n// @effect name: T\n// @effect kind: overlay\n\
         // @effect themed: true\n// @effect param: paper colour #F1E3C6 Paper",
    )
    .unwrap();
    assert!(tint.themed);
    assert_eq!(tint.params[0].kind, ParamKind::Colour);
    assert_eq!(unpack_colour(tint.default_params()[0]), [0xF1, 0xE3, 0xC6]);
    assert_eq!(unpack_colour(pack_colour([1, 2, 255])), [1, 2, 255]);
    assert_eq!(
        tint.sanitize_params([1e9, 0.0, 0.0, 0.0])[0],
        pack_colour([255; 3])
    );
    assert!(
        parse_manifest(
            "t",
            "// @effect id: t\n// @effect name: T\n// @effect kind: overlay\n\
             // @effect param: paper colour white Paper"
        )
        .is_err(),
        "a colour needs #rrggbb"
    );

    assert!(
        parse_manifest("x", "// @effect name: N\n// @effect kind: overlay").is_err(),
        "no id"
    );
    assert!(
        parse_manifest(
            "x",
            "// @effect id: a\n// @effect name: A\n// @effect kind: sparkle"
        )
        .is_err()
    );
    assert!(
        parse_manifest(
            "x",
            "// @effect id: a\n// @effect name: A\n// @effect kind: overlay\n// @effect param: p float 0 1 2"
        )
        .is_err(),
        "default outside its range"
    );
}

#[test]
fn builtin_effects_compile() {
    let (device, _queue) = require_device!();
    let registry = EffectRegistry::new(&device, FORMAT);
    assert!(registry.errors().is_empty(), "{:?}", registry.errors());
    for id in [PRESENT, SLIDE_OVER, CROSSFADE, SOLID, AURORA] {
        assert!(registry.find(id).is_some(), "{id} missing");
    }
}

#[test]
fn transitions_start_at_the_outgoing_view_and_end_at_the_incoming_one() {
    let (device, queue) = require_device!();
    let registry = EffectRegistry::new(&device, FORMAT);
    let from = solid_texture(&device, &queue, [200, 30, 30, 255]);
    let to = solid_texture(&device, &queue, [30, 30, 200, 255]);
    let transitions: Vec<String> = registry
        .defs()
        .filter(|def| def.kind == EffectKind::Transition)
        .map(|def| def.id.clone())
        .collect();
    assert!(transitions.len() >= 2);
    for id in transitions {
        let start = render(&device, &queue, &registry, &id, &from, &to, 0.0);
        let end = render(&device, &queue, &registry, &id, &from, &to, 1.0);
        assert!(
            start.chunks_exact(4).all(|p| p == [200, 30, 30, 255]),
            "{id} at 0: {:?}",
            &start[..4]
        );
        assert!(
            end.chunks_exact(4).all(|p| p == [30, 30, 200, 255]),
            "{id} at 1: {:?}",
            &end[..4]
        );
    }
}

#[test]
fn a_broken_user_effect_is_reported_not_fatal() {
    let (device, _queue) = require_device!();
    let mut registry = EffectRegistry::new(&device, FORMAT);
    let broken = "// @effect id: broken\n// @effect name: Broken\n// @effect kind: overlay\n\
                  fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> { return nope; }";
    assert!(!registry.add(&device, "broken.wgsl", broken));
    assert!(registry.find("broken").is_none());
    assert_eq!(registry.errors().len(), 1);
    assert_eq!(registry.errors()[0].source, "broken.wgsl");
    assert!(registry.find(PRESENT).is_some(), "built-ins stay usable");
}

#[test]
fn user_effects_load_from_a_directory_and_can_replace_builtins() {
    let (device, _queue) = require_device!();
    let dir = std::env::temp_dir().join(format!("glyphst-effects-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("tint.wgsl"),
        "// @effect id: solid\n// @effect name: Tinted solid\n// @effect kind: background\n\
         fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> { return vec4<f32>(0.1, 0.2, 0.3, 1.0); }",
    )
    .unwrap();
    std::fs::write(dir.join("notes.txt"), "not an effect").unwrap();
    let mut registry = EffectRegistry::new(&device, FORMAT);
    registry.reload_user(&device, &dir);
    assert!(registry.errors().is_empty(), "{:?}", registry.errors());
    assert_eq!(registry.find(SOLID).unwrap().name, "Tinted solid");
    assert!(registry.find(SOLID).unwrap().user);
    let solids = registry.defs().filter(|def| def.id == SOLID).count();
    assert_eq!(solids, 1, "the user effect takes the built-in's place");

    // Taking the file away brings the built-in back.
    std::fs::remove_file(dir.join("tint.wgsl")).unwrap();
    registry.reload_user(&device, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(registry.find(SOLID).unwrap().name, "Solid colour");
    assert!(!registry.find(SOLID).unwrap().user);
}

#[test]
fn reloading_keeps_good_effects_reports_broken_ones_and_notices_edits() {
    let (device, _queue) = require_device!();
    let dir = std::env::temp_dir().join(format!("glyphst-effects-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let good = "// @effect id: wave\n// @effect name: Wave\n// @effect kind: background\n\
                fn effect(uv: vec2<f32>, pixel: vec2<f32>) -> vec4<f32> { return vec4<f32>(uv, 0.0, 1.0); }";
    std::fs::write(dir.join("a-wave.wgsl"), good).unwrap();
    std::fs::write(
        dir.join("b-broken.wgsl"),
        "// @effect id: b\nfn effect() {}",
    )
    .unwrap();
    let mut registry = EffectRegistry::new(&device, FORMAT);
    let before = user_stamp(&dir);
    registry.reload_user(&device, &dir);
    assert!(registry.find("wave").is_some());
    assert_eq!(registry.errors().len(), 1);
    assert!(registry.errors()[0].source.ends_with("b-broken.wgsl"));

    // An edit changes the stamp; a reload replaces the layer and its errors.
    std::fs::write(dir.join("b-broken.wgsl"), good.replace("wave", "fixed")).unwrap();
    let after = user_stamp(&dir);
    assert_ne!(before, after);
    registry.reload_user(&device, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(registry.errors().is_empty(), "{:?}", registry.errors());
    assert!(registry.find("fixed").is_some() && registry.find("wave").is_some());
    let ids: Vec<&str> = registry.defs().map(|def| def.id.as_str()).collect();
    assert_eq!(
        &ids[ids.len() - 2..],
        ["wave", "fixed"],
        "user effects after the built-ins"
    );
}

#[test]
fn the_template_and_every_builtin_compile() {
    let (device, _queue) = require_device!();
    let mut registry = EffectRegistry::new(&device, FORMAT);
    assert!(registry.errors().is_empty(), "{:?}", registry.errors());
    for id in [IMAGE, SEPIA, NIGHT, PAPER, TINT, THEME_TINT] {
        assert!(registry.find(id).is_some(), "{id} missing");
    }
    assert!(registry.find(IMAGE).unwrap().image);
    assert!(registry.find(PRESENT).unwrap().hidden);
    assert!(
        registry.add(&device, "template.wgsl", TEMPLATE),
        "{:?}",
        registry.errors()
    );
}

#[test]
fn parameters_are_kept_within_their_ranges() {
    let def = parse_manifest(
        "x",
        "// @effect id: a\n// @effect name: A\n// @effect kind: overlay\n\
         // @effect param: s float 0 1 0.5 S\n// @effect param: t toggle 0",
    )
    .unwrap();
    assert_eq!(
        def.sanitize_params([2.0, 0.7, 9.0, 9.0]),
        [1.0, 1.0, 0.0, 0.0]
    );
    assert_eq!(
        def.sanitize_params([f32::NAN, 0.2, 0.0, 0.0]),
        [0.5, 0.0, 0.0, 0.0]
    );
}

/// Draws `id` over a `size` target from one input and returns its pixels.
fn render_from(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    registry: &EffectRegistry,
    id: &str,
    input: &wgpu::Texture,
) -> Vec<u8> {
    render(device, queue, registry, id, input, input, 0.0)
}

#[test]
fn wallpapers_fill_the_background_cropped_to_cover() {
    let (device, queue) = require_device!();
    let registry = EffectRegistry::new(&device, FORMAT);
    // A wallpaper twice as wide as high: left half red, right half blue,
    // with a green column at each edge that the crop cuts away.
    let (width, height) = (32u32, 16u32);
    let pixels: Vec<u8> = (0..height)
        .flat_map(|_| {
            (0..width).flat_map(|x| match x {
                0..=3 | 28..=31 => [0, 255, 0, 255],
                x if x < width / 2 => [255, 0, 0, 255],
                _ => [0, 0, 255, 255],
            })
        })
        .collect();
    let image = texture_from(&device, &queue, &pixels, [width, height]);
    let out = render_from(&device, &queue, &registry, IMAGE, &image);
    let pixel = |x: u32, y: u32| {
        let i = ((y * SIZE + x) * 4) as usize;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    };
    assert_eq!(pixel(0, 0), [255, 0, 0, 255], "the green edge is cropped");
    assert_eq!(pixel(SIZE - 1, SIZE - 1), [0, 0, 255, 255]);

    // Without a wallpaper (one transparent pixel) the background is black.
    let none = solid_texture(&device, &queue, [0, 0, 0, 0]);
    let out = render_from(&device, &queue, &registry, IMAGE, &none);
    assert!(out.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
}

#[test]
fn night_darkens_white_pages_and_keeps_their_edges() {
    let (device, queue) = require_device!();
    let registry = EffectRegistry::new(&device, FORMAT);
    let white = solid_texture(&device, &queue, [255, 255, 255, 255]);
    let out = render_from(&device, &queue, &registry, NIGHT, &white);
    assert!(
        out.chunks_exact(4).all(|p| p[0] < 40 && p[3] == 255),
        "{:?}",
        &out[..4]
    );
    let black = solid_texture(&device, &queue, [0, 0, 0, 255]);
    let out = render_from(&device, &queue, &registry, NIGHT, &black);
    assert!(out.chunks_exact(4).all(|p| p[0] > 180), "{:?}", &out[..4]);
    let around = solid_texture(&device, &queue, [0, 0, 0, 0]);
    for id in [NIGHT, SEPIA, PAPER] {
        let out = render_from(&device, &queue, &registry, id, &around);
        assert!(
            out.chunks_exact(4).all(|p| p == [0, 0, 0, 0]),
            "{id} outside pages"
        );
    }
}
