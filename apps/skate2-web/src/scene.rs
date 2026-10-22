//! The browser build's standalone test scene: a plaza with a skate deck that
//! answers the pad, drawn with wgpu on WebGPU (WebGL2 as the fallback
//! backend). This is not Skate 2 gameplay or a Skate 2 material — the real
//! renderer and the reconstructed session arrive in a later pass
//! (`docs/WEB_PORT.md`). What it proves now: the WASM target renders through
//! wgpu with the repo's winit 0.29/wgpu 30 pairing, and `XboxPadSample`s from
//! the browser pad platform visibly reach a scene.

use skate2_core::XboxPadSample;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

const SHADER: &str = include_str!("scene.wgsl");

/// Deck dimensions in metres: a 0.82 m deck on 0.055 m wheels.
const DECK_LENGTH: f32 = 0.82;
const DECK_WIDTH: f32 = 0.215;
const DECK_THICKNESS: f32 = 0.012;
const WHEEL_RADIUS: f32 = 0.055;
/// Push speed and turn rate of the demo deck.
const PUSH_SPEED: f32 = 3.2;
const TURN_RATE: f32 = 2.2;
/// The demo hop's launch speed, in metres per second.
const HOP_SPEED: f32 = 3.0;

/// One drawable vertex: position, normal, color.
#[derive(Clone, Copy)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
}

/// Uploaded geometry: one indexed mesh.
struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
}

/// A uniform buffer with the view-projection and one model matrix, and its
/// bind group; one per draw.
struct DrawSet {
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// What went wrong presenting a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PresentProblem {
    /// The surface needs reconfiguring, then the next frame will draw.
    Reconfigure,
    /// Nothing to present this frame (the tab is hidden or the frame timed out).
    Skip,
    /// The presentation failed in a way the scene cannot recover from.
    Fatal,
}

impl PresentProblem {
    fn of(status: wgpu::CurrentSurfaceTexture) -> Result<wgpu::SurfaceTexture, Self> {
        match status {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => Ok(texture),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                Err(Self::Reconfigure)
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                Err(Self::Skip)
            }
            wgpu::CurrentSurfaceTexture::Validation => Err(Self::Fatal),
        }
    }
}

/// Where the deck is and how it is tilted.
#[derive(Clone, Copy, Default)]
struct DeckPose {
    position: [f32; 2],
    yaw: f32,
    height: f32,
    vertical_speed: f32,
    roll: f32,
    pitch: f32,
}

/// The test-scene renderer: surface, pipeline, plaza geometry and the deck.
pub(crate) struct PlazaScene {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    configuration: wgpu::SurfaceConfiguration,
    depth: wgpu::TextureView,
    depth_size: (u32, u32),
    pipeline: wgpu::RenderPipeline,
    plaza: Mesh,
    deck: Mesh,
    plaza_set: DrawSet,
    deck_set: DrawSet,
    pose: DeckPose,
    adapter: String,
}

impl PlazaScene {
    /// Creates the surface, adapter, device and static geometry for `window`.
    ///
    /// # Errors
    ///
    /// No WebGPU/WebGL adapter or device, or no usable surface format, on
    /// this browser or GPU.
    pub(crate) async fn new(window: Arc<Window>) -> Result<Self, Box<dyn std::error::Error>> {
        let backends = if cfg!(target_arch = "wasm32") {
            // WebGPU first; GL is wgpu's WebGL2 backend in the browser.
            wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL
        } else {
            wgpu::Backends::PRIMARY
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|error| format!("no compatible GPU adapter in the browser: {error}"))?;
        let adapter_name = format!("{:?}", adapter.get_info().backend).to_lowercase();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("skate2 web test scene"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                ..Default::default()
            })
            .await?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| {
                matches!(
                    format,
                    wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
                )
            })
            .or_else(|| capabilities.formats.first().copied())
            .ok_or("the surface supports no render formats")?;
        let alpha_mode = capabilities
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        let size = window.inner_size();
        let configuration = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: Vec::new(),
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &configuration);

        let (pipeline, uniform_layout) = pipeline(&device, format);
        let plaza_set = draw_set(&device, &uniform_layout);
        let deck_set = draw_set(&device, &uniform_layout);
        let plaza = upload(&device, &plaza_geometry());
        let deck = upload(&device, &deck_geometry());
        let depth_size = (configuration.width, configuration.height);
        let depth = depth_view(&device, depth_size);
        Ok(Self {
            window,
            surface,
            device,
            queue,
            configuration,
            depth,
            depth_size,
            pipeline,
            plaza,
            deck,
            plaza_set,
            deck_set,
            pose: DeckPose::default(),
            adapter: adapter_name,
        })
    }

    /// The adapter family that is rendering, for the debug HUD.
    #[must_use]
    pub(crate) fn adapter_backend(&self) -> &str {
        &self.adapter
    }

    /// Reconfigures the surface for a new window size.
    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.configuration.width = width;
        self.configuration.height = height;
        self.surface.configure(&self.device, &self.configuration);
        if self.depth_size != (width, height) {
            self.depth_size = (width, height);
            self.depth = depth_view(&self.device, self.depth_size);
        }
    }

    /// Advances the demo deck with one pad sample and draws a frame.
    ///
    /// # Errors
    ///
    /// See [`PresentProblem`].
    pub(crate) fn render(&mut self, sample: XboxPadSample, dt: f32) -> Result<(), PresentProblem> {
        self.advance_deck(sample, dt);

        let frame = PresentProblem::of(self.surface.get_current_texture())?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.ensure_depth();
        #[expect(
            clippy::cast_precision_loss,
            reason = "a window width has at most 24 significant bits, exactly what f32 holds"
        )]
        let aspect = self.configuration.width as f32 / self.configuration.height.max(1) as f32;
        let view_matrix = camera(self.pose);
        let view_proj = multiply(&perspective(0.9, aspect, 0.05, 120.0), &view_matrix);
        let identity = translation(0.0, 0.0, 0.0);
        let deck = deck_model(&self.pose);
        self.queue.write_buffer(
            &self.plaza_set.uniforms,
            0,
            &matrix_bytes(&view_proj, &identity),
        );
        self.queue
            .write_buffer(&self.deck_set.uniforms, 0, &matrix_bytes(&view_proj, &deck));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            multiview_mask: None,
            label: Some("skate2 web test scene pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.52,
                        g: 0.66,
                        b: 0.84,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &self.depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.plaza_set.bind_group, &[]);
        pass.set_vertex_buffer(0, self.plaza.vertices.slice(..));
        pass.set_index_buffer(self.plaza.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.plaza.index_count, 0, 0..1);
        pass.set_bind_group(0, &self.deck_set.bind_group, &[]);
        pass.set_vertex_buffer(0, self.deck.vertices.slice(..));
        pass.set_index_buffer(self.deck.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.deck.index_count, 0, 0..1);
        drop(pass);
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.window.pre_present_notify();
        Ok(())
    }

    /// Recreates the depth view when the surface size changed since it was made.
    fn ensure_depth(&mut self) {
        if self.depth_size == (self.configuration.width, self.configuration.height) {
            return;
        }
        self.depth_size = (self.configuration.width, self.configuration.height);
        self.depth = depth_view(&self.device, self.depth_size);
    }

    /// The demo deck's own idea of movement: the left stick carves, the right
    /// stick tilts, A hops, the triggers pitch. Real skating is the
    /// reconstructed session's job; this only makes the pad's sample visible.
    fn advance_deck(&mut self, sample: XboxPadSample, dt: f32) {
        let axis = |value: i16| f32::from(value) / 32767.0;
        let forward = axis(sample.left_y);
        let steer = axis(sample.left_x);
        let (sin, cos) = self.pose.yaw.sin_cos();
        self.pose.position[0] += cos * forward * PUSH_SPEED * dt;
        self.pose.position[1] += sin * forward * PUSH_SPEED * dt;
        self.pose.yaw += steer * forward.abs() * TURN_RATE * dt;

        // A press hops; gravity brings the deck back down.
        if sample.buttons & 0x1000 != 0
            && self.pose.height == 0.0
            && self.pose.vertical_speed == 0.0
        {
            self.pose.vertical_speed = HOP_SPEED;
        }
        self.pose.vertical_speed -= 9.81 * dt;
        self.pose.height = (self.pose.height + self.pose.vertical_speed * dt).max(0.0);
        if self.pose.height == 0.0 && self.pose.vertical_speed < 0.0 {
            self.pose.vertical_speed = 0.0;
        }

        // The right stick rolls and pitches the deck; the triggers add pitch.
        let target_roll = axis(sample.right_x);
        let target_pitch = axis(sample.right_y) * 0.5
            + f32::from(sample.right_trigger) / 255.0 * 0.35
            - f32::from(sample.left_trigger) / 255.0 * 0.35;
        let ease = |current: f32, target: f32| current + (target - current) * (dt * 10.0).min(1.0);
        self.pose.roll = ease(self.pose.roll, target_roll);
        self.pose.pitch = ease(self.pose.pitch, target_pitch);
    }
}

/// The render pipeline for both draws, with its uniform bind group layout.
fn pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("skate2 web test scene shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("skate2 web frame uniforms"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("skate2 web test scene pipeline layout"),
        bind_group_layouts: &[Some(&uniform_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("skate2 web test scene pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 36,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 12,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x3,
                        offset: 24,
                        shader_location: 2,
                    },
                ],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: Some(wgpu::Face::Back),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth24Plus,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, uniform_layout)
}

/// One uniform set: the view-projection and model matrices, and its binding.
fn draw_set(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> DrawSet {
    let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("skate2 web frame uniforms"),
        contents: &[0_u8; 128],
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("skate2 web frame uniforms bind group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniforms.as_entire_binding(),
        }],
    });
    DrawSet {
        uniforms,
        bind_group,
    }
}

/// One indexed mesh on the GPU.
fn upload(device: &wgpu::Device, mesh: &(Vec<Vertex>, Vec<u32>)) -> Mesh {
    let (vertex_bytes, index_bytes) = mesh_bytes(&mesh.0, &mesh.1);
    let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("skate2 web mesh vertices"),
        contents: &vertex_bytes,
        usage: wgpu::BufferUsages::VERTEX,
    });
    let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("skate2 web mesh indices"),
        contents: &index_bytes,
        usage: wgpu::BufferUsages::INDEX,
    });
    Mesh {
        vertices,
        indices,
        index_count: u32::try_from(mesh.1.len()).expect("the test scene's meshes fit a u32"),
    }
}

/// A depth view for a surface size.
fn depth_view(device: &wgpu::Device, size: (u32, u32)) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("skate2 web depth"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth24Plus,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The plaza: a checkerboard, a ledge and a rail to skate at.
fn plaza_geometry() -> (Vec<Vertex>, Vec<u32>) {
    // The checkerboard: 20 x 20 one-metre tiles.
    const CELLS: i32 = 20;
    let metre = |cells: i32| f32::from(i16::try_from(cells).expect("grid index fits an i16"));
    let mut builder = MeshBuilder::default();
    for cell_x in -CELLS / 2..CELLS / 2 {
        for cell_z in -CELLS / 2..CELLS / 2 {
            let shade = if (cell_x + cell_z).rem_euclid(2) == 0 {
                [0.62, 0.62, 0.60]
            } else {
                [0.48, 0.48, 0.47]
            };
            builder.quad_y(
                [metre(cell_x), 0.0, metre(cell_z)],
                [metre(cell_x + 1), 0.0, metre(cell_z + 1)],
                shade,
            );
        }
    }
    // A concrete ledge.
    builder.box_([-2.6, 0.2, -1.2], [1.2, 0.4, 0.55], [0.58, 0.57, 0.55]);
    // A rail on two posts.
    builder.box_([2.2, 0.18, -1.8], [0.08, 0.36, 0.08], [0.35, 0.38, 0.42]);
    builder.box_([3.9, 0.18, -1.8], [0.08, 0.36, 0.08], [0.35, 0.38, 0.42]);
    builder.box_([3.05, 0.38, -1.8], [1.9, 0.07, 0.07], [0.72, 0.74, 0.78]);
    builder.finish()
}

/// The deck assembly: grip, wood, two trucks and four wheels.
fn deck_geometry() -> (Vec<Vertex>, Vec<u32>) {
    let mut builder = MeshBuilder::default();
    let wheel_y = WHEEL_RADIUS;
    let deck_y = wheel_y + 0.045;
    // Grip tape above, wood below.
    builder.box_(
        [0.0, deck_y + DECK_THICKNESS / 2.0, 0.0],
        [DECK_LENGTH, DECK_THICKNESS, DECK_WIDTH],
        [0.10, 0.10, 0.11],
    );
    builder.box_(
        [0.0, deck_y - DECK_THICKNESS / 2.0, 0.0],
        [DECK_LENGTH, DECK_THICKNESS * 0.6, DECK_WIDTH],
        [0.78, 0.62, 0.42],
    );
    // Trucks.
    for x in [-DECK_LENGTH / 2.0 + 0.14, DECK_LENGTH / 2.0 - 0.14] {
        builder.box_(
            [x, wheel_y + 0.02, 0.0],
            [0.09, 0.05, DECK_WIDTH * 0.8],
            [0.66, 0.67, 0.70],
        );
    }
    // Wheels.
    for x in [-DECK_LENGTH / 2.0 + 0.14, DECK_LENGTH / 2.0 - 0.14] {
        for z in [-DECK_WIDTH / 2.0 + 0.02, DECK_WIDTH / 2.0 - 0.02] {
            builder.box_(
                [x, wheel_y, z],
                [0.05, WHEEL_RADIUS * 2.0, 0.045],
                [0.94, 0.72, 0.18],
            );
        }
    }
    builder.finish()
}

/// Collects boxes and ground quads into one indexed mesh.
#[derive(Default)]
struct MeshBuilder {
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
}

impl MeshBuilder {
    /// The index of the next pushed vertex.
    fn next_index(&self) -> u32 {
        u32::try_from(self.vertices.len()).expect("the test scene's meshes fit a u32")
    }

    /// A horizontal ground quad between two corners, facing up.
    fn quad_y(&mut self, min: [f32; 3], max: [f32; 3], color: [f32; 3]) {
        let base = self.next_index();
        for position in [
            [min[0], 0.0, min[2]],
            [max[0], 0.0, min[2]],
            [max[0], 0.0, max[2]],
            [min[0], 0.0, max[2]],
        ] {
            self.vertices.push(Vertex {
                position,
                normal: [0.0, 1.0, 0.0],
                color,
            });
        }
        self.indices
            .extend_from_slice(&[base, base + 2, base + 1, base, base + 3, base + 2]);
    }

    /// An axis-aligned box by center and size.
    fn box_(&mut self, center: [f32; 3], size: [f32; 3], color: [f32; 3]) {
        let [cx, cy, cz] = center;
        let [sx, sy, sz] = [size[0] / 2.0, size[1] / 2.0, size[2] / 2.0];
        // Six faces: (normal, four corners), wound counter-clockwise seen
        // from outside so back-face culling keeps them.
        let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
            (
                [0.0, 0.0, 1.0],
                [
                    [cx - sx, cy - sy, cz + sz],
                    [cx + sx, cy - sy, cz + sz],
                    [cx + sx, cy + sy, cz + sz],
                    [cx - sx, cy + sy, cz + sz],
                ],
            ),
            (
                [0.0, 0.0, -1.0],
                [
                    [cx + sx, cy - sy, cz - sz],
                    [cx - sx, cy - sy, cz - sz],
                    [cx - sx, cy + sy, cz - sz],
                    [cx + sx, cy + sy, cz - sz],
                ],
            ),
            (
                [1.0, 0.0, 0.0],
                [
                    [cx + sx, cy - sy, cz - sz],
                    [cx + sx, cy - sy, cz + sz],
                    [cx + sx, cy + sy, cz + sz],
                    [cx + sx, cy + sy, cz - sz],
                ],
            ),
            (
                [-1.0, 0.0, 0.0],
                [
                    [cx - sx, cy - sy, cz + sz],
                    [cx - sx, cy - sy, cz - sz],
                    [cx - sx, cy + sy, cz - sz],
                    [cx - sx, cy + sy, cz + sz],
                ],
            ),
            (
                [0.0, 1.0, 0.0],
                [
                    [cx - sx, cy + sy, cz + sz],
                    [cx + sx, cy + sy, cz + sz],
                    [cx + sx, cy + sy, cz - sz],
                    [cx - sx, cy + sy, cz - sz],
                ],
            ),
            (
                [0.0, -1.0, 0.0],
                [
                    [cx - sx, cy - sy, cz - sz],
                    [cx + sx, cy - sy, cz - sz],
                    [cx + sx, cy - sy, cz + sz],
                    [cx - sx, cy - sy, cz + sz],
                ],
            ),
        ];
        for (normal, corners) in faces {
            let base = self.next_index();
            for position in corners {
                self.vertices.push(Vertex {
                    position,
                    normal,
                    color,
                });
            }
            self.indices
                .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }

    fn finish(self) -> (Vec<Vertex>, Vec<u32>) {
        (self.vertices, self.indices)
    }
}

/// Vertex and index bytes, built field by field: the crate forbids `unsafe`,
/// so there is no `bytemuck`-style reinterpretation of the vertex structs.
fn mesh_bytes(vertices: &[Vertex], indices: &[u32]) -> (Vec<u8>, Vec<u8>) {
    let mut vertex_bytes = Vec::with_capacity(vertices.len() * 36);
    for vertex in vertices {
        for vector in [vertex.position, vertex.normal, vertex.color] {
            for value in vector {
                vertex_bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
    }
    let mut index_bytes = Vec::with_capacity(indices.len() * 4);
    for index in indices {
        index_bytes.extend_from_slice(&index.to_ne_bytes());
    }
    (vertex_bytes, index_bytes)
}

/// The two uniform matrices as bytes.
fn matrix_bytes(view_proj: &[f32; 16], model: &[f32; 16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    for matrix in [view_proj, model] {
        for value in matrix {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
    }
    bytes
}

/// A right-handed perspective projection with the 0..1 depth range wgpu uses:
/// the view space looks down −z, so a point in front of the camera has a
/// positive clip w (`glm::perspective_rh_zo`). The left-handed variant would
/// flip w negative in front of the camera and clip every triangle away.
fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [f32; 16] {
    let focal = 1.0 / (fov_y / 2.0).tan();
    let [m00, m11] = [focal / aspect, focal];
    let depth = far - near;
    // Column-major: [column][row].
    [
        m00,
        0.0,
        0.0,
        0.0,
        0.0,
        m11,
        0.0,
        0.0,
        0.0,
        0.0,
        -far / depth,
        -1.0,
        0.0,
        0.0,
        -near * far / depth,
        0.0,
    ]
}

/// A camera that looks at the deck from behind and above, following its yaw.
fn camera(pose: DeckPose) -> [f32; 16] {
    let (sin, cos) = pose.yaw.sin_cos();
    // Behind the deck's forward direction, 3.4 m back and 1.7 m up.
    let eye = [
        pose.position[0] - cos * 3.4,
        1.7,
        pose.position[1] - sin * 3.4,
    ];
    let target = [pose.position[0], 0.35 + pose.height, pose.position[1]];
    look_at(eye, target, [0.0, 1.0, 0.0])
}

/// A right-handed look-at matrix.
fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [f32; 16] {
    let subtract = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let normalize = |v: [f32; 3]| {
        let length = dot(v, v).sqrt().max(1.0e-6);
        [v[0] / length, v[1] / length, v[2] / length]
    };
    let z_axis = normalize(subtract(eye, target));
    let x_axis = normalize(cross(up, z_axis));
    let y_axis = cross(z_axis, x_axis);
    [
        x_axis[0],
        y_axis[0],
        z_axis[0],
        0.0,
        x_axis[1],
        y_axis[1],
        z_axis[1],
        0.0,
        x_axis[2],
        y_axis[2],
        z_axis[2],
        0.0,
        -dot(x_axis, eye),
        -dot(y_axis, eye),
        -dot(z_axis, eye),
        1.0,
    ]
}

/// Column-major matrix product `a * b`.
fn multiply(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut result = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + row] * b[column * 4 + k];
            }
            result[column * 4 + row] = sum;
        }
    }
    result
}

/// Column-major translation.
fn translation(x: f32, y: f32, z: f32) -> [f32; 16] {
    [
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, x, y, z, 1.0,
    ]
}

/// Column-major rotation about Y.
fn rotation_y(radians: f32) -> [f32; 16] {
    let (sin, cos) = radians.sin_cos();
    [
        cos, 0.0, -sin, 0.0, 0.0, 1.0, 0.0, 0.0, sin, 0.0, cos, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

/// Column-major rotation about X.
fn rotation_x(radians: f32) -> [f32; 16] {
    let (sin, cos) = radians.sin_cos();
    [
        1.0, 0.0, 0.0, 0.0, 0.0, cos, sin, 0.0, 0.0, -sin, cos, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

/// Column-major rotation about Z.
fn rotation_z(radians: f32) -> [f32; 16] {
    let (sin, cos) = radians.sin_cos();
    [
        cos, sin, 0.0, 0.0, -sin, cos, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

/// The deck's model matrix: yaw to face travel, then the demo tilts, at its
/// position and hop height.
fn deck_model(pose: &DeckPose) -> [f32; 16] {
    let yaw = rotation_y(pose.yaw);
    let roll = rotation_z(pose.roll * 0.6);
    let pitch = rotation_x(pose.pitch);
    let tilted = multiply(&multiply(&yaw, &roll), &pitch);
    multiply(
        &translation(pose.position[0], pose.height, pose.position[1]),
        &tilted,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_times_identity_is_identity() {
        let identity = translation(0.0, 0.0, 0.0);
        assert_eq!(multiply(&identity, &identity), identity);
    }

    #[test]
    fn translation_moves_a_point() {
        let matrix = translation(1.0, 2.0, 3.0);
        // Column-major: the point (4, 5, 6, 1) moves to (5, 7, 9, 1).
        let point = [4.0_f32, 5.0, 6.0, 1.0];
        let mut moved = [0.0_f32; 4];
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += matrix[k * 4 + row] * point[k];
            }
            moved[row] = sum;
        }
        assert_eq!(moved, [5.0, 7.0, 9.0, 1.0]);
    }

    #[test]
    fn mesh_bytes_lay_out_positions_then_normals_then_colors() {
        let mesh = (
            vec![Vertex {
                position: [1.0, 2.0, 3.0],
                normal: [0.0, 1.0, 0.0],
                color: [0.5, 0.5, 0.5],
            }],
            vec![0_u32],
        );
        let (vertices, indices) = mesh_bytes(&mesh.0, &mesh.1);
        assert_eq!(vertices.len(), 36);
        assert_eq!(indices.len(), 4);
        assert_eq!(&vertices[0..4], &1.0_f32.to_ne_bytes());
    }

    #[test]
    fn present_problems_map_reconfigure_skip_and_fatal() {
        assert!(
            PresentProblem::of(wgpu::CurrentSurfaceTexture::Outdated)
                .is_err_and(|problem| problem == PresentProblem::Reconfigure)
        );
        assert!(
            PresentProblem::of(wgpu::CurrentSurfaceTexture::Lost)
                .is_err_and(|problem| problem == PresentProblem::Reconfigure)
        );
        assert!(
            PresentProblem::of(wgpu::CurrentSurfaceTexture::Timeout)
                .is_err_and(|problem| problem == PresentProblem::Skip)
        );
        assert!(
            PresentProblem::of(wgpu::CurrentSurfaceTexture::Validation)
                .is_err_and(|problem| problem == PresentProblem::Fatal)
        );
    }

    /// A point through a matrix, as the vertex shader does it.
    fn clip_of(matrix: &[f32; 16], point: [f32; 4]) -> [f32; 4] {
        let mut clip = [0.0_f32; 4];
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += matrix[k * 4 + row] * point[k];
            }
            clip[row] = sum;
        }
        clip
    }

    #[test]
    fn projection_maps_the_depth_range_zero_to_one_and_keeps_w_positive() {
        let projection = perspective(0.9, 16.0 / 9.0, 0.05, 120.0);
        // The view space looks down -z: straight ahead is a negative z.
        let near = clip_of(&projection, [0.0, 0.0, -0.05, 1.0]);
        assert!(near[3] > 0.0, "the near plane has a positive w");
        assert!(
            (near[2] / near[3]).abs() < 1.0e-5,
            "the near plane maps to 0"
        );
        let far = clip_of(&projection, [0.0, 0.0, -120.0, 1.0]);
        assert!(far[3] > 0.0, "the far plane has a positive w");
        assert!(
            (far[2] / far[3] - 1.0).abs() < 1.0e-4,
            "the far plane maps to 1"
        );
        // A point behind the camera must clip away: negative w.
        let behind = clip_of(&projection, [0.0, 0.0, 5.0, 1.0]);
        assert!(
            behind[3] < 0.0,
            "a point behind the camera has a negative w"
        );
    }

    #[test]
    fn the_default_camera_sees_the_plaza_centre() {
        let view_proj = multiply(
            &perspective(0.9, 16.0 / 9.0, 0.05, 120.0),
            &camera(DeckPose::default()),
        );
        let ndc = |p: [f32; 3]| {
            let clip = clip_of(&view_proj, [p[0], p[1], p[2], 1.0]);
            [
                clip[0] / clip[3],
                clip[1] / clip[3],
                clip[2] / clip[3],
                clip[3],
            ]
        };
        let centre = ndc([0.0, 0.0, 0.0]);
        assert!(
            centre[3] > 0.0,
            "the plaza centre is in front of the camera"
        );
        assert!(
            (-1.0..1.0).contains(&centre[0]) && (-1.0..1.0).contains(&centre[1]),
            "the plaza centre is inside the frame: {centre:?}"
        );
        assert!(
            (0.0..1.0).contains(&centre[2]),
            "the plaza centre is inside the depth range: {centre:?}"
        );
        let corner = ndc([10.0, 0.0, -10.0]);
        assert!(
            corner[3] > 0.0 && corner[2] < 1.0,
            "the far corner stays in"
        );
    }
}
