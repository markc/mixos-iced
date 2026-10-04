precision mediump float;

varying vec2 v_tex_coords;
uniform sampler2D u_texture;
uniform float u_zoom;
uniform float u_time;

void main() {
    vec2 coords = v_tex_coords;

    // Example displacement: Create a wave effect that scales with your canvas zoom
    coords.x += sin(coords.y * 20.0 + u_time * 5.0) * (0.01 / u_zoom);

    vec4 color = texture2D(u_texture, coords);

    // Example color pass: B&W inversion based on time
    float gray = dot(color.rgb, vec3(0.2126, 0.7152, 0.0722));
    vec3 final_color = mix(color.rgb, vec3(gray), sin(u_time) * 0.5 + 0.5);

    gl_FragColor = vec4(final_color, color.a);
}