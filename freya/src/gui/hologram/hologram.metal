// The hologram, drawn by the graphics card (see freya/src/gui/hologram.rs): a woman projected in
// translucent light over a city at night in the rain, after Blade Runner 2049.
//
// Two passes a frame: `city_fragment` draws the city at a quarter of the resolution, as it is
// blurred by fog anyway and costs most; `hologram_fragment` draws the full picture from it,
// with the rain and the hologram sharp.
#include <metal_stdlib>
using namespace metal;

// What the CPU hands over each frame. Laid out like `Uniforms` in metal.rs.
struct Uniforms {
    float time;        // seconds since the hologram started
    float pulse;       // the bass, 0 to 1
    float2 size;       // the drawable, in pixels
    float transition;  // the glitch from the current pose to the next, 0 to 1
    float aspect;      // the poses' width over their height
    float seed;        // changes with each pose
    float has_poses;   // 1 when there are poses to project
};

struct Varyings {
    float4 position [[position]];
    float2 uv;         // 0 to 1, from the top left
};

// One triangle that covers the whole drawable: no vertex buffer needed.
vertex Varyings hologram_vertex(uint id [[vertex_id]]) {
    float2 corner = float2((id << 1) & 2, id & 2);
    Varyings out;
    out.position = float4(corner * 2.0 - 1.0, 0.0, 1.0);
    out.uv = float2(corner.x, 1.0 - corner.y);
    return out;
}

// ---- Noise ----------------------------------------------------------------------------------

float hash(float2 p) {
    p = fract(p * float2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

float noise(float2 p) {
    float2 i = floor(p);
    float2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + float2(1, 0)), f.x),
               mix(hash(i + float2(0, 1)), hash(i + float2(1, 1)), f.x), f.y);
}

// ---- The city -------------------------------------------------------------------------------

constant float HORIZON = 0.62;  // where the street begins, from the top

// A neon colour from the film's palette: magenta, cyan, amber or violet.
float3 neon(float h) {
    if (h < 0.3) return float3(1.0, 0.22, 0.62);
    if (h < 0.55) return float3(0.15, 0.85, 1.0);
    if (h < 0.8) return float3(1.0, 0.55, 0.18);
    return float3(0.62, 0.3, 1.0);
}

// Blurred neon signs and lit windows hanging in the fog, at `p` (x across, y down, in the
// box's height). `far` blurs and dims them.
float3 lights(float2 p, float t, float far) {
    float3 c = float3(0.0);
    for (int i = 0; i < 14; i++) {
        float fi = float(i);
        float2 at = float2(hash(float2(fi, 1.7)) * 2.4 - 0.2, 0.08 + hash(float2(fi, 9.1)) * 0.5);
        float2 extent = float2(0.02 + 0.06 * hash(float2(fi, 3.3)), 0.012 + 0.05 * hash(float2(fi, 5.9)));
        float2 d = abs(p - at) / (extent + far * 0.05);
        float glow = exp(-dot(d, d) * 1.6);
        // Some flicker, as old neon does.
        float flicker = hash(float2(fi, floor(t * 7.0 + fi))) > 0.93 ? 0.35 : 1.0;
        c += neon(hash(float2(fi, 7.7))) * glow * flicker * (1.0 - far * 0.55);
    }
    return c;
}

// Buildings: dark blocks with sparse lit windows, three rows deep, fading into the fog.
float3 buildings(float2 p, float3 c, float3 fog) {
    for (int layer = 2; layer >= 0; layer--) {
        float depth = float(layer) / 2.0;          // 1 far, 0 near
        float width = mix(0.11, 0.05, depth);
        float column = floor((p.x + float(layer) * 0.37) / width);
        float top = mix(0.05, 0.3, depth) + hash(float2(column, float(layer))) * mix(0.25, 0.18, depth);
        if (p.y > top) {
            float3 wall = mix(float3(0.015, 0.02, 0.035), fog, 0.35 + 0.5 * depth);
            // Windows: a fine grid, a few of them lit, each a soft point of light.
            float2 grid = float2((p.x + float(layer) * 0.37) / (width / 9.0), p.y / 0.011);
            float2 cell = floor(grid);
            float lit = step(0.93, hash(cell + float(layer) * 11.0));
            float point = smoothstep(0.5, 0.05, length(fract(grid) - 0.5));
            float3 window = mix(float3(1.0, 0.7, 0.35), float3(0.4, 0.8, 1.0), hash(cell * 1.3));
            c = wall + window * lit * point * mix(0.35, 0.1, depth);
        }
    }
    return c;
}

// Rain: thin slanting streaks in two layers, the nearer one faster and brighter.
float rain(float2 p, float t) {
    float r = 0.0;
    for (int layer = 0; layer < 2; layer++) {
        float scale = layer == 0 ? 90.0 : 160.0;
        float speed = layer == 0 ? 2.6 : 1.7;
        float2 q = float2(p.x * scale + p.y * 9.0, p.y * scale * 0.12 - t * speed * 9.0);
        float2 cell = floor(q);
        float h = hash(cell);
        float streak = smoothstep(0.08, 0.0, abs(fract(q.x) - 0.5)) * smoothstep(0.0, 0.5, fract(q.y)) * (1.0 - fract(q.y));
        r += streak * step(0.75, h) * (layer == 0 ? 0.32 : 0.16);
    }
    return r;
}

// The fog's colour at height `y`: a magenta and amber haze low down.
float3 fog_at(float y) {
    return mix(float3(0.45, 0.12, 0.32), float3(0.55, 0.28, 0.12), smoothstep(0.2, 0.62, y));
}

// The city without the rain, which is drawn sharp over it.
float3 city(float2 p, float t, float pulse) {
    // The sky: dark teal overhead, into the haze.
    float3 fog = fog_at(p.y);
    float3 c = mix(float3(0.01, 0.04, 0.06), fog * 0.55, smoothstep(0.0, HORIZON, p.y));
    c += lights(p, t, 1.0) * 0.35;
    c = buildings(p, c, fog * 0.6);
    c += lights(p * float2(1.0, 1.1) + float2(0.3, 0.02), t, 0.4) * 0.45;
    // The street: wet, mirroring the city above it, with ripples, under the same fog. It
    // takes over from the city gradually, so there is no edge where they meet.
    float below = max(p.y - HORIZON, 0.0);
    float2 mirrored = float2(p.x + 0.004 * sin(p.y * 220.0 + t * 3.0), HORIZON - below * 1.6);
    float3 street = float3(0.012, 0.014, 0.024) + fog * 0.18 * exp(-below * 6.0)
        + lights(mirrored, t, 0.6) * 0.42 * exp(-below * 2.2);
    c = mix(c, street, smoothstep(HORIZON - 0.03, HORIZON + 0.05, p.y));
    // The fog breathes a little with the bass.
    c += fog * 0.05 * (1.0 + pulse);
    return c;
}

// ---- The hologram ---------------------------------------------------------------------------

constexpr sampler poses(filter::linear, mip_filter::linear, address::clamp_to_zero);

// Her light: brightness mapped onto the film's hologram colours, deep violet through magenta
// to pale cyan, with some of her own colours kept.
float3 projected(float3 colour) {
    float luma = dot(colour, float3(0.3, 0.59, 0.11));
    float3 tint = luma < 0.5
        ? mix(float3(0.25, 0.05, 0.55), float3(1.0, 0.35, 0.75), luma * 2.0)
        : mix(float3(1.0, 0.35, 0.75), float3(0.75, 0.95, 1.0), luma * 2.0 - 1.0);
    return mix(tint * (0.45 + luma), colour * 1.15, 0.6);
}

// The pose in `pose`, at `uv`, as projected light with its colour fringes.
float4 light_of(texture2d<float> pose, float2 uv, float fringe) {
    float r = pose.sample(poses, uv + float2(fringe, 0.0)).r;
    float4 g = pose.sample(poses, uv);
    float b = pose.sample(poses, uv - float2(fringe, 0.0)).b;
    float3 colour = float3(r, g.g, b);
    // Only black is no light at all and see-through; dark clothes still glow faintly, so all
    // of her is projected, not only her face and skin.
    float brightest = max(colour.r, max(colour.g, colour.b));
    float alpha = smoothstep(0.01, 0.09, brightest);
    // Lifted, most where dark: her clothes glow, and her skin stays bright but keeps its
    // detail.
    float luma = dot(colour, float3(0.3, 0.59, 0.11));
    float3 lifted = mix(pow(colour, float3(0.55)), colour, 0.5 * smoothstep(0.08, 0.45, luma));
    return float4(projected(lifted), alpha);
}

// The first pass: the city, into a texture a quarter of the drawable's size.
fragment float4 city_fragment(Varyings in [[stage_in]], constant Uniforms &u [[buffer(0)]]) {
    float aspect = u.size.x / u.size.y;
    return float4(city(float2(in.uv.x * aspect, in.uv.y), u.time, u.pulse), 1.0);
}

constexpr sampler smooth(filter::linear, address::clamp_to_edge);

fragment float4 hologram_fragment(Varyings in [[stage_in]],
                                  constant Uniforms &u [[buffer(0)]],
                                  texture2d<float> current [[texture(0)]],
                                  texture2d<float> next [[texture(1)]],
                                  texture2d<float> backdrop [[texture(2)]]) {
    float t = u.time;
    float aspect = u.size.x / u.size.y;
    // Points in the box's height: x from 0 to `aspect`, y from 0 to 1.
    float2 p = float2(in.uv.x * aspect, in.uv.y);
    float3 c = backdrop.sample(smooth, in.uv).rgb;
    c += rain(p, t) * mix(float3(0.6, 0.75, 0.9), fog_at(p.y), 0.3);

    // Where she stands: her pose's height a little short of the box's, from the street.
    float height = 0.94;
    float width = height * u.aspect;
    float2 origin = float2(aspect * 0.5 - width * 0.5, 0.05);
    float2 uv = (p - origin) / float2(width, height);
    // Only near her is there anything to project: her glow reaches a little beyond her.
    bool near = all(uv > float2(-0.12)) && all(uv < float2(1.12));

    if (u.has_poses > 0.5 && near) {
        // Breathing and a slow sway, more at her head than at her waist.
        uv.x += 0.006 * sin(t * 0.8) * (1.0 - uv.y);
        uv.y += 0.004 * sin(t * 1.6) * (1.0 - uv.y);

        // Glitches: bands of the picture slip sideways, more often with the bass and through
        // a change of pose.
        float tick = floor(t * 14.0);
        float band = floor(uv.y * 46.0);
        float chance = 0.04 + 0.22 * u.pulse + 0.7 * sin(u.transition * M_PI_F);
        float slip = 0.0;
        if (hash(float2(band, tick + u.seed * 13.0)) < chance * 0.35) {
            slip = (hash(float2(band * 1.7, tick)) - 0.5) * (0.04 + 0.1 * u.pulse);
        }
        float2 slipped = uv + float2(slip, 0.0);
        float fringe = 0.0025 + 0.006 * u.pulse + abs(slip) * 0.4;

        // The change of pose: block by block, in noise, with a flash halfway. Each pixel reads
        // only the pose it shows.
        float blocks = hash(floor(uv * float2(18.0, 30.0)) + u.seed);
        float4 she = blocks < u.transition
            ? light_of(next, slipped, fringe)
            : light_of(current, slipped, fringe);

        // Scanlines drifting up, and flicker.
        float scan = 0.72 + 0.28 * sin(in.uv.y * u.size.y * 0.55 + t * 18.0);
        float flicker = 0.88 + 0.12 * noise(float2(t * 23.0, 0.0));
        if (hash(float2(tick, 3.1)) > 0.97) {
            flicker *= 0.6;
        }
        float flash = 1.0 + 0.5 * exp(-pow((u.transition - 0.5) * 7.0, 2.0)) * step(0.01, u.transition);
        float strength = 0.78 * scan * flicker * flash * (0.9 + 0.35 * u.pulse);

        // Her glow, from a blurred copy of the pose.
        float4 haze = current.sample(poses, uv, level(6.0));
        float glow = smoothstep(0.02, 0.3, max(haze.r, max(haze.g, haze.b)));
        float3 glow_colour = float3(1.0, 0.4, 0.8) * glow * (0.2 + 0.3 * u.pulse);

        // The projector's beam, rising faintly from the street to her.
        float beam = exp(-pow((p.x - aspect * 0.5) / (0.08 + 0.25 * (1.0 - p.y)), 2.0))
            * smoothstep(0.35, 1.0, p.y) * 0.08;

        c += (she.rgb * she.a * strength) + glow_colour + float3(0.6, 0.4, 1.0) * beam;
    }

    // Darker corners, as through a lens.
    float2 centred = in.uv - 0.5;
    c *= 1.0 - dot(centred, centred) * 0.55;
    return float4(c, 1.0);
}
