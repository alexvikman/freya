// The demo, drawn by the graphics card (see freya/src/gui/demo.rs): an Amiga-style screen of
// 256 rows, with copper and raster bars and the look of a CRT.
#include <metal_stdlib>
using namespace metal;

// What the CPU hands over each frame. Laid out like `Uniforms` in metal.rs.
struct Uniforms {
    float time;    // seconds since the demo started
    float pulse;   // the bass, 0 to 1
    float2 size;   // the drawable, in pixels
};

struct Varyings {
    float4 position [[position]];
    float2 uv;     // 0 to 1, from the top left
};

// The virtual screen's height, in its own pixels: a PAL Amiga's. Its width follows the box,
// so the pixels stay square.
constant float ROWS = 256.0;

// One triangle that covers the whole drawable: no vertex buffer needed.
vertex Varyings demo_vertex(uint id [[vertex_id]]) {
    float2 corner = float2((id << 1) & 2, id & 2);
    Varyings out;
    out.position = float4(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = float2(corner.x, 1.0 - corner.y);
    return out;
}

// The Amiga's 12-bit colours: 16 levels a channel.
float3 amiga(float3 c) {
    return floor(clamp(c, 0.0, 1.0) * 15.0 + 0.5) / 15.0;
}

// The background, a colour per row as the copper set it: deep blue at the top, through
// purple, to near black at the bottom; the bass lifts it a little.
float3 copper(float row, float pulse) {
    float y = row / ROWS;
    float3 top = float3(0.05, 0.06, 0.22);
    float3 middle = float3(0.18, 0.05, 0.22);
    float3 bottom = float3(0.02, 0.01, 0.04);
    float3 c = y < 0.55 ? mix(top, middle, y / 0.55) : mix(middle, bottom, (y - 0.55) / 0.45);
    return c * (1.0 + 0.6 * pulse);
}

// Raster bars: bars of colour turning around a cylinder, each a gradient bright in the
// middle. `front` picks the half facing the viewer, drawn after the back half.
float3 raster_bars(float3 c, float row, float t, float pulse, bool front) {
    const int COUNT = 6;
    float half_height = 7.0 + 5.0 * pulse;
    for (int i = 0; i < COUNT; i++) {
        float angle = t * 1.15 + float(i) * (2.0 * M_PI_F / float(COUNT));
        if ((cos(angle) > 0.0) != front) {
            continue;
        }
        float centre = ROWS * 0.5 + ROWS * 0.3 * sin(angle);
        float d = abs(row - centre) / half_height;
        if (d >= 1.0) {
            continue;
        }
        // Each bar in a colour of its own, around the colour wheel.
        float3 hue = 0.55 + 0.45 * cos(float3(0.0, 2.1, 4.2) + float(i) * 1.05);
        // The back half is dimmer, as if further away.
        float depth = front ? 1.0 : 0.55;
        float shade = 1.0 - d * d;
        c = hue * shade * depth * (0.85 + 0.5 * pulse) + 0.15 * shade * shade;
    }
    return c;
}

fragment float4 demo_fragment(Varyings in [[stage_in]], constant Uniforms &u [[buffer(0)]]) {
    // Where on the virtual screen this pixel is: which of its pixels, and where in it.
    float2 screen = float2(round(ROWS * u.size.x / u.size.y), ROWS);
    float2 p = in.uv * screen;
    float2 cell = floor(p);
    float2 within = p - cell;
    float row = cell.y;
    float t = u.time;

    float3 c = copper(row, u.pulse);
    c = raster_bars(c, row, t, u.pulse, false);
    c = raster_bars(c, row, t, u.pulse, true);
    c = amiga(c);

    // The CRT: a dark gap between the rows, a soft glow, and darker corners.
    float scanline = 0.62 + 0.38 * smoothstep(0.0, 0.3, within.y) * smoothstep(1.0, 0.7, within.y);
    c = c * scanline + c * c * 0.25;
    float2 centred = in.uv - 0.5;
    c *= 1.0 - dot(centred, centred) * 0.7;
    return float4(c, 1.0);
}
