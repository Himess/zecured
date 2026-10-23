// GLSL for the scene. Colours are display (sRGB) values: these materials write them as they are, the way the
// renderer writes the clear colour, so no colour-space conversion happens anywhere.

/** The sky: dark above and below, a band of teal light along the horizon between the two chains. */
export const skyVertex = /* glsl */ `
  varying vec3 vDir;
  void main() {
    vDir = normalize(position);
    vec4 p = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
    gl_Position = p.xyww;
  }
`;

export const skyFragment = /* glsl */ `
  uniform vec3 uTop;
  uniform vec3 uHorizon;
  uniform vec3 uBottom;
  uniform vec3 uGlow;
  uniform vec3 uWarm;
  varying vec3 vDir;
  void main() {
    vec3 d = normalize(vDir);
    float y = d.y;
    vec3 col = mix(uHorizon, uTop, smoothstep(0.0, 0.5, y));
    col = mix(col, uBottom, smoothstep(0.0, -0.4, y));
    float band = exp(-pow(y * 14.0, 2.0));
    float line = exp(-pow(y * 60.0, 2.0));
    float ahead = pow(max(0.0, -d.z), 3.0);
    col += uGlow * band * (0.25 + 0.5 * ahead) + uGlow * line * 0.35 * ahead;
    col += uWarm * exp(-pow(y * 28.0, 2.0)) * pow(max(0.0, -d.z), 18.0);
    gl_FragColor = vec4(col, 1.0);
  }
`;

/** The shielded pool: a breathing field of encrypted notes, fading into the distance. */
export const poolVertex = /* glsl */ `
  uniform float uTime;
  uniform float uPixelRatio;
  uniform float uSize;
  uniform vec3 uDeep;
  uniform vec3 uCrest;
  uniform vec3 uGold;
  uniform vec3 uVault;
  uniform float uVaultGlow;
  uniform vec3 uSpot;
  uniform float uSpotGlow;
  attribute float aSeed;
  varying vec3 vColor;
  varying float vAlpha;

  float wave(vec2 p, float t) {
    return sin(p.x * 0.17 + t * 1.3) * 0.34
      + sin(p.y * 0.21 - t * 1.05) * 0.30
      + sin((p.x + p.y) * 0.09 + t * 0.62) * 0.42
      + sin((p.x - p.y) * 0.31 + t * 1.9) * 0.08;
  }

  void main() {
    vec3 p = position;
    float t = uTime * 0.32;
    float w = wave(p.xz, t);
    p.y += w + sin(aSeed * 40.0 + uTime * 1.7) * 0.03;
    vec4 mv = modelViewMatrix * vec4(p, 1.0);
    float dist = -mv.z;
    gl_Position = projectionMatrix * mv;
    float size = uSize * (0.5 + aSeed * 0.8) * uPixelRatio * (15.0 / max(dist, 0.5));
    gl_PointSize = min(size, 7.0 * uPixelRatio);

    float crest = smoothstep(-0.7, 0.9, w);
    vec3 col = mix(uDeep, uCrest, crest);
    // A few notes glint, as if one of them were yours.
    float glint = step(0.993, aSeed) * (0.5 + 0.5 * sin(uTime * 1.6 + aSeed * 90.0));
    col += uGold * glint * 1.1;
    float dv = distance(p.xz, uVault.xz);
    col += uGold * uVaultGlow * exp(-dv * dv * 0.11) * 0.95;
    float ds = distance(p.xz, uSpot.xz);
    col += uGold * uSpotGlow * exp(-ds * ds * 0.2) * 0.9;
    vColor = col;
    vAlpha = smoothstep(90.0, 6.0, dist) * smoothstep(1.5, 6.0, dist) * (0.22 + 0.78 * crest);
  }
`;

export const poolFragment = /* glsl */ `
  varying vec3 vColor;
  varying float vAlpha;
  void main() {
    float d = length(gl_PointCoord - 0.5);
    float a = smoothstep(0.5, 0.05, d);
    gl_FragColor = vec4(vColor * a * vAlpha, 1.0);
  }
`;

/** Base: a ruled ledger of light above the pool, where the proofs land. */
export const ledgerVertex = /* glsl */ `
  varying vec3 vWorld;
  varying float vDist;
  void main() {
    vec4 world = modelMatrix * vec4(position, 1.0);
    vWorld = world.xyz;
    vec4 mv = viewMatrix * world;
    vDist = -mv.z;
    gl_Position = projectionMatrix * mv;
  }
`;

export const ledgerFragment = /* glsl */ `
  uniform float uTime;
  uniform vec3 uLine;
  uniform vec3 uGold;
  uniform vec3 uRed;
  uniform float uOpacity;
  uniform float uCells;
  uniform vec3 uImpact[4];
  uniform float uImpactAge[4];
  uniform vec3 uMark;
  uniform float uMarkGlow;
  uniform float uAlarm;
  uniform vec3 uFocus;
  uniform float uFocusR;
  varying vec3 vWorld;
  varying float vDist;

  float grid(vec2 c, float scale, float width) {
    vec2 g = c / scale;
    vec2 d = abs(fract(g - 0.5) - 0.5) / (fwidth(g) * width);
    return 1.0 - min(min(d.x, d.y), 1.0);
  }

  float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
  }

  void main() {
    vec2 c = vWorld.xz;
    float fade = smoothstep(75.0, 5.0, vDist) * smoothstep(0.8, 3.0, vDist);
    vec2 f = c - uFocus.xz;
    float focus = exp(-dot(f, f) / (uFocusR * uFocusR));
    float minor = grid(c, 1.0, 0.9) * 0.10 * smoothstep(24.0, 5.0, vDist);
    float major = grid(c, 4.0, 1.0) * 0.36;
    float lines = max(minor, major) * (0.18 + 0.82 * focus);

    // Where a proof lands: a stamp, then a ring opening across the ledger and lighting its lines.
    float stamp = 0.0;
    for (int i = 0; i < 4; i++) {
      float age = uImpactAge[i];
      if (age >= 0.0 && age < 1.0) {
        float r = distance(c, uImpact[i].xz);
        float ring = exp(-pow((r - age * 12.0) * 1.4, 2.0)) * (1.0 - age);
        float core = exp(-r * r * 0.9) * pow(1.0 - age, 2.0) * 2.2;
        stamp += ring + core;
        lines += max(minor, major) * exp(-pow((r - age * 12.0) * 0.6, 2.0)) * (1.0 - age) * 2.5;
      }
    }
    vec3 col = uLine * lines * uOpacity + uGold * stamp;

    // Blocks of the ledger lighting up now and then: the public record being written.
    vec2 cell = floor(c / 4.0);
    float epoch = floor(uTime * 0.45 + hash(cell) * 7.0);
    float lit = step(1.0 - uCells, hash(cell + epoch * 1.37));
    float phase = fract(uTime * 0.45 + hash(cell) * 7.0);
    vec2 inCell = fract(c / 4.0);
    float box = smoothstep(0.0, 0.05, inCell.x) * smoothstep(0.0, 0.05, inCell.y)
      * smoothstep(1.0, 0.95, inCell.x) * smoothstep(1.0, 0.95, inCell.y);
    col += uLine * lit * box * (1.0 - phase) * 0.07 * uOpacity * (0.3 + 0.7 * focus);

    // A highlighted square: an account, an escrow, a vault's record.
    vec2 m = abs(c - uMark.xz);
    float edge = max(m.x, m.y);
    float frame = smoothstep(0.09, 0.0, abs(edge - 1.2)) + smoothstep(1.2, 0.0, edge) * 0.12;
    col += mix(uLine, uRed, uAlarm) * frame * uMarkGlow * 1.3;

    gl_FragColor = vec4(col * fade, 1.0);
  }
`;

/** Comets: a head and a fading tail along a curve, for proofs, notes and USDC. */
export const cometVertex = /* glsl */ `
  uniform float uPixelRatio;
  uniform vec3 uFrom[8];
  uniform vec3 uTo[8];
  uniform float uLift[8];
  uniform float uProgress[8];
  uniform float uSize[8];
  uniform vec3 uColor[8];
  uniform float uOn[8];
  attribute float aComet;
  attribute float aTrail;
  varying vec3 vColor;
  varying float vAlpha;

  vec3 path(int i, float s) {
    vec3 a = uFrom[i];
    vec3 b = uTo[i];
    vec3 p = mix(a, b, s);
    p.y += sin(s * 3.14159) * uLift[i];
    return p;
  }

  void main() {
    int i = int(aComet + 0.5);
    float head = uProgress[i];
    float s = head - aTrail * 0.3;
    float visible = step(0.0, s) * step(s, 1.0) * uOn[i];
    s = clamp(s, 0.0, 1.0);
    vec3 p = path(i, s);
    vec4 mv = modelViewMatrix * vec4(p, 1.0);
    gl_Position = projectionMatrix * mv;
    float taper = pow(1.0 - aTrail, 1.8);
    float head1 = aTrail < 0.001 ? 1.0 : 0.0;
    gl_PointSize = uSize[i] * uPixelRatio * (0.18 + taper * 0.7 + head1 * 1.6) * (20.0 / max(-mv.z, 0.5)) * visible;
    vColor = uColor[i];
    vAlpha = visible * (head1 > 0.5 ? 1.0 : taper * 0.85);
  }
`;

export const cometFragment = /* glsl */ `
  varying vec3 vColor;
  varying float vAlpha;
  void main() {
    float d = length(gl_PointCoord - 0.5) * 2.0;
    float core = exp(-d * d * 18.0);
    float halo = exp(-d * d * 3.2) * 0.75;
    vec3 col = vColor * (halo + core * 1.6) + vec3(1.0, 0.97, 0.9) * core * 0.9;
    gl_FragColor = vec4(col * vAlpha, 1.0);
  }
`;

/** Small lights: the signers around a vault, the market and the account. */
export const nodesVertex = /* glsl */ `
  uniform float uPixelRatio;
  uniform float uTime;
  attribute vec3 aColor;
  attribute float aSize;
  varying vec3 vColor;
  void main() {
    vec4 mv = modelViewMatrix * vec4(position, 1.0);
    gl_Position = projectionMatrix * mv;
    gl_PointSize = aSize * uPixelRatio * (20.0 / max(-mv.z, 0.5)) * (0.92 + 0.08 * sin(uTime * 2.2 + position.x * 3.0));
    vColor = aColor;
  }
`;

export const nodesFragment = /* glsl */ `
  varying vec3 vColor;
  void main() {
    float d = length(gl_PointCoord - 0.5) * 2.0;
    float core = exp(-d * d * 14.0);
    float halo = exp(-d * d * 3.5) * 0.55;
    float light = max(max(vColor.r, vColor.g), vColor.b);
    gl_FragColor = vec4(vColor * (core * 1.3 + halo) + vec3(light) * core * 0.45, 1.0);
  }
`;

/** Lines with a colour and an alpha per vertex (threads between the chains, a vault's ring). */
export const linesVertex = /* glsl */ `
  attribute vec3 aColor;
  attribute float aAlpha;
  varying vec3 vColor;
  varying float vAlpha;
  void main() {
    vColor = aColor;
    vAlpha = aAlpha;
    gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
  }
`;

export const linesFragment = /* glsl */ `
  uniform float uOpacity;
  varying vec3 vColor;
  varying float vAlpha;
  void main() {
    gl_FragColor = vec4(vColor * vAlpha * uOpacity, 1.0);
  }
`;
