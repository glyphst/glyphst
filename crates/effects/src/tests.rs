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
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
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
    let pixels: Vec<u8> = rgba.repeat((SIZE * SIZE) as usize);
    queue.write_texture(
        texture.as_image_copy(),
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(SIZE * 4),
            rows_per_image: Some(SIZE),
        },
        wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
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
    registry.load_dir(&device, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(registry.errors().is_empty(), "{:?}", registry.errors());
    assert_eq!(registry.find(SOLID).unwrap().name, "Tinted solid");
}
