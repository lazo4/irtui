//! Abstraction over the street view api, most of this is adapted from <https://github.com/Mikarific/LookoutTheWindow>

use std::{io::Cursor, sync::Arc};

use anyhow::anyhow;
use image::{GenericImage, ImageBuffer, ImageReader, RgbaImage};
use serde_json::Value;
use tokio::task;
use tracing::{Level, debug, info, instrument};
use wreq::Client;

// Whether a pano is official (google coverage) or unofficial (orbs, or the infamous sanjay coverage)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PanoType {
    Official = 2,
    Unofficial = 10,
}

pub struct Tile {
    pub pano: Pano,
    pub x: u32,
    pub y: u32,
    pub zoom: u32,
}

/// Result produced by `decode_panoid`.
#[derive(Debug, Clone)]
pub struct Pano {
    pub pano_type: PanoType,
    pub id: String,
}

/// Minimal metadata needed for rendering a pano.
#[derive(Debug, Clone)]
pub struct ZoomLevel {
    pub crop_width: u32,
    pub crop_height: u32,
    pub num_tiles_x: u32,
    pub num_tiles_y: u32,
}

#[derive(Debug, Clone)]
#[allow(unused)]
pub struct PanoMetadata {
    pub pano: Pano,
    pub lat: f64,
    pub lng: f64,
    pub image_width: u32,
    pub image_height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub max_zoom: usize,
    pub zoom_levels: Vec<ZoomLevel>,
    pub heading: f64,
    pub tilt: f64,
    pub roll: f64,
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

        let Ok(bytes) = base64::decode(&b64) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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
}
