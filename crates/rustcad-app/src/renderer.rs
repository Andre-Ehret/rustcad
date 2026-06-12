use egui_wgpu::wgpu::{self, util::DeviceExt};
use rustcad_geom::TriMesh;

/// Muss zu `NativeOptions::depth_buffer = 32` passen
/// (eframe legt dann einen `Depth32Float`-Buffer an).
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const PICK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;

const GRID_HALF_EXTENT: i32 = 10;
const GRID_COLOR: [f32; 3] = [0.23, 0.25, 0.28];
const EDGE_COLOR: [f32; 3] = [0.08, 0.09, 0.10];

/// Im Viewport pickbare Fläche: `(Body-Index, B-Rep-Face-Index)`.
pub type PickId = (u16, u16);

pub fn encode_pick(body: u16, face: u16) -> u32 {
    ((body as u32) << 16) | face as u32
}

fn decode_pick(id: u32) -> PickId {
    ((id >> 16) as u16, (id & 0xffff) as u16)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub view_proj: [[f32; 4]; 4],
    pub light_dir: [f32; 4],
    /// x = selektierte Pick-ID + 1 (0 = keine Selektion)
    pub selected: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MeshVertex {
    pos: [f32; 3],
    normal: [f32; 3],
    pick_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    pos: [f32; 3],
    color: [f32; 3],
}

/// GPU-Ressourcen der 3D-Szene. Lebt in den `callback_resources` des
/// egui-wgpu-Renderers; der Paint-Callback greift pro Frame darauf zu.
pub struct SceneRenderer {
    mesh_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    pick_pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    pick_uniform_buffer: wgpu::Buffer,
    pick_bind_group: wgpu::BindGroup,
    mesh_vertices: Option<wgpu::Buffer>,
    mesh_indices: Option<wgpu::Buffer>,
    mesh_index_count: u32,
    line_vertices: wgpu::Buffer,
    line_vertex_count: u32,
}

impl SceneRenderer {
    pub fn new(render_state: &egui_wgpu::RenderState) -> Self {
        let device = &render_state.device;
        let target_format = render_state.target_format;

        let shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));

        let uniform_size = std::mem::size_of::<Uniforms>() as u64;
        let make_uniform = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: uniform_size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let uniform_buffer = make_uniform("scene uniforms");
        let pick_uniform_buffer = make_uniform("pick uniforms");

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene bind group layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let make_bind_group = |label, buffer: &wgpu::Buffer| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            })
        };
        let bind_group = make_bind_group("scene bind group", &uniform_buffer);
        let pick_bind_group = make_bind_group("pick bind group", &pick_uniform_buffer);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let mesh_vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<MeshVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Uint32],
        };

        let depth_stencil = Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });

        let mesh_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mesh pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_mesh"),
                compilation_options: Default::default(),
                buffers: std::slice::from_ref(&mesh_vertex_layout),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mesh"),
                compilation_options: Default::default(),
                targets: &[Some(target_format.into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                // Kein Culling: B-Rep-Orientierung garantiert das MVP noch nicht
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: depth_stencil.clone(),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let pick_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pick pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_mesh"),
                compilation_options: Default::default(),
                buffers: std::slice::from_ref(&mesh_vertex_layout),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_pick"),
                compilation_options: Default::default(),
                targets: &[Some(PICK_FORMAT.into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: depth_stencil.clone(),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("line pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_line"),
                compilation_options: Default::default(),
                targets: &[Some(target_format.into())],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::LineList,
                ..Default::default()
            },
            // Depth-Bias für Linien übernimmt der Vertex-Shader (vs_line):
            // wgpu erlaubt DepthBiasState nur bei Dreieck-Topologien
            depth_stencil,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let line_vertex_data = build_line_vertices(&[]);
        let line_vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("line vertices"),
            contents: bytemuck::cast_slice(&line_vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        });

        Self {
            mesh_pipeline,
            line_pipeline,
            pick_pipeline,
            uniform_buffer,
            bind_group,
            pick_uniform_buffer,
            pick_bind_group,
            mesh_vertices: None,
            mesh_indices: None,
            mesh_index_count: 0,
            line_vertices,
            line_vertex_count: line_vertex_data.len() as u32,
        }
    }

    /// Lädt die tessellierten Bodies auf die GPU. Vertex-Pick-IDs
    /// kodieren `(Body-Index, Face-Index)`.
    pub fn set_bodies(&mut self, device: &wgpu::Device, meshes: &[&TriMesh]) {
        let mut vertices: Vec<MeshVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for (body, mesh) in meshes.iter().enumerate() {
            let base = vertices.len() as u32;
            vertices.extend(
                mesh.positions
                    .iter()
                    .enumerate()
                    .map(|(i, &pos)| MeshVertex {
                        pos,
                        normal: mesh.normals[i],
                        pick_id: encode_pick(body as u16, mesh.face_ids[i] as u16),
                    }),
            );
            indices.extend(mesh.indices.iter().map(|&i| base + i));
        }

        self.mesh_index_count = indices.len() as u32;
        self.mesh_vertices = (!vertices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh vertices"),
                contents: bytemuck::cast_slice(&vertices),
                usage: wgpu::BufferUsages::VERTEX,
            })
        });
        self.mesh_indices = (!indices.is_empty()).then(|| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh indices"),
                contents: bytemuck::cast_slice(&indices),
                usage: wgpu::BufferUsages::INDEX,
            })
        });

        let line_vertex_data = build_line_vertices(meshes);
        self.line_vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("line vertices"),
            contents: bytemuck::cast_slice(&line_vertex_data),
            usage: wgpu::BufferUsages::VERTEX,
        });
        self.line_vertex_count = line_vertex_data.len() as u32;
    }

    /// ID-Buffer-Picking (TECH_SPEC §7.2): rendert die Pick-IDs in eine
    /// R32Uint-Textur und liest das Pixel unter dem Cursor zurück.
    /// Blockiert kurz auf die GPU — nur bei Klicks aufrufen.
    pub fn pick(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view_proj: [[f32; 4]; 4],
        size: [u32; 2],
        pixel: [u32; 2],
    ) -> Option<PickId> {
        let (Some(mesh_vertices), Some(mesh_indices)) =
            (self.mesh_vertices.as_ref(), self.mesh_indices.as_ref())
        else {
            return None;
        };
        if size[0] == 0 || size[1] == 0 || pixel[0] >= size[0] || pixel[1] >= size[1] {
            return None;
        }

        queue.write_buffer(
            &self.pick_uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniforms {
                view_proj,
                light_dir: [0.0, 0.0, -1.0, 0.0],
                selected: [0; 4],
            }),
        );

        let extent = wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        };
        let id_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pick id texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PICK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pick depth texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pick readback"),
            size: 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let id_view = id_texture.create_view(&Default::default());
        let depth_view = depth_texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pick pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &id_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pick_pipeline);
            pass.set_bind_group(0, &self.pick_bind_group, &[]);
            pass.set_vertex_buffer(0, mesh_vertices.slice(..));
            pass.set_index_buffer(mesh_indices.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..self.mesh_index_count, 0, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &id_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: pixel[0],
                    y: pixel[1],
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: None,
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let value = {
            let data = slice.get_mapped_range();
            u32::from_ne_bytes(data[0..4].try_into().ok()?)
        };
        readback.unmap();

        (value != 0).then(|| decode_pick(value - 1))
    }
}

/// Liniensegmente für Grid (XY-Ebene), Achsenkreuz und B-Rep-Kanten.
fn build_line_vertices(meshes: &[&TriMesh]) -> Vec<LineVertex> {
    let mut v = Vec::new();
    let half = GRID_HALF_EXTENT as f32;
    let mut segment = |a: [f32; 3], b: [f32; 3], color: [f32; 3]| {
        v.push(LineVertex { pos: a, color });
        v.push(LineVertex { pos: b, color });
    };

    // Grid; die Mittellinien übernimmt das Achsenkreuz
    for i in -GRID_HALF_EXTENT..=GRID_HALF_EXTENT {
        if i == 0 {
            continue;
        }
        let t = i as f32;
        segment([t, -half, 0.0], [t, half, 0.0], GRID_COLOR);
        segment([-half, t, 0.0], [half, t, 0.0], GRID_COLOR);
    }

    // Achsenkreuz: +X rot, +Y grün, +Z blau; negative Hälften gedimmt
    segment([0.0; 3], [half, 0.0, 0.0], [0.84, 0.22, 0.22]);
    segment([-half, 0.0, 0.0], [0.0; 3], [0.42, 0.16, 0.16]);
    segment([0.0; 3], [0.0, half, 0.0], [0.22, 0.72, 0.25]);
    segment([0.0, -half, 0.0], [0.0; 3], [0.15, 0.36, 0.17]);
    segment([0.0; 3], [0.0, 0.0, half * 0.5], [0.25, 0.45, 0.95]);

    // B-Rep-Kanten aller Bodies
    for mesh in meshes {
        for polyline in &mesh.edges {
            for pair in polyline.windows(2) {
                segment(pair[0], pair[1], EDGE_COLOR);
            }
        }
    }

    v
}

/// Paint-Callback: schreibt pro Frame die Uniforms und zeichnet die Szene
/// in den egui-Renderpass.
pub struct SceneCallback {
    pub uniforms: Uniforms,
}

impl egui_wgpu::CallbackTrait for SceneCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(renderer) = callback_resources.get::<SceneRenderer>() {
            queue.write_buffer(
                &renderer.uniform_buffer,
                0,
                bytemuck::bytes_of(&self.uniforms),
            );
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(renderer) = callback_resources.get::<SceneRenderer>() else {
            return;
        };

        render_pass.set_bind_group(0, &renderer.bind_group, &[]);

        if let (Some(vertices), Some(indices)) = (
            renderer.mesh_vertices.as_ref(),
            renderer.mesh_indices.as_ref(),
        ) {
            render_pass.set_pipeline(&renderer.mesh_pipeline);
            render_pass.set_vertex_buffer(0, vertices.slice(..));
            render_pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            render_pass.draw_indexed(0..renderer.mesh_index_count, 0, 0..1);
        }

        render_pass.set_pipeline(&renderer.line_pipeline);
        render_pass.set_vertex_buffer(0, renderer.line_vertices.slice(..));
        render_pass.draw(0..renderer.line_vertex_count, 0..1);
    }
}
