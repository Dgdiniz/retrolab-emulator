// Shader definitions registry for SG1 Emulator.
//
// Each shader is a file of libretro/glsl-shaders embedded as published, and
// each states a permissive license in its own header (public domain, the
// Unlicense or MIT). A shader whose file states no license, or a copyleft
// one, does not belong here.
import { crtLottes } from './crt-lottes.js';
import { crtLottesFast } from './crt-lottes-fast.js';
import { crtNobody } from './crt-nobody.js';
import { omniscale } from './omniscale.js';
import { bicubic } from './bicubic.js';

export const SHADER_DEFS = {
    'crt-lottes': crtLottes,
    'crt-lottes-fast': crtLottesFast,
    'crt-nobody': crtNobody,
    'omniscale': omniscale,
    'bicubic': bicubic,
};
