use std::{f32::consts::PI, io::Cursor, sync::Arc, time::Instant};

use anyhow::anyhow;
use base64::{Engine as _, engine::general_purpose};
use image::{GenericImage, ImageBuffer, ImageReader, Rgb, RgbImage, imageops};
use reqwest::Client;
use serde_json::Value;
use tokio::task;
use tracing::{debug, info};

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

        let bytes = match general_purpose::STANDARD.decode(&b64) {
            Ok(b) => b,
            Err(_) => return None,
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
            _ => PanoType::Official,
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

/// Fetch metadata for a panorama ID using the MapsJs internal service.
/// Returns `None` on any network/error condition.
///
/// Stolen from Mikarific/LookoutTheWindow
///
/// TODO: make this AI slop a little less sloppy
pub async fn get_pano_metadata_from_id(pano_id: &str) -> anyhow::Result<PanoMetadata> {
    let pano = decode_panoid(pano_id);
    let type_num = pano.pano_type as u8;

    let payload = format!(
        "[[\"apiv3\"],[\"en\",\"US\"],[[[{type_num},\"{id}\"]]],[[1,4]]]",
        type_num = type_num,
        id = pano.id
    );

    let client = reqwest::Client::new();
    let res = client
        .post("https://maps.googleapis.com/$rpc/google.internal.maps.mapsjs.v1.MapsJsInternalService/GetMetadata")
        .header("Content-Type", "application/json+protobuf")
        .body(payload)
        .send().await?;

    if !res.status().is_success() {
        anyhow::bail!("Request failed with status: {}", res.status());
    }
    let meta: Value = res.json().await?;

    // extract the simple fields
    let pano_vec = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][1]"))?;
    let p_type = pano_vec
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][1][0] (pano type)"))?
        .as_i64()
        .ok_or(anyhow!("invalid pano type (expected integer)"))?;
    let p_id = pano_vec
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][1][1] (pano id)"))?
        .as_str()
        .ok_or(anyhow!("invalid pano id (expected string)"))?
        .to_owned();

    assert_eq!(p_type as u8, pano.pano_type as u8);
    assert_eq!(p_id, pano.id);

    let lat = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(5)
        .ok_or(anyhow!("missing meta[1][0][5]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][5][0]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][5][0][1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][5][0][1][0]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][5][0][1][0][2] (latitude)"))?
        .as_f64()
        .ok_or(anyhow!("invalid latitude (expected number)"))?;
    let lng = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(5)
        .ok_or(anyhow!("missing meta[1][0][5]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][5][0]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][5][0][1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][5][0][1][0]"))?
        .get(3)
        .ok_or(anyhow!("missing meta[1][0][5][0][1][0][3] (longitude)"))?
        .as_f64()
        .ok_or(anyhow!("invalid longitude (expected number)"))?;

    let image_width = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][2]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][2][2]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][2][2][1] (image width)"))?
        .as_u64()
        .ok_or(anyhow!("invalid image width (expected integer)"))? as u32;
    let image_height = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][2]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][2][2]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][2][2][0] (image height)"))?
        .as_u64()
        .ok_or(anyhow!("invalid image height (expected integer)"))? as u32;
    let tile_width = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][2]"))?
        .get(3)
        .ok_or(anyhow!("missing meta[1][0][2][3]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][2][3][1]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][2][3][1][1] (tile width)"))?
        .as_u64()
        .ok_or(anyhow!("invalid tile width (expected integer)"))? as u32;
    let tile_height = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][2]"))?
        .get(3)
        .ok_or(anyhow!("missing meta[1][0][2][3]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][2][3][1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][2][3][1][0] (tile height)"))?
        .as_u64()
        .ok_or(anyhow!("invalid tile height (expected integer)"))? as u32;

    let zoom_array = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(2)
        .ok_or(anyhow!("missing meta[1][0][2]"))?
        .get(3)
        .ok_or(anyhow!("missing meta[1][0][2][3]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][2][3][0] (zoom levels)"))?
        .as_array()
        .ok_or(anyhow!("invalid zoom levels (expected array)"))?;
    let max_zoom = zoom_array.len().saturating_sub(1);
    let mut zoom_levels = Vec::new();
    for zoom in zoom_array {
        let crop_width =
            zoom.get(0)
                .ok_or(anyhow!("missing zoom[0]"))?
                .get(1)
                .ok_or(anyhow!("missing zoom[0][1] (crop width)"))?
                .as_u64()
                .ok_or(anyhow!("invalid crop width (expected integer)"))? as u32;
        let crop_height =
            zoom.get(0)
                .ok_or(anyhow!("missing zoom[0]"))?
                .get(0)
                .ok_or(anyhow!("missing zoom[0][0] (crop height)"))?
                .as_u64()
                .ok_or(anyhow!("invalid crop height (expected integer)"))? as u32;
        let num_tiles_x = crop_width.div_ceil(tile_width) as u32;
        let num_tiles_y = crop_height.div_ceil(tile_height) as u32;
        zoom_levels.push(ZoomLevel {
            crop_width,
            crop_height,
            num_tiles_x,
            num_tiles_y,
        });
    }

    let heading_tilt_roll_arr = meta
        .get(1)
        .ok_or(anyhow!("missing meta[1]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0]"))?
        .get(5)
        .ok_or(anyhow!("missing meta[1][0][5]"))?
        .get(0)
        .ok_or(anyhow!("missing meta[1][0][5][0]"))?
        .get(1)
        .ok_or(anyhow!("missing meta[1][0][5][0][1] (heading/tilt/roll)"))?
        .as_array();
    let (heading, tilt, roll) = if let Some(arr) = heading_tilt_roll_arr {
        if arr.len() >= 3 {
            let inner = arr[2].as_array().ok_or(anyhow!(
                "missing meta[1][0][5][0][1][2] (heading/tilt/roll array)"
            ))?;
            (
                inner
                    .first()
                    .ok_or(anyhow!("missing heading value"))?
                    .as_f64()
                    .unwrap_or(0.0),
                inner
                    .get(1)
                    .ok_or(anyhow!("missing tilt value"))?
                    .as_f64()
                    .unwrap_or(90.0),
                inner
                    .get(2)
                    .ok_or(anyhow!("missing roll value"))?
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

async fn load_tile(tile: &Tile, client: &Client) -> anyhow::Result<RgbImage> {
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

    Ok(img.to_rgb8())
}

pub async fn load_equirect(meta: &PanoMetadata) -> anyhow::Result<RgbImage> {
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

fn interpolate_color(x: f32, y: f32, pano: &RgbImage) -> Rgb<u8> {
    let x = x.rem_euclid(pano.width() as f32 - 1.0);
    let y = y.clamp(0., (pano.height() - 1) as f32);

    imageops::interpolate_bilinear(pano, x, y).unwrap()
}

/// Render an equirectangular projection to a 2d plane, with yaw and pitch.
///
/// Thanks to https://blogs.codingballad.com/unwrapping-the-view-transforming-360-panoramas-into-intuitive-videos-with-python-6009bd5bca94
/// for this code
fn pano_to_plane(
    pano: &RgbImage,
    fov: f32,
    out_w: u32,
    out_h: u32,
    yaw: f32,
    pitch: f32,
    roll: f32,
) -> RgbImage {
    let (pano_width, pano_height) = pano.dimensions();
    let yaw_radian = yaw.to_radians();
    let pitch_radian = pitch.to_radians();
    let roll_radian = roll.to_radians();

    let mut out = RgbImage::new(out_w, out_h);

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

            let color = interpolate_color(sphere_u as f32, sphere_v as f32, pano);
            out.put_pixel(u, v, color);
        }
    }

    out
}

pub async fn render_pano_from_metadata(
    meta: &PanoMetadata,
    pano: &RgbImage,
    heading: f32,
    out_w: u32,
    out_h: u32,
) -> anyhow::Result<RgbImage> {
    let before_load = Instant::now();

    let load_ms = before_load.elapsed().as_secs_f64() * 1000.0;

    let before_render = Instant::now();

    let rendered = pano_to_plane(
        pano,
        90.,
        out_w,
        out_h,
        (270. - meta.heading as f32 + heading as f32).rem_euclid(360.), // This seems to work, let's not mess with it, OK
        meta.tilt as f32,
        -meta.roll as f32,
    );

    let time_ms = before_render.elapsed().as_secs_f64() * 1000.0;

    let total_ms = before_load.elapsed().as_secs_f64() * 1000.0;

    info!("Load: {load_ms}ms, render: {time_ms}ms, total: {total_ms}ms");

    Ok(rendered)
}
