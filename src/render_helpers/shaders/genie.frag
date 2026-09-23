// Inverse-map a tapered sheet in an axis system whose +Y points toward the Dock.
// All four edges use the same deformation; the snapshot keeps its original orientation.
precision highp float;
varying vec2 niri_v_coords;
uniform vec2 niri_size;
uniform float niri_alpha;
uniform float niri_scale;
uniform sampler2D niri_tex;
uniform vec4 window_rect;
uniform vec4 target_rect;
uniform vec4 texture_rect;
uniform vec2 area_origin;
uniform float edge; // bottom, left, right, top
uniform float morph; // 0 = window, 1 = icon, reversed for restoration
#if defined(DEBUG_FLAGS)
uniform float niri_tint;
#endif

vec2 to_axis(vec2 p) {
    if (edge < 0.5) return p;
    if (edge < 1.5) return vec2(p.y, -p.x);
    if (edge < 2.5) return p.yx;
    return vec2(p.x, -p.y);
}

vec2 from_axis(vec2 p) {
    if (edge < 0.5) return p;
    if (edge < 1.5) return vec2(-p.y, p.x);
    if (edge < 2.5) return p.yx;
    return vec2(p.x, -p.y);
}

vec4 axis_rect(vec4 r) {
    vec2 a = to_axis(r.xy);
    vec2 b = to_axis(r.xy + r.zw);
    return vec4(min(a, b), abs(b - a));
}

void main() {
    vec4 w = axis_rect(window_rect);
    vec4 t = axis_rect(target_rect);
    vec2 point = to_axis(area_origin + niri_v_coords * niri_size);

    // The near edge leads; the far edge starts later. Different cross-sections
    // then narrow and bend toward the icon, instead of scaling one rigid rectangle.
    float lead = smoothstep(0.0, 0.55, morph);
    float lag = smoothstep(0.25, 1.0, morph);
    float far_edge = mix(w.y, t.y, lag);
    float near_edge = mix(w.y + w.w, t.y + t.w, lead);
    float row = (point.y - far_edge) / max(near_edge - far_edge, 0.001);
    float pull = mix(lag, lead, smoothstep(0.0, 1.0, row));
    float width = mix(w.z, t.z, pull);
    float center = mix(w.x + w.z * 0.5, t.x + t.z * 0.5, pull);
    float column = (point.x - center) / max(width, 0.001) + 0.5;

    // Let shadow/border padding outside the window geometry follow the sheet too.
    vec2 source = from_axis(w.xy + vec2(column, row) * w.zw);
    vec2 uv = (source - window_rect.xy) / window_rect.zw;
    uv = (uv - texture_rect.xy) / texture_rect.zw;
    vec2 extent = (edge < 0.5 || edge > 2.5)
        ? vec2(width, near_edge - far_edge) : vec2(near_edge - far_edge, width);
    vec2 coverage = clamp(min(uv, 1.0 - uv) * extent * texture_rect.zw * niri_scale + 0.5, 0.0, 1.0);
    vec4 color = texture2D(niri_tex, clamp(uv, 0.0, 1.0)) * (coverage.x * coverage.y * niri_alpha);
#if defined(DEBUG_FLAGS)
    if (niri_tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
