struct Camera { orbit: vec4<f32>, display: vec4<f32> }
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var picture: texture_2d<f32>;
@group(0) @binding(2) var filtering: sampler;
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn mesh_vertex(@location(0) position: vec3<f32>, @location(1) uv: vec2<f32>) -> Vertex {
    let yaw = camera.orbit.x;
    let pitch = camera.orbit.y;
    let p = vec3<f32>(cos(yaw)*position.x+sin(yaw)*position.z, position.y, -sin(yaw)*position.x+cos(yaw)*position.z);
    let q = vec3<f32>(p.x, cos(pitch)*p.y-sin(pitch)*p.z, sin(pitch)*p.y+cos(pitch)*p.z);
    let distance = camera.orbit.z-q.z;
    var out: Vertex;
    out.position = vec4<f32>(q.x*2.4/camera.orbit.w, q.y*2.4, (distance-0.05)*20.0/19.95, distance);
    out.uv = uv;
    return out;
}
@fragment fn mesh_fragment(in: Vertex) -> @location(0) vec4<f32> {
    let color = textureSample(picture, filtering, in.uv);
    if color.a < 0.01 { discard; }
    return color;
}
@vertex fn blit_vertex(@builtin(vertex_index) i: u32) -> Vertex {
    let xy = vec2<f32>(f32((i<<1u)&2u), f32(i&2u));
    var out: Vertex;
    out.position = vec4<f32>(xy*2.0-1.0, 0.0, 1.0);
    out.uv = vec2<f32>(xy.x, 1.0-xy.y);
    return out;
}
@fragment fn blit_fragment(in: Vertex) -> @location(0) vec4<f32> {
    let color = textureSample(picture, filtering, in.uv);
    let encoded = select(12.92*color.rgb, 1.055*pow(max(color.rgb,vec3<f32>(0.0)),vec3<f32>(1.0/2.4))-0.055, color.rgb>vec3<f32>(0.0031308));
    return vec4<f32>(select(color.rgb,encoded,camera.display.x>0.5),1.0);
}
