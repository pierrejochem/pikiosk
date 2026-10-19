// Animated "aurora" wallpaper: soft colour fields drifting over a dark base.
VARY vec2 v_uv;
uniform float u_time;
uniform float u_aspect;
uniform vec3 u_base;
uniform vec3 u_c0;
uniform vec3 u_c1;
uniform vec3 u_c2;
uniform vec3 u_c3;

float blob(vec2 p, vec2 c, float r) {
    vec2 d = p - c;
    return exp(-dot(d, d) / (r * r));
}

float hash(vec2 p) {
    return fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453);
}

void main() {
    vec2 p = vec2(v_uv.x * u_aspect, v_uv.y);
    float t = u_time * 0.06;
    float a = u_aspect;

    vec3 col = u_base;
    col = mix(col, u_c0, 0.95 * blob(p, vec2(a * (0.22 + 0.10 * sin(t * 1.30)), 0.78 + 0.10 * cos(t * 1.10)), 0.42));
    col = mix(col, u_c1, 0.85 * blob(p, vec2(a * (0.78 + 0.12 * cos(t * 0.90)), 0.70 + 0.14 * sin(t * 1.70)), 0.38));
    col = mix(col, u_c2, 0.80 * blob(p, vec2(a * (0.55 + 0.18 * sin(t * 0.70 + 2.0)), 0.22 + 0.10 * cos(t * 1.30)), 0.40));
    col = mix(col, u_c3, 0.70 * blob(p, vec2(a * (0.10 + 0.08 * cos(t * 1.10 + 1.0)), 0.18 + 0.12 * sin(t * 0.80)), 0.30));

    // two thin luminous ribbons: fine detail for the glass to bend
    float y1 = 0.42 + 0.10 * sin(p.x * 2.3 + t * 2.0) + 0.04 * sin(p.x * 5.1 - t);
    float y2 = 0.36 + 0.12 * sin(p.x * 1.7 - t * 1.6 + 1.3);
    float r1 = abs(v_uv.y - y1);
    float r2 = abs(v_uv.y - y2);
    col += vec3(1.0, 0.95, 1.0) * (0.55 * exp(-r1 * r1 * 90000.0) + 0.10 * exp(-r1 * r1 * 900.0));
    col += mix(u_c1, vec3(1.0), 0.5) * (0.40 * exp(-r2 * r2 * 120000.0) + 0.08 * exp(-r2 * r2 * 1200.0));

    // soft diagonal light band
    col += 0.035 * smoothstep(0.35, 0.0, abs(v_uv.x - v_uv.y * 0.8 - 0.25 - 0.15 * sin(t)));

    // vignette
    vec2 v = v_uv - 0.5;
    col *= 1.0 - 0.55 * dot(v, v);

    // dither against banding
    col += (hash(gl_FragCoord.xy + fract(u_time)) - 0.5) / 255.0;
    FRAG_COLOR = vec4(col, 1.0);
}
