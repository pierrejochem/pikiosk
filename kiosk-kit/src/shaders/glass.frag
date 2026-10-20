// Liquid glass: a rounded slab that refracts and frosts the wallpaper behind
// it, with a lit rim, soft inner glow and a drop shadow.
// All geometry is in framebuffer pixels (origin bottom-left).
VARY vec2 v_uv;
uniform sampler2D u_sharp;   // wallpaper, half resolution
uniform sampler2D u_light;   // lightly blurred, 1/4 resolution
uniform sampler2D u_heavy;   // heavily blurred, 1/8 resolution
uniform vec2 u_screen;
uniform vec4 u_rect;         // centre.xy, half-size.xy
uniform float u_radius;
uniform vec4 u_tint;         // rgb, amount
uniform vec4 u_optics;       // frost 0..1, refraction px, bevel px, highlight
uniform vec4 u_shadow;       // alpha, blur px, y offset px, opacity

float sd_round_box(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

vec2 round_box_normal(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    vec2 qp = max(q, 0.0);
    float l = length(qp);
    vec2 side = q.x > q.y ? vec2(1.0, 0.0) : vec2(0.0, 1.0);
    vec2 n = l > 0.0 ? qp / l : side;
    return n * sign(p);
}

vec3 backdrop(vec2 px, float frost) {
    vec2 uv = px / u_screen;
    vec3 s = TEX(u_sharp, uv).rgb;
    vec3 l = TEX(u_light, uv).rgb;
    vec3 h = TEX(u_heavy, uv).rgb;
    return mix(mix(s, l, clamp(frost * 2.0, 0.0, 1.0)), h, clamp(frost * 2.0 - 1.0, 0.0, 1.0));
}

void main() {
    vec2 frag = gl_FragCoord.xy;
    vec2 p = frag - u_rect.xy;
    vec2 b = u_rect.zw;
    float r = min(u_radius, min(b.x, b.y));
    float d = sd_round_box(p, b, r);
    float cover = clamp(0.5 - d, 0.0, 1.0) * u_shadow.w;

    // drop shadow (y up, so a downward offset is +y in p)
    float ds = sd_round_box(p + vec2(0.0, u_shadow.z), b, r);
    float shadow = u_shadow.x * u_shadow.w * (1.0 - smoothstep(-u_shadow.y * 0.3, u_shadow.y, ds));

    // refraction: displacement grows along a circular bevel profile
    vec2 n = round_box_normal(p, b, r);
    float bevel = max(u_optics.z, 1.0);
    float t = clamp(1.0 + d / bevel, 0.0, 1.0);
    float bend = 1.0 - sqrt(max(1.0 - t * t, 0.0));
    vec2 off = n * bend * u_optics.y;

    vec3 col = backdrop(frag + off, u_optics.x);
    // chromatic dispersion at the rim
    col.r = mix(col.r, TEX(u_light, (frag + off * 1.25) / u_screen).r, 0.6 * bend);
    col.b = mix(col.b, TEX(u_light, (frag + off * 0.75) / u_screen).b, 0.6 * bend);

    // tint, lift and top sheen
    col = mix(col, u_tint.rgb, u_tint.a);
    col += 0.03 + 0.035 * clamp(p.y / b.y, -1.0, 1.0);

    // lit rim (light from the top left, weaker bounce bottom right) and inner glow
    vec2 L = vec2(-0.5446, 0.8387);
    float lt = max(dot(n, L), 0.0);
    float lb = max(dot(n, -L), 0.0);
    float rim = 1.0 - smoothstep(0.0, 2.0, -d);
    float spec = rim * (0.70 * lt * lt + 0.35 * lb * lb + 0.12);
    float glow = t * t * (0.06 + 0.10 * lt);
    col = clamp(col + (spec + glow) * u_optics.w, 0.0, 1.0);

    FRAG_COLOR = vec4(col * cover, cover) + vec4(0.0, 0.0, 0.0, shadow * (1.0 - cover));
}
