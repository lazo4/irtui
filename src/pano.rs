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
    MultisampleState, Operations, Origin3d, PollType, PrimitiveState, RenderPassColorAttachment,
    RenderPassDescriptor, RenderPipelineDescriptor, ShaderModuleDescriptor, ShaderSource,
    TexelCopyBufferInfo, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
    TextureDimension, TextureFormat, TextureUsages, TextureView, TextureViewDescriptor,
    VertexState,
    wgt::{BufferDescriptor, TextureDescriptor},
};
use wreq::Client;

use crate::{
    app::PanoRequest,
    event::{AppEvent, Event},
};

// All of this is adapted from Mikarific/LookoutTheWindow

// Panorama type enum matching the JS PanoramaType
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PanoType {
    Official = 2,
    Unofficial = 10,
}

pub struct Tile {
    pano: Pano,
    x: u32,
    y: u32,
    zoom: u32,
}

/// Result produced by `decode_panoid`.
#[derive(Debug, Clone)]
pub struct Pano {
    pano_type: PanoType,
    id: String,
}
/// Minimal metadata needed for rendering a pano.
#[derive(Debug, Clone)]
pub struct ZoomLevel {
    crop_width: u32,
    crop_height: u32,
    num_tiles_x: u32,
    num_tiles_y: u32,
}

#[derive(Debug, Clone)]
#[allow(unused)]
pub struct PanoMetadata {
    pano: Pano,
    lat: f64,
    lng: f64,
    image_width: u32,
    image_height: u32,
    tile_width: u32,
    tile_height: u32,
    max_zoom: usize,
    zoom_levels: Vec<ZoomLevel>,
    heading: f64,
    tilt: f64,
    roll: f64,
}

/// Infer a panos type from its id
fn decode_panoid(panoid: &str) -> Pano {
    // Helper for the two error paths in the JS version. We try to parse as
    // protobuf; if anything goes wrong we land in the catch branch below.
    let try_protobuf = || -> Option<Pano> {
        // Convert "base64url" style to regular base64 and decode.
        let mut b64 = panoid.replace('-', "+").replace('_', "/");
        // dot is used for padding in the JS code
        b64 = b64.replace('.', "=");

        let Ok(bytes) = general_purpose::STANDARD.decode(&b64) else {
            return None;
        };

        let mut index = 0;
        if index >= bytes.len() || bytes[index] != 0x08 {
            return None;
        }
        index += 1;

        // decode a varint, return None on failure
        let decode_varint = |bytes: &[u8], idx: &mut usize| -> Option<u32> {
            let mut result: u32 = 0;
            let mut shift = 0;
            let mut count = 0;
            while *idx < bytes.len() && count < 5 {
                let byte = bytes[*idx];
                *idx += 1;
                result |= ((byte & 0x7f) as u32) << shift;
                if (byte & 0x80) == 0 {
                    return Some(result);
                }
                shift += 7;
                count += 1;
            }
            None
        };

        let ty = decode_varint(&bytes, &mut index)?;

        if index >= bytes.len() || bytes[index] != 0x12 {
            return None;
        }
        index += 1;

        // decode length-prefixed string
        let id = {
            let length = decode_varint(&bytes, &mut index)? as usize;
            if index + length > bytes.len() {
                return None;
            }
            let slice = &bytes[index..index + length];
            match std::str::from_utf8(slice) {
                Ok(s) => s.to_owned(),
                Err(_) => return None,
            }
        };

        // map numeric value to our enum; unknown values default to Official
        let pano_type = match ty {
            2 => PanoType::Official,
            10 => PanoType::Unofficial,
            _ => unreachable!(),
        };

        Some(Pano { pano_type, id })
    };

    if let Some(pano) = try_protobuf() {
        return pano;
    }

    // Fall back to guessing the type from the plain string.
    let mut pano_type = PanoType::Official;
    // Official panoids are 22 chars long and end with one of g w A Q.
    if !(panoid.len() == 22
        && matches!(panoid.chars().last().unwrap(), 'g' | 'w' | 'A' | 'Q')
        && panoid
            .chars()
            .take(21)
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    {
        pano_type = PanoType::Unofficial;
    }

    Pano {
        pano_type,
        id: panoid.to_string(),
    }
}

/// Fetch metadata for a panorama ID using the `MapsJs` internal service.
/// Returns `None` on any network/error condition.
///
/// Stolen from Mikarific/LookoutTheWindow
///
/// # Errors
///
/// This fails if the network fails, or if we fail to parse the response json for some reason
#[instrument(level = Level::DEBUG)]
pub async fn get_pano_metadata_from_id(pano_id: &str) -> anyhow::Result<PanoMetadata> {
    let pano = decode_panoid(pano_id);
    let type_num = pano.pano_type as u8;

    let payload = format!(
        "[[\"apiv3\"],[\"en\",\"US\"],[[[{type_num},\"{id}\"]]],[[1,4]]]",
        type_num = type_num,
        id = pano.id
    );

    let client = Client::new();
    let res = client
        .post("https://maps.googleapis.com/$rpc/google.internal.maps.mapsjs.v1.MapsJsInternalService/GetMetadata")
        .header("Content-Type", "application/json+protobuf")
        .body(payload)
        .send().await?;

    if !res.status().is_success() {
        anyhow::bail!("Request failed with status: {}", res.status());
    }
    let meta: Value = res.json().await?;

    // Helper to extract nested f64
    let get_f64 = |path: &[usize]| -> anyhow::Result<f64> {
        let mut val = &meta;
        for &idx in path {
            val = val
                .get(idx)
                .ok_or_else(|| anyhow!("missing path: {path:?}"))?;
        }
        val.as_f64()
            .ok_or_else(|| anyhow!("expected f64 at path: {path:?}"))
    };

    // Helper to extract nested u64
    let get_u64 = |value: &Value, path: &[usize]| -> anyhow::Result<u64> {
        let mut val = value;
        for &idx in path {
            val = val
                .get(idx)
                .ok_or_else(|| anyhow!("missing path: {path:?}"))?;
        }
        val.as_u64()
            .ok_or_else(|| anyhow!("expected u64 at path: {path:?}"))
    };

    // Helper to extract nested str
    let get_str = |path: &[usize]| -> anyhow::Result<&str> {
        let mut val = &meta;
        for &idx in path {
            val = val
                .get(idx)
                .ok_or_else(|| anyhow!("missing path: {path:?}"))?;
        }
        val.as_str()
            .ok_or_else(|| anyhow!("expected str at path: {path:?}"))
    };

    // Helper to extract nested vec
    let get_vec = |path: &[usize]| -> anyhow::Result<&Vec<Value>> {
        let mut val = &meta;
        for &idx in path {
            val = val
                .get(idx)
                .ok_or_else(|| anyhow!("missing path: {path:?}"))?;
        }
        val.as_array()
            .ok_or_else(|| anyhow!("expected array at path: {path:?}"))
    };

    // Extract pano info
    let p_type = get_u64(&meta, &[1, 0, 1, 0])? as u8;
    let p_id = get_str(&[1, 0, 1, 1])?.to_owned();
    assert_eq!(p_type, pano.pano_type as u8);
    assert_eq!(p_id, pano.id);

    // Location
    let lat = get_f64(&[1, 0, 5, 0, 1, 0, 2])?;
    let lng = get_f64(&[1, 0, 5, 0, 1, 0, 3])?;

    // Image dimensions
    let image_width = get_u64(&meta, &[1, 0, 2, 2, 1])? as u32;
    let image_height = get_u64(&meta, &[1, 0, 2, 2, 0])? as u32;
    let tile_width = get_u64(&meta, &[1, 0, 2, 3, 1, 1])? as u32;
    let tile_height = get_u64(&meta, &[1, 0, 2, 3, 1, 0])? as u32;

    // Zoom levels
    let zoom_array = get_vec(&[1, 0, 2, 3, 0])?;

    let max_zoom = zoom_array.len().saturating_sub(1);
    let mut zoom_levels = Vec::new();
    for zoom in zoom_array {
        let crop_width = get_u64(zoom, &[0, 1])? as u32;
        let crop_height = get_u64(zoom, &[0, 0])? as u32;
        let num_tiles_x = crop_width.div_ceil(tile_width);
        let num_tiles_y = crop_height.div_ceil(tile_height);
        zoom_levels.push(ZoomLevel {
            crop_width,
            crop_height,
            num_tiles_x,
            num_tiles_y,
        });
    }

    // Heading/Tilt/Roll
    let heading_tilt_roll_arr = get_vec(&[1, 0, 5, 0, 1]);
    let (heading, tilt, roll) = if let Ok(arr) = heading_tilt_roll_arr {
        if arr.len() >= 3 {
            let inner = arr[2]
                .as_array()
                .ok_or(anyhow!("Failed to get meta[1][0][5][0][1][2]"))?;
            (
                inner
                    .first()
                    .ok_or(anyhow!("Failed to get meta[1][0][5][0][1][2][0]"))?
                    .as_f64()
                    .unwrap_or(0.0),
                inner
                    .get(1)
                    .ok_or(anyhow!("Failed to get meta[1][0][5][0][1][2][1]"))?
                    .as_f64()
                    .unwrap_or(90.0),
                inner
                    .get(2)
                    .ok_or(anyhow!("Failed to get meta[1][0][5][0][1][2][2]"))?
                    .as_f64()
                    .unwrap_or(0.0),
            )
        } else {
            (0.0, 90.0, 0.0)
        }
    } else {
        (0.0, 90.0, 0.0)
    };

    Ok(PanoMetadata {
        pano,
        lat,
        lng,
        image_width,
        image_height,
        tile_width,
        tile_height,
        max_zoom,
        zoom_levels,
        heading,
        tilt,
        roll,
    })
}

#[instrument(skip_all, level = Level::TRACE)]
async fn load_tile(tile: &Tile, client: &Client) -> anyhow::Result<RgbaImage> {
    let query_url = match tile.pano.pano_type {
        PanoType::Official => {
            format!(
                "https://streetviewpixels-pa.googleapis.com/v1/tile?cb_client=apiv3&panoid={}&output=tile&x={}&y={}&zoom={}&nbt=1&fover=2",
                tile.pano.id, tile.x, tile.y, tile.zoom
            )
        }
        PanoType::Unofficial => {
            format!(
                "https://lh3.ggpht.com/jsapi2/a/b/c/x{}-y{}-z{}/{}",
                tile.x, tile.y, tile.zoom, tile.pano.id
            )
        }
    };

    debug!("Fetching tile from URL: {}", query_url);

    let resp = client.get(&query_url).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!("Request failed with status: {}", resp.status());
    }

    let resp = resp.bytes().await?;

    let cursor = Cursor::new(resp);

    // Decode the image
    let mut img = ImageReader::new(cursor); // auto-detect JPEG/PNG/etc

    img.set_format(image::ImageFormat::Jpeg);

    let img = img.decode()?;

    Ok(img.to_rgba8())
}

/// Asynchrounously fetch and stitch together all the tiles for a given pano
/// TODO: allow specifing the zoom level, for better perfs
///
/// # Errors
/// This fail if any of the tiles fail to load
///
/// TODO: render blank squares instead of failing
#[instrument(level = Level::DEBUG)]
pub async fn load_equirect(meta: &PanoMetadata) -> anyhow::Result<RgbaImage> {
    let zoom = meta.max_zoom.min(3);
    let mut buf = ImageBuffer::new(
        meta.zoom_levels[zoom].crop_width,
        meta.zoom_levels[zoom].crop_height,
    );

    let client = Arc::new(Client::new());

    let mut handles = vec![];

    for y in 0..meta.zoom_levels[zoom].num_tiles_y {
        for x in 0..meta.zoom_levels[zoom].num_tiles_x {
            let tile = Tile {
                pano: meta.pano.clone(),
                x,
                y,
                zoom: zoom as u32,
            };

            let client = client.clone();

            handles.push(task::spawn(async move {
                ((x, y), load_tile(&tile, &client).await)
            }));
        }
    }

    let mut tiles = vec![];

    for handle in handles {
        tiles.push(handle.await?);
    }

    info!("All tiles loaded, assembling final image...");

    for ((x, y), tile) in tiles {
        let mut tile = tile?;
        let x_offset = x * meta.tile_width;
        let y_offset = y * meta.tile_height;

        if x_offset + tile.width() > buf.width() || y_offset + tile.height() > buf.height() {
            // Crop the tile if it is too large
            let crop_width = (buf.width() - x_offset).min(tile.width());
            let crop_height = (buf.height() - y_offset).min(tile.height());
            tile = image::imageops::crop_imm(&tile, 0, 0, crop_width, crop_height).to_image();
        }

        buf.copy_from(&tile, x_offset, y_offset)?;
    }

    // buf.save("equirect.png").expect("Equirect saving failed");

    Ok(buf)
}

#[instrument(level = Level::TRACE)]
fn map_to_sphere(x: f32, y: f32, z: f32, yaw: f32, pitch: f32) -> (f32, f32) {
    let theta = f32::acos(z / (x * x + y * y + z * z).sqrt());
    let phi = f32::atan2(y, x);

    let theta_prime = f32::acos(theta.sin() * phi.sin() * pitch.sin() + theta.cos() * pitch.cos());

    let mut phi_prime = f32::atan2(
        theta.sin() * phi.sin() * pitch.cos() - theta.cos() * pitch.sin(),
        theta.sin() * phi.cos(),
    );

    phi_prime += yaw;

    phi_prime %= 2. * PI;

    (theta_prime, phi_prime)
}

#[instrument(skip(pano), level = Level::TRACE)]
fn interpolate_color(x: f32, y: f32, pano: &RgbaImage) -> Rgba<u8> {
    let x = x.rem_euclid(pano.width() as f32 - 1.0);
    let y = y.clamp(0., (pano.height() - 1) as f32);

    imageops::interpolate_bilinear(pano, x, y).unwrap()
}

/// Render an equirectangular projection to a 2d plane, with yaw and pitch.
///
/// Thanks to <https://blogs.codingballad.com/unwrapping-the-view-transforming-360-panoramas-into-intuitive-videos-with-python-6009bd5bca94>
/// for this code
#[instrument(skip(pano), level = Level::TRACE)]
fn pano_to_plane(
    pano: &RgbaImage,
    fov: f32,
    out_w: u32,
    out_h: u32,
    yaw: f32,
    pitch: f32,
    roll: f32,
) -> RgbaImage {
    let (pano_width, pano_height) = pano.dimensions();
    let yaw_radian = yaw.to_radians();
    let pitch_radian = pitch.to_radians();
    let roll_radian = roll.to_radians();

    let mut out = RgbaImage::new(out_w, out_h);

    let out_width = out_w as f32;
    let out_height = out_h as f32;
    let focal_len = (0.5 * out_width) / (fov.to_radians() / 2.0).tan();
    info!("focal length is {focal_len}");

    for u in 0..out_width as u32 {
        for v in 0..out_height as u32 {
            let x = u as f32 - out_width * 0.5;
            let y = out_height * 0.5 - v as f32;
            let z = focal_len;

            // Apply roll
            let x_rot = x * roll_radian.cos() - y * roll_radian.sin();
            let y_rot = x * roll_radian.sin() + y * roll_radian.cos();

            let (theta, phi) = map_to_sphere(x_rot, y_rot, z, yaw_radian, pitch_radian);

            let sphere_u = phi * pano_width as f32 / (2. * PI);
            let sphere_v = theta * pano_height as f32 / PI;

            let sphere_u = sphere_u.rem_euclid(pano_width as f32);
            let sphere_v = sphere_v.clamp(0., pano_height as f32 - 1.);

            let color = interpolate_color(sphere_u, sphere_v, pano);
            out.put_pixel(u, v, color);
        }
    }

    out
}

/// Render a pano from it's metadata, fetching the tiles and rendering them into the output buffer
///
/// # Errors
/// This can fail if we fail to fetch any of the tiles
/// TODO: Render a blank space instead of failing
fn create_out_tex_view(device: &Device, width: u32, height: u32) -> TextureView {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("Out texture"),
        size: Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: TextureUsages::COPY_SRC | TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&TextureViewDescriptor::default())
}

/// Render a pano from it's metadata, fetching the tiles and rendering them into the output buffer
///
/// # Errors
/// This can fail if we fail to fetch any of the tiles
/// TODO: Render a blank space instead of failing
// #[instrument(skip(pano, meta), level = Level::DEBUG)]
#[allow(
    clippy::default_trait_access,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_lossless
)]
pub async fn render_pano_from_metadata(
    meta: &PanoMetadata,
    pano: &RgbaImage,
    heading: f32,
    out_w: u32,
    out_h: u32,
) -> anyhow::Result<RgbaImage> {
    let before_render = Instant::now();

    // Note: this first part is slow, be sure to only do this once in prod

    // Request an instance, the entry point to wgpu
    let instance = Instance::new(InstanceDescriptor::new_without_display_handle());

    // Request an adapter
    let adapter = instance.request_adapter(&Default::default()).await?; // Default options

    // Request access to the actual GPU
    let (device, queue) = adapter.request_device(&Default::default()).await?;

    // Create the Offscreen Render Target Texture

    let tex_view = create_out_tex_view(&device, out_w, out_h);

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

    let in_tex = device.create_texture(&TextureDescriptor {
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
        usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let in_tex_view = &in_tex.create_view(&Default::default());

    let sampler = device.create_sampler(&Default::default());

    queue.write_texture(
        TexelCopyTextureInfo {
            texture: &in_tex,
            mip_level: 0,
            origin: Origin3d::ZERO,
            aspect: TextureAspect::All,
        },
        pano,
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(pano.width() * 4),
            rows_per_image: None,
        },
        in_tex.size(),
    );

    // Compile and create the shader

    let shader = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("Equirectangular shader"),
        source: ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
    });

    // Create a rendering pipeline

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

        let mut meta_cache = None;

        let mut equirect_cache = None;

        let mut cur_heading = 0.0;

        let mut cur_panoid = String::new();

        while let Some(request) = pano_rx.recv().await {
            let mut needs_render = false;
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
                    needs_render = true;
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
                        needs_render = true;
                    }
                }
            }

            // Now only do the necessary work
            if needs_render {
                // Fetch the current pano if needed
                info!(panoid = %cur_panoid, "Fetching panorama metadata");

                // Render the pano
                let meta = match get_pano_metadata_from_id(&cur_panoid).await {
                    Ok(ok) => {
                        info!(
                            panoid = %cur_panoid,
                            image_width = ok.image_width,
                            image_height = ok.image_height,
                            max_zoom = ok.max_zoom,
                            "Fetched panorama metadata"
                        );
                        ok
                    }
                    Err(err) => {
                        error!(
                            error = %err,
                            panoid = %cur_panoid,
                            "Failed to fetch panorama metadata"
                        );
                        continue;
                    }
                };

                let equirect = match load_equirect(&meta).await {
                    Ok(ok) => {
                        info!(
                            panoid = %cur_panoid,
                            width = ok.width(),
                            height = ok.height(),
                            "Loaded equirectangular image"
                        );
                        ok
                    }
                    Err(err) => {
                        error!(
                            error = %err,
                            panoid = %cur_panoid,
                            "Failed to load equirectangular image"
                        );
                        continue;
                    }
                };

                meta_cache = Some(meta.clone());
                equirect_cache = Some(equirect.clone());
            }

            if let Some(equirect) = &equirect_cache
                && let Some(meta) = &meta_cache
                && (needs_render || needs_resize)
            {
                let width = cur_size.0 * font_size.0;
                let height = cur_size.1 * font_size.1;

                let pano = match render_pano_from_metadata(
                    meta,
                    equirect,
                    cur_heading as f32,
                    width as u32,
                    height as u32,
                )
                .await
                {
                    Ok(pano) => {
                        debug!("Rendered panorama for display");
                        pano
                    }
                    Err(err) => {
                        error!(error = %err, panoid = %cur_panoid, "Failed to render panorama");
                        continue;
                    }
                };

                let protocol = match picker.new_protocol(
                    pano.into(),
                    Rect::new(0, 0, width, height),
                    Resize::Crop(None),
                ) {
                    Ok(proto) => proto,
                    Err(err) => {
                        error!(
                            error = ?err,
                            panoid = %cur_panoid,
                            "Failed to create display protocol"
                        );
                        continue;
                    }
                };

                info!(panoid = %cur_panoid, "Displaying new frame");
                let _ = evt_sender.send(Event::App(AppEvent::NewFrame(protocol)));
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};
    use tokio::sync::mpsc::{channel, unbounded_channel};

    #[test]
    fn test_decode_panoid() {
        // official-looking pano
        let pano = decode_panoid("tXVQoL_JtBEBbV7LYKW_2A");
        assert_eq!(pano.pano_type, PanoType::Official);
        assert_eq!(pano.id, "tXVQoL_JtBEBbV7LYKW_2A");

        // unofficial / malformed
        let pano = decode_panoid("CAoSFkNJSE0wb2dLRUlDQWdJQ0U5SVBWR1E.");
        assert_eq!(pano.pano_type, PanoType::Unofficial);
        assert_eq!(pano.id, "CIHM0ogKEICAgICE9IPVGQ");
    }

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

    #[test]
    fn test_map_to_sphere() {
        let (theta, phi) = map_to_sphere(1.0, 2.0, 3.0, 0.0, 0.0);

        assert!(theta.is_finite());
        assert!(phi.is_finite());
        assert!((0.0..=std::f32::consts::PI).contains(&theta));

        let (theta, _) = map_to_sphere(0.0, 0.0, 1.0, 0.0, 0.0);

        // should point straight ahead → near 0
        assert!(theta.abs() < 1e-5);

        let (_, phi1) = map_to_sphere(1.0, 0.0, 1.0, 0.0, 0.0);
        let (_, phi2) = map_to_sphere(1.0, 0.0, 1.0, 1.0, 0.0);

        assert_ne!(phi1, phi2);

        for i in 0..1000 {
            let x = (i as f32).sin();
            let y = (i as f32).cos();
            let z = 1.0;

            let (theta, phi) = map_to_sphere(x, y, z, 0.3, 0.7);

            assert!(theta.is_finite());
            assert!(phi.is_finite());
        }
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
            pano: super::Pano {
                pano_type: super::PanoType::Official,
                id: "fake".to_string(),
            },
            lat: 0.0,
            lng: 0.0,
            image_width: 2,
            image_height: 2,
            tile_width: 2,
            tile_height: 2,
            max_zoom: 0,
            zoom_levels: vec![super::ZoomLevel {
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
        let rendered = super::render_pano_from_metadata(&meta, &pano, 0.0, 4, 4)
            .await
            .unwrap();

        assert_eq!(rendered.width(), 4);
        assert_eq!(rendered.height(), 4);
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
