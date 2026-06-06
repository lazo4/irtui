use std::{f32::consts::PI, io::Cursor, sync::Arc, time::Instant};

use anyhow::anyhow;
use base64::{Engine as _, engine::general_purpose};
use glam::{EulerRot, Mat4};
use image::{GenericImage, ImageBuffer, ImageReader, Rgba, RgbaImage, imageops};
use ratatui::layout::Rect;
use ratatui_image::{Resize, picker::Picker};
use serde_json::Value;
use tokio::task;
use tracing::{Level, debug, error, info, instrument, warn};
use wgpu::{
    BindGroupDescriptor, BindGroupEntry, BufferUsages, ColorTargetState, ColorWrites,
    CommandEncoderDescriptor, Device, Extent3d, FragmentState, Instance, InstanceDescriptor,
    MultisampleState, Operations, Origin3d, PollType, PrimitiveState, Queue,
    RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline, RenderPipelineDescriptor,
    ShaderModule, ShaderModuleDescriptor, ShaderSource, TexelCopyBufferInfo, TexelCopyBufferLayout,
    TexelCopyTextureInfo, Texture, TextureAspect, TextureDimension, TextureFormat, TextureUsages,
    TextureView, TextureViewDescriptor, VertexState,
    hal::auxil::db::qualcomm,
    wgc::command::CommandEncoderError::Query,
    wgt::{BufferDescriptor, TextureDescriptor},
};
use wreq::Client;

use crate::{
    app::PanoRequest,
    event::{AppEvent, Event},
    pano::api::{PanoMetadata, get_pano_metadata_from_id, load_equirect},
};

mod api;
mod render;

/// Render a pano from it's metadata, fetching the tiles and rendering them into the output buffer
///
/// # Errors
/// This can fail if we fail to fetch any of the tiles
/// TODO: Render a blank space instead of failing
#[instrument(skip(meta), level = Level::DEBUG)]
pub async fn render_pano_from_metadata(
    meta: &PanoMetadata,
    heading: f32,
    out_w: u32,
    out_h: u32,
    GPUState {
        device,
        queue,
        out_texture,
        in_texture,
        shader,
        pipeline,
    }: &GPUState,
) -> anyhow::Result<RgbaImage> {
    let before_render = Instant::now();

    // Note: this first part is slow, be sure to only do this once in prod

    // Create the Offscreen Render Target Texture

    let tex_view = out_texture.create_view(&Default::default());

    let unpadded_bytes_per_row = out_w * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT; // 256
    let padding = (align - unpadded_bytes_per_row % align) % align;
    let padded_bytes_per_row = unpadded_bytes_per_row + padding;

    // Create a staging buffer to copy texture data into
    let staging_buffer = device.create_buffer(&BufferDescriptor {
        label: Some("Staging buffer"),
        size: (padded_bytes_per_row * out_h) as u64,
        usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    // Set up the transform matrix, this is part AI, i suck at this
    let fov = 90.0f32.to_radians();
    let aspect = out_w as f32 / out_h as f32;
    let projection_inv = Mat4::perspective_rh(fov, aspect, 0.1, 10.0).inverse();

    let yaw = (270. - meta.heading as f32 + heading)
        .rem_euclid(360.)
        .to_radians();
    let pitch = 0.0;
    let roll = meta.roll.to_radians() as f32;

    // V_inv is just the pure rotation matrix of the camera in the world.
    // YXZ order applies Heading (Y), then Pitch (X), then Roll (Z).
    let view_inv = Mat4::from_euler(EulerRot::YXZ, yaw, pitch, roll);

    // 3. Combine them directly
    // Order matters: We want to un-project first, then un-rotate.
    let inv_view_proj = view_inv * projection_inv;

    // Create uniform buffer with raw 64-byte matrix capacity
    let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Camera Matrix Buffer"),
        size: std::mem::size_of::<[[f32; 4]; 4]>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(
        &camera_buffer,
        0,
        bytemuck::cast_slice(&inv_view_proj.to_cols_array_2d()),
    );

    // Create and send the input texture

    let in_tex_view = &in_texture.create_view(&Default::default());

    let sampler = device.create_sampler(&Default::default());

    // Create a rendering pipeline

    let camera_bind = device.create_bind_group(&BindGroupDescriptor {
        label: Some("Camera Bind Group"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[BindGroupEntry {
            binding: 0,
            resource: camera_buffer.as_entire_binding(),
        }],
    });

    let texture_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("Texture Bind Group"),
        layout: &pipeline.get_bind_group_layout(1), // Automatically extracted @group(1)
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(in_tex_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });

    let render_pass_desc = RenderPassDescriptor {
        label: Some("Render Pass"),
        color_attachments: &[Some(RenderPassColorAttachment {
            view: &tex_view,
            depth_slice: None,
            resolve_target: None,
            ops: Operations::default(),
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    };

    let mut encoder = device.create_command_encoder(&CommandEncoderDescriptor {
        label: Some("Command Encoder"),
    });

    // Create a render pass (frame, afaik)
    {
        let mut pass = encoder.begin_render_pass(&render_pass_desc);
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &camera_bind, &[]);
        pass.set_bind_group(1, &texture_bind, &[]);
        pass.draw(0..3, 0..1);
    }

    // 10. Copy Texture to Aligned CPU Buffer
    encoder.copy_texture_to_buffer(
        TexelCopyTextureInfo {
            texture: tex_view.texture(),
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        TexelCopyBufferInfo {
            buffer: &staging_buffer,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: None,
            },
        },
        Extent3d {
            width: out_w,
            height: out_h,
            depth_or_array_layers: 1,
        },
    );

    queue.submit(std::iter::once(encoder.finish()));

    let buffer_slice = staging_buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        tx.send(result).unwrap();
    });

    device.poll(PollType::Wait {
        submission_index: None,
        timeout: None,
    })?;
    rx.recv().unwrap().unwrap();

    let data = buffer_slice.get_mapped_range();
    let mut pixel_data = Vec::with_capacity((out_w * out_h * 4) as usize);
    for chunk in data.chunks_exact(padded_bytes_per_row as usize) {
        pixel_data.extend_from_slice(&chunk[..unpadded_bytes_per_row as usize]);
    }
    drop(data);
    staging_buffer.unmap();

    let total_ms = before_render.elapsed().as_secs_f64() * 1000.0;
    info!(total_ms, out_w, out_h, heading, "GPU rendered panorama");

    RgbaImage::from_raw(out_w, out_h, pixel_data)
        .ok_or_else(|| anyhow!("Failed to create image from pixel data"))
}

pub struct GPUState {
    pub device: Device,
    pub queue: Queue,
    pub out_texture: Texture,
    pub in_texture: Texture,
    pub shader: ShaderModule,
    pub pipeline: RenderPipeline,
}

/// Spawn the task responsible for fetching and rendering gsv panos.
///
/// # Errors
/// This fails if we fail to spawn the task e.g. we fail to query the terminal size
#[instrument(skip_all, level = Level::DEBUG)]
pub fn spawn_rendering_task(
    mut pano_rx: tokio::sync::mpsc::Receiver<PanoRequest>,
    evt_sender: tokio::sync::mpsc::UnboundedSender<Event>,
) -> anyhow::Result<()> {
    let picker = Picker::halfblocks(); // TODO: Support real image protocols but for now support for text on top of images is too flaky
    let mut cur_size = crossterm::terminal::size()?;
    info!(
        width = cur_size.0,
        height = cur_size.1,
        "Panorama renderer initialized"
    );

    tokio::task::spawn(async move {
        let font_size = picker.font_size();

        // Request an instance, the entry point to wgpu
        let instance = Instance::new(InstanceDescriptor::new_without_display_handle());

        // Request an adapter
        let adapter = instance.request_adapter(&Default::default()).await.unwrap(); // Default options

        // Request access to the actual GPU
        let (device, queue) = adapter.request_device(&Default::default()).await.unwrap();

        let out_texture = device.create_texture(&TextureDescriptor {
            label: Some("Out texture"),
            size: Extent3d {
                width: (font_size.0 * cur_size.0) as u32,
                height: (font_size.1 * cur_size.1) as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        let in_texture = device.create_texture(&TextureDescriptor {
            label: Some("In texture"),
            size: Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        }); // This is inefficient, but our struct must be fully initialized

        let shader = device.create_shader_module(ShaderModuleDescriptor {
            label: Some("Equirectangular shader"),
            source: ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
            label: Some("Pano Render Pipeline"),
            layout: None,
            vertex: VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(ColorTargetState {
                    format: TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: ColorWrites::all(),
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let mut gpu_state = GPUState {
            device,
            queue,
            out_texture,
            in_texture,
            shader,
            pipeline,
        };

        let mut meta_cache: Option<PanoMetadata> = None;

        let mut cur_heading = 0.0;

        let mut cur_panoid = String::new();

        while let Some(request) = pano_rx.recv().await {
            let mut new_pano = false;
            let mut needs_resize = false;

            match request {
                PanoRequest::Resize(width, height) => {
                    if cur_size != (width, height) {
                        needs_resize = true;
                        cur_size = (width, height);
                        info!(width, height, "Terminal resized");
                    }
                }
                PanoRequest::Render(panoid, hdg) => {
                    debug!(panoid = %panoid, heading = hdg, "Render request");
                    cur_panoid = panoid;
                    cur_heading = hdg;
                    new_pano = true;
                }
            }

            // Drain the queue, to avoid lagging out
            while let Ok(req) = pano_rx.try_recv() {
                match req {
                    PanoRequest::Resize(width, height) => {
                        if cur_size != (width, height) {
                            needs_resize = true;
                            cur_size = (width, height);
                        }
                    }
                    PanoRequest::Render(panoid, hdg) => {
                        cur_panoid = panoid;
                        cur_heading = hdg;
                        new_pano = true;
                    }
                }
            }
            // If a new pano has arrived
            if new_pano {
                // Fetch it
                match get_pano_metadata_from_id(&cur_panoid).await {
                    Ok(meta) => {
                        match load_equirect(&meta).await {
                            Ok(pano) => {
                                // Only resize if the pano size has changed
                                if meta_cache
                                    .as_ref()
                                    .map(|meta| (meta.image_width, meta.image_height))
                                    != Some((meta.image_width, meta.image_height))
                                {
                                    gpu_state.in_texture.destroy();
                                    gpu_state.in_texture =
                                        gpu_state.device.create_texture(&TextureDescriptor {
                                            label: Some("In texture"),
                                            size: Extent3d {
                                                width: pano.width(),
                                                height: pano.height(),
                                                depth_or_array_layers: 1,
                                            },
                                            mip_level_count: 1,
                                            sample_count: 1,
                                            dimension: TextureDimension::D2,
                                            format: TextureFormat::Rgba8Unorm,
                                            usage: TextureUsages::TEXTURE_BINDING
                                                | TextureUsages::COPY_DST,
                                            view_formats: &[],
                                        });
                                }

                                meta_cache = Some(meta);

                                // Now store the pano in the texture

                                gpu_state.queue.write_texture(
                                    TexelCopyTextureInfo {
                                        texture: &gpu_state.in_texture,
                                        mip_level: 0,
                                        origin: Origin3d::ZERO,
                                        aspect: TextureAspect::All,
                                    },
                                    &pano,
                                    TexelCopyBufferLayout {
                                        offset: 0,
                                        bytes_per_row: Some(pano.width() * 4),
                                        rows_per_image: None,
                                    },
                                    gpu_state.in_texture.size(),
                                );
                            }
                            Err(err) => {
                                error!(error = ?err, panoid = %cur_panoid, "Failed to load equirectangular image");
                            }
                        }
                    }
                    Err(err) => {
                        error!(error = ?err, panoid = %cur_panoid, "Failed to fetch pano metadata");
                    }
                }
            }

            // Invalidate the output texture if the screen has been resized
            if needs_resize {
                gpu_state.out_texture.destroy();
                gpu_state.out_texture = gpu_state.device.create_texture(&TextureDescriptor {
                    label: Some("Out texture"),
                    size: Extent3d {
                        width: (font_size.0 * cur_size.0) as u32,
                        height: (font_size.1 * cur_size.1) as u32,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: TextureDimension::D2,
                    format: TextureFormat::Rgba8Unorm,
                    usage: TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
            }

            // In both cases rerender the screen
            if let Some(meta) = &meta_cache
                && (needs_resize || new_pano)
            {
                let width = cur_size.0 * font_size.0;
                let height = cur_size.1 * font_size.1;

                debug!("Rendering pano");

                match render_pano_from_metadata(
                    meta,
                    cur_heading as f32,
                    width as u32,
                    height as u32,
                    &gpu_state,
                )
                .await
                {
                    Ok(screen) => {
                        match picker.new_protocol(
                            screen.into(),
                            Rect::new(0, 0, width, height),
                            Resize::Crop(None),
                        ) {
                            Ok(protocol) => {
                                let _ = evt_sender.send(Event::App(AppEvent::NewFrame(protocol)));
                            }
                            Err(err) => {
                                error!(error = ?err, "Failed to create protocol from rendered image");
                            }
                        }
                    }
                    Err(err) => {
                        error!(error = ?err, "Failed to render panorama");
                    }
                }
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::pano::api::{Pano, PanoType, ZoomLevel};

    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};
    use tokio::sync::mpsc::{channel, unbounded_channel};

    #[tokio::test]
    #[ignore = "uses the network"]
    async fn test_pano_fetch() {
        let meta = get_pano_metadata_from_id("tXVQoL_JtBEBbV7LYKW_2A")
            .await
            .unwrap();
        let _ = load_equirect(&meta).await;

        let meta = get_pano_metadata_from_id("CAoSFkNJSE0wb2dLRUlDQWdJQ0U5SVBWR1E.")
            .await
            .unwrap();
        let _ = load_equirect(&meta).await;
    }

    #[tokio::test]
    async fn test_render_pano_from_metadata_basic() {
        use image::RgbaImage;

        // fake 2x2 pano image
        let mut pano = RgbaImage::new(2, 2);
        pano.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        pano.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
        pano.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        pano.put_pixel(1, 1, image::Rgba([255, 255, 0, 255]));

        // fake metadata
        let meta = super::PanoMetadata {
            pano: Pano {
                pano_type: PanoType::Official,
                id: "fake".to_string(),
            },
            lat: 0.0,
            lng: 0.0,
            image_width: 2,
            image_height: 2,
            tile_width: 2,
            tile_height: 2,
            max_zoom: 0,
            zoom_levels: vec![ZoomLevel {
                crop_width: 2,
                crop_height: 2,
                num_tiles_x: 1,
                num_tiles_y: 1,
            }],
            heading: 0.0,
            tilt: 0.0,
            roll: 0.0,
        };

        // call render
        // let rendered = super::render_pano_from_metadata(&meta, &pano, 0.0, 4, 4)
        //     .await
        //     .unwrap();

        // assert_eq!(rendered.width(), 4);
        // assert_eq!(rendered.height(), 4);
    }

    #[tokio::test]
    #[ignore = "Uses the network"]
    async fn test_rendering_task() {
        let (pano_tx, pano_rx) = channel(10); // Who cares?
        let (evt_tx, mut evt_rx) = unbounded_channel();

        spawn_rendering_task(pano_rx, evt_tx).unwrap();

        pano_tx
            .send(PanoRequest::Render(
                "tXVQoL_JtBEBbV7LYKW_2A".to_string(),
                0.0,
            ))
            .await
            .unwrap();

        let timeout = std::time::Duration::from_secs(30);

        tokio::time::timeout(timeout, evt_rx.recv())
            .await
            .unwrap()
            .unwrap();
    }
}
