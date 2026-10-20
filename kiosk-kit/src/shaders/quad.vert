// Full-viewport quad. a_pos is in clip space (-1..1).
ATTR vec2 a_pos;
VARY vec2 v_uv;

void main() {
    v_uv = a_pos * 0.5 + 0.5;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
