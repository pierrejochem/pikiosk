// Separable 9-tap gaussian using 5 linear-filtered fetches.
VARY vec2 v_uv;
uniform sampler2D u_tex;
uniform vec2 u_dir;    // one texel along the blur axis, in uv

void main() {
    vec3 c = TEX(u_tex, v_uv).rgb * 0.2270270270;
    c += TEX(u_tex, v_uv + u_dir * 1.3846153846).rgb * 0.3162162162;
    c += TEX(u_tex, v_uv - u_dir * 1.3846153846).rgb * 0.3162162162;
    c += TEX(u_tex, v_uv + u_dir * 3.2307692308).rgb * 0.0702702703;
    c += TEX(u_tex, v_uv - u_dir * 3.2307692308).rgb * 0.0702702703;
    FRAG_COLOR = vec4(c, 1.0);
}
