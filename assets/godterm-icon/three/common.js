// Shared rig for the GodTerm 3D icons. World units = pixels of a 1024 icon canvas at z = 0.
import * as THREE from 'three';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import { EffectComposer } from 'three/addons/postprocessing/EffectComposer.js';
import { RenderPass } from 'three/addons/postprocessing/RenderPass.js';
import { UnrealBloomPass } from 'three/addons/postprocessing/UnrealBloomPass.js';
import { OutputPass } from 'three/addons/postprocessing/OutputPass.js';
import { RoundedBoxGeometry } from 'three/addons/geometries/RoundedBoxGeometry.js';
export { THREE, RoundedBoxGeometry };

export const SIZE = 2048;          // render resolution
export const BODY_HALF = 421;      // silhouette half-size; the post step re-masks at 412 (824/1024)

export function rig({ exposure = 1.0, envIntensity = 1.0, fov = 18 } = {}) {
  const renderer = new THREE.WebGLRenderer({ antialias: true, preserveDrawingBuffer: true, alpha: false });
  renderer.setPixelRatio(1);
  renderer.setSize(SIZE, SIZE);
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = exposure;
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFSoftShadowMap;
  document.body.appendChild(renderer.domElement);
  const scene = new THREE.Scene();
  scene.background = new THREE.Color(0x000000);
  const pmrem = new THREE.PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environmentIntensity = envIntensity;
  const d = 512 / Math.tan(THREE.MathUtils.degToRad(fov / 2));
  const camera = new THREE.PerspectiveCamera(fov, 1, 10, d * 3);
  camera.position.set(0, 0, d);
  camera.lookAt(0, 0, 0);
  return { renderer, scene, camera };
}

export function squircleShape(half, n = 5, pts = 256) {
  const s = new THREE.Shape();
  for (let i = 0; i <= pts; i++) {
    const t = (i / pts) * Math.PI * 2, c = Math.cos(t), si = Math.sin(t);
    const x = half * Math.sign(c) * Math.pow(Math.abs(c), 2 / n);
    const y = half * Math.sign(si) * Math.pow(Math.abs(si), 2 / n);
    i === 0 ? s.moveTo(x, y) : s.lineTo(x, y);
  }
  return s;
}

// vertical (optionally diagonal) gradient texture for the body face
export function gradientTexture(stops, { angle = 90, radial = null } = {}) {
  const c = document.createElement('canvas'); c.width = c.height = 1024;
  const g = c.getContext('2d');
  let grad;
  if (radial) grad = g.createRadialGradient(radial[0] * 1024, radial[1] * 1024, 0, radial[0] * 1024, radial[1] * 1024, radial[2] * 1024);
  else {
    const a = THREE.MathUtils.degToRad(angle), cx = 512, cy = 512, r = 724;
    grad = g.createLinearGradient(cx - Math.cos(a) * r, cy - Math.sin(a) * r, cx + Math.cos(a) * r, cy + Math.sin(a) * r);
  }
  stops.forEach(([o, col]) => grad.addColorStop(o, col));
  g.fillStyle = grad; g.fillRect(0, 0, 1024, 1024);
  const t = new THREE.CanvasTexture(c); t.colorSpace = THREE.SRGBColorSpace; return t;
}

// the icon body: an extruded, beveled squircle whose front face sits at z = 0
export function body(scene, { map, color = 0xffffff, roughness = 0.35, clearcoat = 1, metalness = 0, depth = 60, bevel = 26 } = {}) {
  const shape = squircleShape(BODY_HALF - bevel);
  const geo = new THREE.ExtrudeGeometry(shape, { depth, bevelEnabled: true, bevelThickness: bevel, bevelSize: bevel, bevelSegments: 12, curveSegments: 256 });
  // planar UVs over the full face so the gradient spans the icon
  const pos = geo.attributes.position, uv = geo.attributes.uv;
  for (let i = 0; i < pos.count; i++) uv.setXY(i, pos.getX(i) / (2 * BODY_HALF) + 0.5, pos.getY(i) / (2 * BODY_HALF) + 0.5);
  const mat = new THREE.MeshPhysicalMaterial({ map, color, roughness, metalness, clearcoat, clearcoatRoughness: 0.12 });
  const m = new THREE.Mesh(geo, mat);
  m.position.z = -depth - bevel;
  m.receiveShadow = true;
  scene.add(m);
  return m;
}

// ">_" prompt glyph as extruded shapes, centred on its own origin; s = cap height in px
export function promptGlyph(s = 120, mat, { depth = 18, bevel = 5, stroke = 0.2 } = {}) {
  const g = new THREE.Group();
  const w = s * stroke;
  const ch = new THREE.Shape();
  const hx = s * 0.42, hy = s * 0.5;
  // thick chevron outline
  ch.moveTo(-hx, hy); ch.lineTo(-hx + w * 1.15, hy); ch.lineTo(hx, 0); ch.lineTo(-hx + w * 1.15, -hy);
  ch.lineTo(-hx, -hy); ch.lineTo(hx - w * 1.15, 0); ch.closePath();
  const opts = { depth, bevelEnabled: true, bevelThickness: bevel, bevelSize: bevel * 0.8, bevelSegments: 6 };
  const chev = new THREE.Mesh(new THREE.ExtrudeGeometry(ch, opts), mat);
  chev.position.x = -s * 0.42;
  const us = new THREE.Mesh(new RoundedBoxGeometry(s * 0.62, w * 0.95, depth + bevel * 2, 4, Math.min(w * 0.45, bevel * 1.6)), mat);
  us.position.set(s * 0.5, -hy + w * 0.48, (depth) / 2);
  g.add(chev, us);
  g.traverse(o => { if (o.isMesh) o.castShadow = true; });
  return g;
}

export function composer(renderer, scene, camera, { strength = 0.6, radius = 0.5, threshold = 0.85 } = {}) {
  const c = new EffectComposer(renderer);
  c.setPixelRatio(1); c.setSize(SIZE, SIZE);
  c.addPass(new RenderPass(scene, camera));
  const bloom = new UnrealBloomPass(new THREE.Vector2(SIZE, SIZE), strength, radius, threshold);
  c.addPass(bloom);
  c.addPass(new OutputPass());
  return { c, bloom };
}

export function run(fn, frames = 6) {
  let i = 0;
  const tick = () => { fn(i); i++; if (i < frames) requestAnimationFrame(tick); else { window.__ready = true; console.log('ready'); } };
  requestAnimationFrame(tick);
}

// studio environment with coloured softboxes, for rich reflections on glass and clearcoat
export function studioEnv(renderer, panels = [
  { c: '#ffffff', i: 6, p: [0, 9, 4], s: [8, 0.6, 6] },
  { c: '#ff7ad9', i: 4, p: [9, 1, 2], s: [0.6, 6, 4] },
  { c: '#6fd3ff', i: 4, p: [-9, 1, 2], s: [0.6, 6, 4] },
  { c: '#ffd27a', i: 2.5, p: [0, -9, 3], s: [6, 0.6, 3] },
  { c: '#ffffff', i: 2, p: [0, 2, 10], s: [6, 4, 0.6] },
]) {
  const env = new THREE.Scene();
  const room = new THREE.Mesh(new THREE.BoxGeometry(20, 20, 20), new THREE.MeshBasicMaterial({ color: 0x07070c, side: THREE.BackSide }));
  env.add(room);
  for (const q of panels) {
    const m = new THREE.Mesh(new THREE.BoxGeometry(...q.s), new THREE.MeshBasicMaterial({ color: new THREE.Color(q.c).multiplyScalar(q.i) }));
    m.position.set(...q.p); env.add(m);
  }
  const pm = new THREE.PMREMGenerator(renderer);
  return pm.fromScene(env, 0.02).texture;
}
