// SPDX-FileCopyrightText: 2026 the makshell contributors
// SPDX-License-Identifier: GPL-3.0-or-later
#version 140

// Glass as a physical material: a flat interior with no glow, whose curved edge is a lens. Each pixel
// shows the background from further in, by an amount that grows sharply toward the edge, and the very
// edge also reflects the surroundings. That's why an edge can be pink right above purple wallpaper,
// and why it doesn't read as an outline.

// The background behind the shape (frosted ones: blurred), stretched over the shape.
uniform sampler2D sampler;
// The same background, blurred very widely.
uniform sampler2D wide;
// The same background, not blurred at all.
uniform sampler2D sharp;

// The size of what's being drawn in, which is what the background is stretched over, and the shape's
// corner radius, in logical pixels.
uniform vec2 geoSize;
uniform float cornerRadius;
// The shape's own size, and how far it's turned clockwise about the middle of geoSize, in radians.
uniform vec2 boxSize;
uniform float rotation;
// Where its middle is, from the middle of geoSize.
uniform vec2 boxOffset;
// Circles melted into the shape, like drops of liquid: where each is from the middle of the shape, its
// radius, which is 0 if there isn't one, and how far it reaches for the shape.
uniform vec4 droplets[4];
// Device pixels per logical pixel. The edge lines are sized in device pixels, to stay crisp.
uniform float deviceScale;

// The lens, when thickness isn't 0: a slab of glass whose edge curves down in a rounded shoulder. Each
// ray looks straight down, refracts where it enters the curved surface, and lands on the background
// below, so the whole bend follows from the shape and the material. Sizes are for a shape at least 62
// logical pixels across; smaller ones are the same lens, scaled down.
// Refracting through a bezel's normal, and the noise below, are adapted from MacShell's shader
// (https://github.com/0-ss/MacShell, MIT; see LICENSES/MacShell-MIT.txt).
// thickness: how tall the slab is at its very edge, in logical pixels.
uniform float thickness;
// How wide the shoulder is, in logical pixels, and how tall as a share of that.
uniform float bezel;
uniform float shoulder;
// Its curve, as a superellipse: 2 is a circle's arc, and higher stays flat for longer.
uniform float profile;
// The refractive index of green light, and how much higher it is for red and lower for blue.
uniform float ior;
uniform float iorSpread;
// How bright a light looks reflected in the shoulder, from the top left, with a dimmer glint across
// from it where that light leaves the glass again.
uniform float gloss;
// How much fine noise there is, which breaks up the banding of smooth blurs on an 8-bit screen.
uniform float grain;
// How much of a soft rim's color comes from what's behind it, rather than being a plain grey.
uniform float rimTint;
// Multiplies the background's color; below 1 darkens it.
uniform float brightness;
// Saturation of the background.
uniform mat4 colorMatrix;
// A color laid over the background: rgb, and how opaque it is.
uniform vec4 tint;
// The edge shows the wide blur, multiplied by edgeGain and mixed with white by edgeWhite. It either
// fades in over edgeFade logical pixels, or, with edgeFade 0, is a crisp two-device-pixel line
// inside a one-device-pixel dark line that darkens by outerAlpha.
uniform float edgeFade;
uniform float edgeGain;
uniform float edgeWhite;
uniform float outerAlpha;
// A rim instead of an edge: how much its outer device pixel whitens where it's lit. 0 means no rim.
uniform float rim;
// How much of the rim's light depends on its direction, from 0 (even all around) to 1.
uniform float rimDirectional;
// How much the next device pixel whitens, relative to the outer one.
uniform float rimInner;
// Instead: a rim that's a logical pixel and a half wide, soft on the inside, the color of bright
// glass rather than white.
uniform float rimSoft;
// A backdrop instead of a shape: just the blur, fading in from each side over this many logical
// pixels (left, top, right, bottom), with no edge. All 0 means it isn't one.
uniform vec4 softEdges;
// A tail pointing down from the shape: where its tip is from the left, how wide it is and how tall,
// in logical pixels. The tail is part of geoSize. A height of 0 means none, and a negative one means
// it's on top, pointing up.
uniform vec3 tail;
uniform float opacity;

// How wide the disc of samples of a backdrop's fade gets, in logical pixels.
const float maxBackdropDisc = 8.0;

// Texture coordinates: 0 to 1 across the shape, with y pointing up.
in vec2 uv;

out vec4 fragColor;

// Signed distance to a rounded box centered at the origin.
float sdRoundedBox(vec2 p, vec2 halfSize, float r)
{
    vec2 q = abs(p) - halfSize + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}

// Signed distance to an isosceles triangle with its tip at the origin, widening toward +y.
float sdTriangle(vec2 p, vec2 q)
{
    p.x = abs(p.x);
    vec2 a = p - q * clamp(dot(p, q) / dot(q, q), 0.0, 1.0);
    vec2 b = p - q * vec2(clamp(p.x / q.x, 0.0, 1.0), 1.0);
    float k = sign(q.y);
    float d = min(dot(a, a), dot(b, b));
    float s = max(k * (p.x * q.y - p.y * q.x), k * (p.y - q.y));
    return sqrt(d) * sign(s);
}

// Signed distance to the shape, which may have a tail flaring out of its bottom.
float sdShape(vec2 p, vec2 halfSize, float r)
{
    if (tail.z == 0.0) {
        return sdRoundedBox(p, halfSize, r);
    }
    // A tail on top is one at the bottom, upside down.
    if (tail.z < 0.0) {
        p.y = -p.y;
    }
    float tailHeight = abs(tail.z);
    float body = sdRoundedBox(p + vec2(0.0, tailHeight * 0.5), vec2(halfSize.x, halfSize.y - tailHeight * 0.5), r);
    // The tip is rounded by pulling the triangle in and growing it back, which keeps it as tall. It
    // carries on into the body at the same angle, so that the join is a clean one to smooth.
    const float tipRadius = 3.5;
    vec2 tip = vec2(p.x + halfSize.x - tail.x, halfSize.y - tipRadius - p.y);
    // Growing it back widens it as well, so it starts out that much narrower.
    vec2 reach = vec2(max(tail.y * 0.5 - tipRadius, 1.0), tailHeight) * 1.5;
    float triangle = sdTriangle(tip, reach) - tipRadius;
    // A smooth union, which flares the tail's base into the body.
    const float flare = 4.0;
    float h = clamp(0.5 + 0.5 * (triangle - body) / flare, 0.0, 1.0);
    return mix(triangle, body, h) - flare * h * (1.0 - h);
}

// Signed distance to the shape with its droplets melted in: a smooth union, which makes a neck between
// them while they're close, and pinches it off as they part.
float sdWhole(vec2 p, vec2 halfSize, float r)
{
    float d = sdShape(p, halfSize, r);
    for (int i = 0; i < 4; ++i) {
        if (droplets[i].z > 0.0) {
            float circle = length(p - droplets[i].xy) - droplets[i].z;
            float reach = max(droplets[i].w, 0.001);
            float h = clamp(0.5 + 0.5 * (circle - d) / reach, 0.0, 1.0);
            d = mix(circle, d, h) - reach * h * (1.0 - h);
        }
    }
    return d;
}

// Samples the background at shape coordinates (0 to 1, y pointing down).
vec4 sampleGeo(vec2 geo)
{
    return texture(sampler, vec2(geo.x, 1.0 - geo.y));
}

// Noise with a triangular distribution, around 0, the same for every channel.
float noise()
{
    vec3 p = fract(gl_FragCoord.xyx * 0.1031);
    p += dot(p, p.yzx + 33.33);
    float first = fract((p.x + p.y) * p.z);
    p = fract((gl_FragCoord.xyx * 1.7 + 17.0) * 0.1031);
    p += dot(p, p.yzx + 33.33);
    return first + fract((p.x + p.y) * p.z) - 1.0;
}

// Where a ray looking straight down lands on the background, relative to where it entered the glass.
vec2 throughGlass(vec3 surface, float height, float index)
{
    vec3 ray = refract(vec3(0.0, 0.0, -1.0), surface, 1.0 / index);
    return ray.xy / -ray.z * height;
}

// 1 between two distances from the edge, in device pixels, antialiased.
float band(float depth, float from, float to)
{
    return clamp(depth - from + 0.5, 0.0, 1.0) * clamp(to - depth + 0.5, 0.0, 1.0);
}

void main(void)
{
    vec2 geo = vec2(uv.x, 1.0 - uv.y);
    vec2 halfSize = geoSize * 0.5;
    vec2 p = geo * geoSize - halfSize;

    if (softEdges != vec4(0.0)) {
        vec2 near = geo * geoSize;
        vec2 far = geoSize - near;
        float amount = smoothstep(0.0, softEdges.x, near.x) * smoothstep(0.0, softEdges.y, near.y)
            * smoothstep(0.0, softEdges.z, far.x) * smoothstep(0.0, softEdges.w, far.y);
        // The blur grows instead of fading in, so nothing sharp shows through it: a disc of samples
        // that widens, handing over to the real blur before its samples could be told apart.
        const int samples = 16;
        float radius = amount * maxBackdropDisc;
        vec3 disc = vec3(0.0);
        for (int i = 0; i < samples; ++i) {
            float r = sqrt((float(i) + 0.5) / float(samples)) * radius;
            float angle = float(i) * 2.39996;
            vec2 at = geo + vec2(cos(angle), sin(angle)) * r / geoSize;
            disc += texture(sharp, vec2(at.x, 1.0 - at.y)).rgb;
        }
        vec3 color = mix(disc / float(samples), sampleGeo(geo).rgb, smoothstep(0.3, 0.85, amount));
        color *= mix(1.0, brightness, amount);
        // Only so the very first pixels don't pick up rounding.
        color += noise() * grain * amount;
        fragColor = vec4(color, 1.0) * clamp(amount * 20.0, 0.0, 1.0) * opacity;
        return;
    }

    // In the shape's own coordinates, where it's level.
    float turnCos = cos(rotation);
    float turnSin = sin(rotation);
    vec2 fromBox = p - boxOffset;
    vec2 level = vec2(fromBox.x * turnCos + fromBox.y * turnSin, -fromBox.x * turnSin + fromBox.y * turnCos);
    vec2 boxHalf = boxSize * 0.5;
    float d = sdWhole(level, boxHalf, cornerRadius);

    // Antialiased coverage over one device pixel.
    float coverage = clamp(0.5 - d * deviceScale, 0.0, 1.0);
    if (coverage <= 0.0) {
        discard;
    }

    // Outward surface normal from the SDF gradient, which only the lens and rims need: four more
    // distances for every pixel, which plain frosted glass, like an app's window, does without.
    vec2 n = vec2(0.0);
    if (thickness > 0.0 || rim > 0.0 || rimSoft > 0.0) {
        vec2 e = vec2(0.5, 0.0);
        n = vec2(
            sdWhole(level + e.xy, boxHalf, cornerRadius) - sdWhole(level - e.xy, boxHalf, cornerRadius),
            sdWhole(level + e.yx, boxHalf, cornerRadius) - sdWhole(level - e.yx, boxHalf, cornerRadius));
        // Back to the screen's coordinates, where the background and the light are.
        n = vec2(n.x * turnCos - n.y * turnSin, n.x * turnSin + n.y * turnCos);
        n = length(n) > 0.0 ? normalize(n) : vec2(0.0);
    }

    vec2 refracted = geo;
    vec3 surface = vec3(0.0, 0.0, 1.0);
    float slabHeight = 0.0;
    if (thickness > 0.0) {
        float fit = min(1.0, min(boxSize.x, boxSize.y) / 62.0);
        // A shoulder wider than the corners would crease along their diagonals.
        float width = max(min(bezel * fit, min(cornerRadius, min(boxHalf.x, boxHalf.y))), 1.0);
        float across = clamp(1.0 - max(-d, 0.0) / width, 0.0, 1.0);
        float curve = max(1.0 - pow(across, profile), 0.0);
        surface = normalize(vec3(n * shoulder * pow(across, profile - 1.0), pow(curve, (profile - 1.0) / profile)));
        slabHeight = thickness * fit + shoulder * width * pow(curve, 1.0 / profile);
        refracted = geo + throughGlass(surface, slabHeight, ior) / geoSize;
    }

    vec4 sampled = sampleGeo(refracted);
    if (thickness > 0.0) {
        sampled.r = sampleGeo(geo + throughGlass(surface, slabHeight, ior + iorSpread) / geoSize).r;
        sampled.b = sampleGeo(geo + throughGlass(surface, slabHeight, ior - iorSpread) / geoSize).b;
    }
    vec4 color = sampled * colorMatrix;
    // The copied background is opaque wherever there's anything behind it.
    color.a = 1.0;
    color.rgb *= brightness;
    color.rgb = mix(color.rgb, tint.rgb, tint.a);

    if (thickness > 0.0) {
        // Glass reflects more the more glancing the view is (Schlick's approximation): here, the
        // blurred surroundings.
        float straightOn = pow((ior - 1.0) / (ior + 1.0), 2.0);
        float fresnel = straightOn + (1.0 - straightOn) * pow(1.0 - surface.z, 5.0);
        color.rgb = mix(color.rgb, min(texture(wide, uv).rgb * edgeGain, vec3(1.0)), fresnel);
        vec3 mirrored = reflect(vec3(0.0, 0.0, -1.0), surface);
        const vec3 light = normalize(vec3(-0.45, -0.68, 0.58));
        float key = pow(max(dot(mirrored, light), 0.0), 24.0);
        float glint = pow(max(dot(mirrored, vec3(-light.xy, light.z)), 0.0), 24.0);
        color.rgb += gloss * (key + 0.55 * glint);
    }

    // The edge, with no dimming: it shows the surroundings, not what's right behind it.
    vec3 edge = mix(min(texture(wide, uv).rgb * edgeGain, vec3(1.0)), vec3(1.0), edgeWhite);
    float pixels = -d * deviceScale;
    if (rimSoft > 0.0) {
        float lit = mix(0.12, 1.0, smoothstep(0.25, 0.72, abs(dot(n, vec2(0.7071)))));
        // Half a logical pixel in full, then fading out by one and a half.
        float line = clamp(1.4 - max(-d, 0.0), 0.0, 1.0);
        vec3 rimColor = mix(vec3(0.85), clamp(color.rgb * 2.0 + 0.18, 0.0, 1.0), rimTint);
        color.rgb = mix(color.rgb, rimColor, line * lit * rim);
    } else if (rim > 0.0) {
        // Light falls along the top-left to bottom-right diagonal: the rim is even across those two
        // quadrants and almost gone at the other two corners, measured around Tahoe's buttons.
        float lit = mix(0.12, 1.0, smoothstep(0.25, 0.72, abs(dot(n, vec2(0.7071)))));
        lit = mix(1.0, lit, rimDirectional);
        float line = band(pixels, 0.0, 1.0) + band(pixels, 1.0, 2.0) * rimInner;
        color.rgb = mix(color.rgb, vec3(1.0), line * lit * rim);
    } else if (edgeFade > 0.0) {
        color.rgb = mix(color.rgb, edge, 1.0 - smoothstep(0.0, edgeFade, -d));
    } else {
        color.rgb = mix(color.rgb, edge, band(pixels, 1.0, 3.0));
        color.rgb = mix(color.rgb, vec3(0.0), band(pixels, 0.0, 1.0) * outerAlpha);
    }

    color.rgb += noise() * grain;
    fragColor = color * coverage * opacity;
}
