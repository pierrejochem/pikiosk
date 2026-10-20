VARY vec2 v_uv;
uniform sampler2D u_tex;

void main() {
    FRAG_COLOR = vec4(TEX(u_tex, v_uv).rgb, 1.0);
}
