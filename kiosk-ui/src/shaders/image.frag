// Photo wallpaper, scaled to cover the screen.
VARY vec2 v_uv;
uniform sampler2D u_tex;
uniform vec2 u_scale;

void main() {
    vec2 uv = (v_uv - 0.5) * u_scale + 0.5;
    uv.y = 1.0 - uv.y;                       // image rows are stored top-down
    vec3 col = TEX(u_tex, uv).rgb * 0.88;    // a touch darker so white text reads
    FRAG_COLOR = vec4(col, 1.0);
}
