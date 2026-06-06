struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) clip_coords: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    
    // Generates a fullscreen triangle covering clip space coordinates [-1, 1]
    let x = f32(i32(vertex_index << 1u) & 2) * 2.0 - 1.0;
    let y = f32(i32(vertex_index & 2u)) * 2.0 - 1.0;
    
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.clip_coords = vec2<f32>(x, y);
    return out;
}


@group(0) @binding(0) var<uniform> camera: mat4x4<f32>;
@group(1) @binding(0) var t_pano: texture_2d<f32>;
@group(1) @binding(1) var s_pano: sampler;

const PI: f32 = 3.14159265359;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // 1. Transform the screen/clip space coordinate into a 3D world space direction ray
    let raw_ray = camera * vec4<f32>(in.clip_coords.x, in.clip_coords.y, 1.0, 1.0);
    let ray_dir = normalize(raw_ray.xyz / raw_ray.w);

    // 2. Spherical Mapping Math (Assuming Y-Up coordinate system)
    let theta = atan2(ray_dir.z, ray_dir.x); // Longitude: [-PI, PI]
    let phi = asin(ray_dir.y);               // Latitude:  [-PI/2, PI/2]

    // 3. Map to [0.0, 1.0] range for UV texture coordinates
    let u = (theta + PI) / (2.0 * PI);
    let v = 0.5 - (phi / PI); 

    // 4. Sample the panorama texture
    // CRITICAL: We use textureSampleLevel instead of textureSample to prevent 
    // a visible mipmapping seam artifact where theta wraps from PI to -PI.
    return textureSampleLevel(t_pano, s_pano, vec2<f32>(u, v), 0.0);
}