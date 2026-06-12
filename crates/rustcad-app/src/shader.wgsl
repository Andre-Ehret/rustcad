struct Uniforms {
    view_proj: mat4x4<f32>,
    // xyz = Lichtrichtung (Headlight, zeigt von der Kamera weg)
    light_dir: vec4<f32>,
    // x = selektierte Pick-ID + 1 (0 = keine Selektion)
    selected: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

// --- Flächen: flat shading + Lambert; pick_id = (body << 16) | face ---

struct MeshOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) @interpolate(flat) pick_id: u32,
};

@vertex
fn vs_mesh(
    @location(0) pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) pick_id: u32,
) -> MeshOut {
    var out: MeshOut;
    out.clip_pos = u.view_proj * vec4<f32>(pos, 1.0);
    out.normal = normal;
    out.pick_id = pick_id;
    return out;
}

@fragment
fn fs_mesh(in: MeshOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    // abs(): beidseitig beleuchten, damit verdrehte Normalen nicht schwarz werden
    let lambert = abs(dot(n, -normalize(u.light_dir.xyz)));
    var base = vec3<f32>(0.62, 0.67, 0.74);
    if in.pick_id + 1u == u.selected.x {
        base = vec3<f32>(0.95, 0.62, 0.25); // selektierte Fläche
    }
    let color = base * (0.25 + 0.75 * lambert);
    return vec4<f32>(color, 1.0);
}

// --- ID-Buffer-Picking: Pick-ID + 1 in eine R32Uint-Textur schreiben ---

@fragment
fn fs_pick(in: MeshOut) -> @location(0) u32 {
    return in.pick_id + 1u;
}

// --- Linien: Grid, Achsen, B-Rep-Kanten ---

struct LineOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_line(@location(0) pos: vec3<f32>, @location(1) color: vec3<f32>) -> LineOut {
    var out: LineOut;
    out.clip_pos = u.view_proj * vec4<f32>(pos, 1.0);
    // Leichter Depth-Bias Richtung Kamera, damit Kanten auf Flächen nicht
    // im Depth-Buffer verschwinden (Pipeline-Bias ist für Linien verboten)
    out.clip_pos.z -= 2e-4 * out.clip_pos.w;
    out.color = color;
    return out;
}

@fragment
fn fs_line(in: LineOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
