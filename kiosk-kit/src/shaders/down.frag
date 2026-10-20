// Dual-Kawase style downsample: 5 bilinear taps.
VARY vec2 v_uv;
uniform sampler2D u_tex;
uniform vec2 u_half;   // half a source texel, in uv

void main() {
    vec3 c = TEX(u_tex, v_uv).rgb * 4.0;
    c += TEX(u_tex, v_uv - u_half).rgb;
    c += TEX(u_tex, v_uv + u_half).rgb;
    c += TEX(u_tex, v_uv + vec2(u_half.x, -u_half.y)).rgb;
    c += TEX(u_tex, v_uv - vec2(u_half.x, -u_half.y)).rgb;
    FRAG_COLOR = vec4(c / 8.0, 1.0);
}
