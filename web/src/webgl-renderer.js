/**
 * WebGL Renderer for SG1 Emulator
 *
 * Zero-copy framebuffer display using WebGL textures.
 * Replaces Canvas2D putImageData with texSubImage2D + fullscreen quad,
 * eliminating per-frame JS allocations and enabling GPU-accelerated scaling.
 *
 * Supports optional post-processing shaders (CRT effects, scaling filters)
 * using RetroArch-compatible GLSL shader format.
 *
 * Usage:
 *   const renderer = new WebGLRenderer(canvasElement);
 *   renderer.uploadFrame(rgbaPixels, 320, 224);
 *   renderer.drawFrame();
 *   renderer.setShader(shaderDef);  // Enable CRT shader
 *   renderer.setShader(null);       // Disable shader
 */

const VERTEX_SHADER_SRC = [
    'attribute vec2 a_pos;',
    'varying vec2 v_uv;',
    'void main() {',
    '    v_uv = vec2(a_pos.x * 0.5 + 0.5, 1.0 - (a_pos.y * 0.5 + 0.5));',
    '    gl_Position = vec4(a_pos, 0.0, 1.0);',
    '}',
].join('\n');

const FRAGMENT_SHADER_SRC = [
    'precision mediump float;',
    'uniform sampler2D u_texture;',
    'varying vec2 v_uv;',
    'void main() {',
    '    gl_FragColor = texture2D(u_texture, v_uv);',
    '}',
].join('\n');

// Identity matrix for RetroArch MVPMatrix uniform
const IDENTITY_MATRIX = new Float32Array([
    1, 0, 0, 0,
    0, 1, 0, 0,
    0, 0, 1, 0,
    0, 0, 0, 1,
]);

export class WebGLRenderer {
    /**
     * @param {HTMLCanvasElement} canvas - The canvas element to render to
     */
    constructor(canvas) {
        this.canvas = canvas;
        this.gl = null;
        this.passthroughProgram = null;
        this.texture = null;
        this.vao = null;
        this.vertexBuffer = null;
        this.texWidth = 0;
        this.texHeight = 0;
        this.isWebGL2 = false;
        this.useFallback = false;
        this.fallbackCtx = null;
        this._resizeObserver = null;
        this._aPos = -1;
        this._uTexture = null;

        // Shader support
        this.shaderProgram = null;
        this.shaderVao = null;
        this.shaderVertexBuffer = null;
        this.frameCount = 0;
        this._shaderFilterLinear = false;
        this._shaderUniformCache = null;

        this._initGL();
    }

    _initGL() {
        // Try WebGL2 first, then WebGL1
        let gl = this.canvas.getContext('webgl2', {
            alpha: false,
            antialias: false,
            premultipliedAlpha: false,
            preserveDrawingBuffer: false,
        });

        if (gl) {
            this.isWebGL2 = true;
        } else {
            gl = this.canvas.getContext('webgl', {
                alpha: false,
                antialias: false,
                premultipliedAlpha: false,
                preserveDrawingBuffer: false,
            });
        }

        if (!gl) {
            console.warn('WebGL not available, falling back to Canvas2D');
            this.useFallback = true;
            this.fallbackCtx = this.canvas.getContext('2d');
            if (this.fallbackCtx) {
                this.fallbackCtx.imageSmoothingEnabled = false;
            }
            return;
        }

        this.gl = gl;

        // Compile shaders and link passthrough program
        this.passthroughProgram = this._createProgram(VERTEX_SHADER_SRC, FRAGMENT_SHADER_SRC);
        gl.useProgram(this.passthroughProgram);

        // Get attribute/uniform locations
        this._aPos = gl.getAttribLocation(this.passthroughProgram, 'a_pos');
        this._uTexture = gl.getUniformLocation(this.passthroughProgram, 'u_texture');

        // Create fullscreen quad (triangle strip: BL, BR, TL, TR)
        const quadVerts = new Float32Array([
            -1, -1,
             1, -1,
            -1,  1,
             1,  1,
        ]);

        this.vertexBuffer = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.vertexBuffer);
        gl.bufferData(gl.ARRAY_BUFFER, quadVerts, gl.STATIC_DRAW);

        // Setup VAO (WebGL2) or configure attributes directly (WebGL1)
        if (this.isWebGL2) {
            this.vao = gl.createVertexArray();
            gl.bindVertexArray(this.vao);
        }

        gl.enableVertexAttribArray(this._aPos);
        gl.vertexAttribPointer(this._aPos, 2, gl.FLOAT, false, 0, 0);

        if (this.isWebGL2) {
            gl.bindVertexArray(null);
        }

        // Create texture for framebuffer
        this.texture = gl.createTexture();
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, this.texture);

        // NEAREST filtering for pixel-perfect scaling (no blurring)
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

        // Set texture unit for the shader
        gl.uniform1i(this._uTexture, 0);

        // Clear color black
        gl.clearColor(0, 0, 0, 1);

        // Initialize shader-mode VAO (WebGL2 only)
        if (this.isWebGL2) {
            this._initShaderVao();
        }

        // Setup ResizeObserver for automatic scaling
        this._setupResizeObserver();

        console.log('WebGL renderer initialized (' + (this.isWebGL2 ? 'WebGL2' : 'WebGL1') + ')');
    }

    /**
     * Initialize the VAO used for RetroArch-compatible shaders.
     * Provides VertexCoord (vec4) and TexCoord (vec4) attributes.
     */
    _initShaderVao() {
        const gl = this.gl;

        // 8 floats per vertex: VertexCoord(x,y,z,w) + TexCoord(s,t,0,0)
        // Y-flip is encoded in TexCoord: top-left screen maps to texture (0,0),
        // bottom-left screen maps to texture (0,1), matching the WASM framebuffer layout.
        const shaderQuadVerts = new Float32Array([
            // VertexCoord        TexCoord
            -1, -1, 0, 1,       0, 1, 0, 0,  // BL
             1, -1, 0, 1,       1, 1, 0, 0,  // BR
            -1,  1, 0, 1,       0, 0, 0, 0,  // TL
             1,  1, 0, 1,       1, 0, 0, 0,  // TR
        ]);

        this.shaderVertexBuffer = gl.createBuffer();
        gl.bindBuffer(gl.ARRAY_BUFFER, this.shaderVertexBuffer);
        gl.bufferData(gl.ARRAY_BUFFER, shaderQuadVerts, gl.STATIC_DRAW);

        this.shaderVao = gl.createVertexArray();
        gl.bindVertexArray(this.shaderVao);

        const stride = 8 * 4; // 8 floats * 4 bytes

        // VertexCoord at location 0 (vec4)
        gl.enableVertexAttribArray(0);
        gl.vertexAttribPointer(0, 4, gl.FLOAT, false, stride, 0);

        // TexCoord at location 1 (vec4)
        gl.enableVertexAttribArray(1);
        gl.vertexAttribPointer(1, 4, gl.FLOAT, false, stride, 4 * 4);

        // COLOR at location 2 (provide default white via disabled attribute)
        gl.disableVertexAttribArray(2);
        gl.vertexAttrib4f(2, 1, 1, 1, 1);

        gl.bindVertexArray(null);
    }

    _compileShader(type, source) {
        const gl = this.gl;
        const shader = gl.createShader(type);
        gl.shaderSource(shader, source);
        gl.compileShader(shader);

        if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
            const info = gl.getShaderInfoLog(shader);
            gl.deleteShader(shader);
            throw new Error('Shader compilation failed: ' + info);
        }

        return shader;
    }

    _createProgram(vsSrc, fsSrc) {
        const gl = this.gl;
        const vs = this._compileShader(gl.VERTEX_SHADER, vsSrc);
        const fs = this._compileShader(gl.FRAGMENT_SHADER, fsSrc);

        const program = gl.createProgram();
        gl.attachShader(program, vs);
        gl.attachShader(program, fs);

        // Bind RetroArch-compatible attribute locations for shader programs
        // (passthrough uses its own a_pos attribute which is fine)
        gl.bindAttribLocation(program, 0, 'VertexCoord');
        gl.bindAttribLocation(program, 1, 'TexCoord');
        gl.bindAttribLocation(program, 2, 'COLOR');
        gl.bindAttribLocation(program, 2, 'Color');

        gl.linkProgram(program);

        if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
            const info = gl.getProgramInfoLog(program);
            gl.deleteProgram(program);
            throw new Error('Shader program linking failed: ' + info);
        }

        gl.deleteShader(vs);
        gl.deleteShader(fs);

        return program;
    }

    /**
     * Upload framebuffer pixel data to the GPU texture.
     *
     * @param {Uint8Array|Uint8ClampedArray} pixels - RGBA pixel data
     * @param {number} width - Framebuffer width in pixels
     * @param {number} height - Framebuffer height in pixels
     */
    uploadFrame(pixels, width, height) {
        if (this.useFallback) {
            this._fallbackUpload(pixels, width, height);
            return;
        }

        const gl = this.gl;
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, this.texture);

        if (width !== this.texWidth || height !== this.texHeight) {
            // Texture size changed (e.g. H32->H40 mode switch): reallocate
            gl.texImage2D(
                gl.TEXTURE_2D, 0, gl.RGBA,
                width, height, 0,
                gl.RGBA, gl.UNSIGNED_BYTE,
                pixels
            );
            this.texWidth = width;
            this.texHeight = height;

            // Re-evaluate canvas size for new resolution
            const container = this.canvas.parentElement;
            if (container) {
                const rect = container.getBoundingClientRect();
                this._updateCanvasSize(rect.width, rect.height);
            }
        } else {
            // Same size: fast sub-image update
            gl.texSubImage2D(
                gl.TEXTURE_2D, 0,
                0, 0, width, height,
                gl.RGBA, gl.UNSIGNED_BYTE,
                pixels
            );
        }
    }

    /**
     * Draw the framebuffer texture as a fullscreen quad.
     */
    drawFrame() {
        if (this.useFallback) return;

        const gl = this.gl;

        gl.viewport(0, 0, this.canvas.width, this.canvas.height);
        gl.clear(gl.COLOR_BUFFER_BIT);

        if (this.shaderProgram && this.isWebGL2) {
            // CRT / post-processing shader path
            gl.useProgram(this.shaderProgram);
            this._setShaderUniforms();
            gl.bindVertexArray(this.shaderVao);
        } else {
            // Passthrough path (no shader)
            gl.useProgram(this.passthroughProgram);

            if (this.isWebGL2) {
                gl.bindVertexArray(this.vao);
            } else {
                gl.bindBuffer(gl.ARRAY_BUFFER, this.vertexBuffer);
                gl.enableVertexAttribArray(this._aPos);
                gl.vertexAttribPointer(this._aPos, 2, gl.FLOAT, false, 0, 0);
            }
        }

        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, this.texture);

        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);

        if (this.isWebGL2) {
            gl.bindVertexArray(null);
        }

        this.frameCount++;
    }

    /**
     * Set RetroArch-compatible uniforms for the active shader program.
     */
    _setShaderUniforms() {
        const gl = this.gl;
        const cache = this._shaderUniformCache;
        if (!cache) return;

        if (cache.TextureSize !== null) {
            gl.uniform2f(cache.TextureSize, this.texWidth, this.texHeight);
        }
        if (cache.InputSize !== null) {
            gl.uniform2f(cache.InputSize, this.texWidth, this.texHeight);
        }
        if (cache.OutputSize !== null) {
            gl.uniform2f(cache.OutputSize, this.canvas.width, this.canvas.height);
        }
        if (cache.FrameCount !== null) {
            gl.uniform1i(cache.FrameCount, this.frameCount);
        }
        if (cache.FrameDirection !== null) {
            gl.uniform1i(cache.FrameDirection, 1);
        }
        if (cache.MVPMatrix !== null) {
            gl.uniformMatrix4fv(cache.MVPMatrix, false, IDENTITY_MATRIX);
        }
        if (cache.Texture !== null) {
            gl.uniform1i(cache.Texture, 0);
        }
    }

    /**
     * Cache uniform locations for the current shader program.
     */
    _cacheShaderUniforms() {
        const gl = this.gl;
        const program = this.shaderProgram;
        if (!program) {
            this._shaderUniformCache = null;
            return;
        }

        this._shaderUniformCache = {
            TextureSize: gl.getUniformLocation(program, 'TextureSize'),
            InputSize: gl.getUniformLocation(program, 'InputSize'),
            OutputSize: gl.getUniformLocation(program, 'OutputSize'),
            FrameCount: gl.getUniformLocation(program, 'FrameCount'),
            FrameDirection: gl.getUniformLocation(program, 'FrameDirection'),
            MVPMatrix: gl.getUniformLocation(program, 'MVPMatrix'),
            Texture: gl.getUniformLocation(program, 'Texture'),
        };
    }

    /**
     * Set texture filtering mode.
     * @param {boolean} linear - true for LINEAR, false for NEAREST
     */
    _setTextureFiltering(linear) {
        const gl = this.gl;
        if (!gl) return;

        const filter = linear ? gl.LINEAR : gl.NEAREST;
        gl.bindTexture(gl.TEXTURE_2D, this.texture);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    }

    /**
     * Compile and activate a RetroArch-compatible GLSL shader,
     * or pass null to revert to the passthrough shader.
     *
     * shaderDef format:
     *   {
     *     source: string,          // Raw RetroArch GLSL source (combined vertex+fragment)
     *     filterLinear: boolean,    // Whether to use LINEAR texture filtering
     *   }
     *
     * @param {object|null} shaderDef - Shader definition or null to disable
     * @returns {boolean} true if shader compiled successfully
     */
    setShader(shaderDef) {
        if (!this.isWebGL2 || !this.gl) return false;

        const gl = this.gl;

        // Clean up previous shader
        if (this.shaderProgram) {
            gl.deleteProgram(this.shaderProgram);
            this.shaderProgram = null;
            this._shaderUniformCache = null;
        }

        if (!shaderDef) {
            // Revert to passthrough
            this._setTextureFiltering(false);
            this._shaderFilterLinear = false;
            return true;
        }

        try {
            // Build vertex and fragment sources from the combined RetroArch GLSL.
            // The source uses #if defined(VERTEX) / #elif defined(FRAGMENT) guards.
            // We prepend #version 300 es and the appropriate #define.
            // Fragment shaders need `precision highp float` before any declarations
            // because GLSL 300 es has no default float precision for fragments,
            // and many RetroArch shaders declare `out vec4 FragColor` before their
            // own precision statement.
            // The sources are kept as their authors published them, and some
            // open with a `#version` line of their own; GLSL allows one, first,
            // so it is dropped here.
            const body = shaderDef.source.replace(/^[ \t]*#version[^\n]*\n?/m, '');
            const vsSrc = '#version 300 es\n#define VERTEX\n' + body;
            const fsSrc = '#version 300 es\nprecision highp float;\n#define FRAGMENT\n' + body;

            this.shaderProgram = this._createProgram(vsSrc, fsSrc);
            this._cacheShaderUniforms();

            this._shaderFilterLinear = shaderDef.filterLinear || false;
            this._setTextureFiltering(this._shaderFilterLinear);

            this.frameCount = 0;
            return true;
        } catch (e) {
            console.error('Failed to compile shader:', e);
            this.shaderProgram = null;
            this._shaderUniformCache = null;
            this._setTextureFiltering(false);
            this._shaderFilterLinear = false;
            return false;
        }
    }

    /**
     * Check if a shader is currently active.
     * @returns {boolean}
     */
    hasActiveShader() {
        return this.shaderProgram !== null;
    }

    /**
     * Canvas2D fallback for when WebGL is not available.
     */
    _fallbackUpload(pixels, width, height) {
        if (!this.fallbackCtx) return;

        if (this.canvas.width !== width || this.canvas.height !== height) {
            this.canvas.width = width;
            this.canvas.height = height;
        }

        const clamped = (pixels instanceof Uint8ClampedArray)
            ? pixels
            : new Uint8ClampedArray(pixels.buffer, pixels.byteOffset, pixels.byteLength);

        const imageData = new ImageData(clamped, width, height);
        this.fallbackCtx.putImageData(imageData, 0, 0);
    }

    /**
     * Setup ResizeObserver to handle canvas container resizing.
     */
    _setupResizeObserver() {
        const container = this.canvas.parentElement;
        if (!container) return;

        this._resizeObserver = new ResizeObserver((entries) => {
            for (const entry of entries) {
                const rect = entry.contentRect;
                this._updateCanvasSize(rect.width, rect.height);
            }
        });

        this._resizeObserver.observe(container);

        // Initial size update
        const rect = container.getBoundingClientRect();
        this._updateCanvasSize(rect.width, rect.height);
    }

    /**
     * Update canvas dimensions with integer scaling to fill the container.
     *
     * @param {number} containerW - Container width in CSS pixels
     * @param {number} containerH - Container height in CSS pixels
     */
    _updateCanvasSize(containerW, containerH) {
        const fbW = this.texWidth || 320;
        const fbH = this.texHeight || 224;

        // Largest integer scale that fits both dimensions
        const scaleX = Math.floor(containerW / fbW);
        const scaleY = Math.floor(containerH / fbH);
        const scale = Math.max(1, Math.min(scaleX, scaleY));

        const canvasW = fbW * scale;
        const canvasH = fbH * scale;

        if (this.canvas.width !== canvasW || this.canvas.height !== canvasH) {
            this.canvas.width = canvasW;
            this.canvas.height = canvasH;
            this.canvas.style.width = canvasW + 'px';
            this.canvas.style.height = canvasH + 'px';
        }
    }

    /**
     * Clean up all WebGL resources.
     */
    destroy() {
        if (this._resizeObserver) {
            this._resizeObserver.disconnect();
            this._resizeObserver = null;
        }

        if (this.gl) {
            const gl = this.gl;
            if (this.texture) gl.deleteTexture(this.texture);
            if (this.vertexBuffer) gl.deleteBuffer(this.vertexBuffer);
            if (this.vao && this.isWebGL2) gl.deleteVertexArray(this.vao);
            if (this.passthroughProgram) gl.deleteProgram(this.passthroughProgram);
            if (this.shaderProgram) gl.deleteProgram(this.shaderProgram);
            if (this.shaderVertexBuffer) gl.deleteBuffer(this.shaderVertexBuffer);
            if (this.shaderVao) gl.deleteVertexArray(this.shaderVao);
        }

        this.gl = null;
        this.passthroughProgram = null;
        this.shaderProgram = null;
        this.texture = null;
    }
}
