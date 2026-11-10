// SPDX-FileCopyrightText: 2026 the makshell contributors
// SPDX-License-Identifier: GPL-3.0-or-later
#version 140

// A window's own contents with parts of them out of focus, the way things come into view on Tahoe:
// blurred at first, and sharp a moment later. The blur is read from the texture's smaller copies of
// itself, so that any amount of it costs the same few samples.

#include "colormanagement.glsl"
#include "saturation.glsl"

// The window, drawn by itself, with smaller copies of itself (mipmaps).
uniform sampler2D sampler;
uniform vec4 modulation;
// The parts to blur, in the coordinates the texture is sampled with (left, top, right, bottom), and
// how much each is blurred: which of the smaller copies it's read from, fractions included.
uniform vec4 rects[16];
uniform float lods[16];
uniform int rectCount;
// How much all of it is blurred.
uniform float everywhere;
// How far past a part's edges its blur fades out, in those coordinates.
uniform vec2 feather;

in vec2 texcoord0;

out vec4 fragColor;

void main(void)
{
    float lod = everywhere;
    for (int i = 0; i < 16; ++i) {
        if (i < rectCount) {
            vec2 inside = min(texcoord0 - rects[i].xy, rects[i].zw - texcoord0) / feather;
            lod = max(lod, lods[i] * clamp(min(inside.x, inside.y) + 1.0, 0.0, 1.0));
        }
    }

    vec4 result;
    if (lod <= 0.01) {
        result = textureLod(sampler, texcoord0, 0.0);
    } else {
        // Nine samples a blurred pixel apart, weighted like a tent, which smooths the copies' squares.
        // The distance goes to nothing with the blur, so the last of it doesn't end in a jump.
        vec2 apart = (exp2(lod) - 1.0) / vec2(textureSize(sampler, 0));
        result = vec4(0.0);
        for (int y = -1; y <= 1; ++y) {
            for (int x = -1; x <= 1; ++x) {
                float weight = (2.0 - abs(float(x))) * (2.0 - abs(float(y)));
                result += textureLod(sampler, texcoord0 + vec2(x, y) * apart, lod) * weight;
            }
        }
        result /= 16.0;
    }

    result = encodingToNits(result, sourceNamedTransferFunction, sourceTransferFunctionParams.x, sourceTransferFunctionParams.y);
    result.rgb = (colorimetryTransform * vec4(result.rgb, 1.0)).rgb;
    result = adjustSaturation(result);
    result *= modulation;
    result.rgb = doTonemapping(result.rgb);
    fragColor = nitsToDestinationEncoding(result);
}
