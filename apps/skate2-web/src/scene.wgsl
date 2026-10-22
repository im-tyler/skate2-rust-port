// The web build's standalone test-scene shader. This is not a reconstruction
// of a Skate 2 material (those live in apps/skate2-pc/src/gpu_renderer/ and
// will move into the browser build later); it only proves the WebGPU pipeline.

struct FrameUniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> frame: FrameUniforms;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world = frame.model * vec4<f32>(input.position, 1.0);
    output.clip_position = frame.view_proj * world;
    output.world_position = world.xyz;
    // The test scene's models only rotate and translate, so the model matrix
    // carries the normals too.
    output.normal = (frame.model * vec4<f32>(input.normal, 0.0)).xyz;
    output.color = input.color;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let sun_direction = normalize(vec3<f32>(0.45, 1.0, 0.35));
    let normal = normalize(input.normal);
    let lambert = max(dot(normal, sun_direction), 0.0);
    // A hemisphere ambient: sky above, plaza-coloured ground below.
    let up = normal.y * 0.5 + 0.5;
    let ambient = mix(vec3<f32>(0.28, 0.27, 0.25), vec3<f32>(0.45, 0.5, 0.58), up);
    let lit = input.color * (ambient + lambert * vec3<f32>(0.75, 0.72, 0.65));
    return vec4<f32>(lit, 1.0);
}
