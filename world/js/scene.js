// Renderer, light, sky and the static campus. Lighting is a single warm
// late-afternoon sun with a fitted shadow frustum, a sky/ground hemisphere,
// a low-intensity prefiltered environment for bronze and marble, and haze
// that matches the horizon so the distance dissolves instead of ending.

import * as THREE from '../vendor/three.module.min.js';
import {
  EffectComposer,
  GTAOPass,
  OutputPass,
  RenderPass,
  RoomEnvironment,
  UnrealBloomPass,
} from '../vendor/three.module.min.js';
import {
  LAYOUT,
  buildCensors,
  buildCuria,
  buildGround,
  buildHills,
  buildLegionHall,
  buildMosaic,
  buildTabularium,
  buildVegetation,
} from './architecture.js';
import { drawAquila } from './aquila.js';
import { PALETTE, buildMaterials } from './materials.js';
import { makeBanner, makeCampaignBoard, makeCensorTable, makeLamp, makeShelf, makeTabletRack, makeWorkstation } from './props.js';
import { makeCanvasTexture } from './textures.js';
import { bake, box, columnGeometry } from './architecture.js';

export const QUALITY = {
  low: { pixelRatio: 1, shadow: 1024, ao: false, bloom: false, antialias: false },
  medium: { pixelRatio: 1.5, shadow: 2048, ao: false, bloom: true, antialias: true },
  high: { pixelRatio: 2, shadow: 2048, ao: true, bloom: true, antialias: true },
};

export function pickQuality() {
  const param = new URLSearchParams(location.search).get('quality');
  if (param && QUALITY[param]) return param;
  const cores = navigator.hardwareConcurrency || 4;
  const mobile = /Mobi|Android/i.test(navigator.userAgent);
  if (mobile || cores <= 4) return 'low';
  return 'high';
}

function skyDome() {
  const g = new THREE.SphereGeometry(400, 32, 16);
  const m = new THREE.ShaderMaterial({
    side: THREE.BackSide,
    depthWrite: false,
    fog: false,
    uniforms: {
      top: { value: new THREE.Color('#8fb1cf') },
      horizon: { value: new THREE.Color('#f0dcbc') },
      sunDir: { value: new THREE.Vector3() },
    },
    vertexShader: `varying vec3 vDir; void main(){ vDir = normalize(position); gl_Position = projectionMatrix * modelViewMatrix * vec4(position,1.0); }`,
    fragmentShader: `uniform vec3 top; uniform vec3 horizon; uniform vec3 sunDir; varying vec3 vDir;
      void main(){
        float h = clamp(vDir.y, 0.0, 1.0);
        vec3 c = mix(horizon, top, pow(h, 0.55));
        float s = max(dot(normalize(vDir), normalize(sunDir)), 0.0);
        c += vec3(1.0, 0.8, 0.55) * (pow(s, 12.0) * 0.35 + pow(s, 400.0) * 1.5);
        gl_FragColor = vec4(c, 1.0);
        #include <colorspace_fragment>
      }`,
  });
  return new THREE.Mesh(g, m);
}

function mosaicTexture() {
  return makeCanvasTexture(1024, 1024, (ctx, W) => {
    // Dark marble ground with a tessera grain.
    ctx.fillStyle = '#3b312a';
    ctx.fillRect(0, 0, W, W);
    for (let y = 0; y < W; y += 8) {
      for (let x = 0; x < W; x += 8) {
        const v = 40 + ((x * 13 + y * 7) % 23);
        ctx.fillStyle = `rgb(${v + 14},${v + 6},${v})`;
        ctx.fillRect(x + 0.5, y + 0.5, 7, 7);
      }
    }
    ctx.strokeStyle = '#b0843f';
    ctx.lineWidth = 14;
    ctx.beginPath();
    ctx.arc(W / 2, W / 2, W / 2 - 40, 0, Math.PI * 2);
    ctx.stroke();
    ctx.lineWidth = 4;
    ctx.beginPath();
    ctx.arc(W / 2, W / 2, W / 2 - 70, 0, Math.PI * 2);
    ctx.stroke();
    drawAquila(ctx, 212, 190, 600, '#c99a4b', '#3b312a');
    ctx.fillStyle = '#c99a4b';
    ctx.font = '600 52px Georgia, serif';
    ctx.textAlign = 'center';
    ctx.fillText('S · P · Q · R', W / 2, W - 150);
  });
}

export function createWorld(canvas, qualityName) {
  const Q = QUALITY[qualityName];
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: Q.antialias, powerPreference: 'high-performance' });
  renderer.setPixelRatio(Math.min(window.devicePixelRatio, Q.pixelRatio));
  renderer.setSize(window.innerWidth, window.innerHeight);
  renderer.shadowMap.enabled = true;
  renderer.shadowMap.type = THREE.PCFShadowMap;
  renderer.toneMapping = THREE.ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.0;
  renderer.outputColorSpace = THREE.SRGBColorSpace;
  renderer.info.autoReset = false;

  const scene = new THREE.Scene();
  const fogColor = new THREE.Color('#e6d3b3');
  scene.fog = new THREE.FogExp2(fogColor, 0.0024);
  scene.background = fogColor;

  const camera = new THREE.PerspectiveCamera(38, window.innerWidth / window.innerHeight, 0.3, 900);

  // Light.
  const sunDir = new THREE.Vector3(-0.55, 0.62, 0.56).normalize();
  const sun = new THREE.DirectionalLight('#ffe0b0', 3.6);
  sun.position.copy(sunDir).multiplyScalar(90);
  sun.castShadow = true;
  sun.shadow.mapSize.set(Q.shadow, Q.shadow);
  const sc = sun.shadow.camera;
  sc.left = -58;
  sc.right = 58;
  sc.top = 48;
  sc.bottom = -48;
  sc.near = 20;
  sc.far = 220;
  sun.shadow.bias = -0.0004;
  sun.shadow.normalBias = 0.04;
  sun.shadow.radius = 3;
  scene.add(sun, sun.target);
  const hemi = new THREE.HemisphereLight('#c3d6ea', '#7a6448', 0.6);
  scene.add(hemi);

  const pmrem = new THREE.PMREMGenerator(renderer);
  scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
  scene.environmentIntensity = 0.35;

  const sky = skyDome();
  sky.material.uniforms.sunDir.value.copy(sunDir);
  scene.add(sky);

  const M = buildMaterials();
  const uniforms = { uTime: { value: 0 } };

  buildGround(scene, M);
  buildMosaic(scene, mosaicTexture());
  buildHills(scene);
  const legion = buildLegionHall(scene, M);
  const censors = buildCensors(scene, M);
  const curia = buildCuria(scene, M);
  buildTabularium(scene, M);
  buildVegetation(scene, M);

  // Campaign board at the head of the Forum, flanked by honorific columns
  // bearing the Aquila banners.
  const board = makeCampaignBoard(M);
  board.group.position.set(LAYOUT.board.x, 0, LAYOUT.board.z);
  bake(board.group, [board.face]);
  scene.add(board.group);
  const rack = makeTabletRack(M);
  rack.group.position.set(LAYOUT.board.x + 5.2, 0, LAYOUT.board.z + 0.8);
  rack.group.rotation.y = -0.35;
  scene.add(rack.group);
  const tall = columnGeometry(7.5, 0.36);
  for (const dx of [-5.6, 5.6]) {
    const c = new THREE.Mesh(tall, M.marble);
    c.castShadow = c.receiveShadow = true;
    c.position.set(LAYOUT.board.x + dx, 0.36, LAYOUT.board.z - 0.6);
    scene.add(c);
    const banner = makeBanner(M, uniforms);
    banner.position.set(LAYOUT.board.x + dx, 5.4, LAYOUT.board.z - 0.1);
    scene.add(banner);
    const pole = new THREE.Mesh(new THREE.CylinderGeometry(0.04, 0.04, 1.8, 6), M.bronze);
    pole.rotation.z = Math.PI / 2;
    pole.position.set(LAYOUT.board.x + dx, 6.85, LAYOUT.board.z - 0.1);
    scene.add(pole);
  }

  // Legion Hall workstations: six places in two rows in the open courtyard.
  const desks = [];
  const L = legion.courtyard;
  // Two columns of three; Cohorts face west, so from the Forum side you see
  // each worker's shoulder and the glow of the tablet-terminal in front.
  const deskSpots = [];
  for (const dx of [-3.4, 3.4]) for (const dz of [-4.2, 0, 4.2]) deskSpots.push([L.x + dx, L.z + dz]);
  for (const [x, z] of deskSpots) {
    const ws = makeWorkstation(M);
    ws.group.position.set(x, 0.3, z);
    ws.group.rotation.y = Math.PI / 2;
    ws.group.updateMatrixWorld(true);
    bake(ws.group, [ws.screen, ws.tablets, ws.orderTablet, ws.seal, ws.standard, ...ws.lamps]);
    scene.add(ws.group);
    desks.push(ws);
  }
  // Scroll shelves along the roofed north and west walks. Decoration only.
  const LL = LAYOUT.legion;
  for (let i = 0; i < 4; i++) {
    const shelf = makeShelf(M);
    shelf.position.set(LL.x - 6 + i * 4.6, 0.3, LL.z - LL.d / 2 + 0.75);
    scene.add(bake(shelf));
  }
  for (let i = 0; i < 3; i++) {
    const shelf = makeShelf(M);
    shelf.position.set(LL.x - LL.w / 2 + 0.75, 0.3, LL.z - 2 + i * 4.6);
    shelf.rotation.y = Math.PI / 2;
    scene.add(bake(shelf));
  }
  // Censors' table.
  const censorTable = makeCensorTable(M);
  censorTable.group.position.copy(censors.seat).add(new THREE.Vector3(-1.1, 0, 0));
  bake(censorTable.group, [censorTable.submitted, censorTable.wax, ...censorTable.flames]);
  scene.add(censorTable.group);

  // Oil lamps at the Legion Hall entrance and in the Forum; lit, not
  // animated, except for the flame's soft breathing.
  const lamps = [];
  for (const [x, z] of [
    [LAYOUT.legion.x + 12.9, LAYOUT.legion.z - 2.8],
    [LAYOUT.legion.x + 12.9, LAYOUT.legion.z + 2.8],
    [LAYOUT.censors.x - 3.4, -5],
    [LAYOUT.censors.x - 3.4, 5],
  ]) {
    const l = makeLamp(M);
    l.group.position.set(x, 0.3, z);
    bake(l.group, [l.flame]);
    scene.add(l.group);
    lamps.push(l);
  }
  // Seal-red hangings on the exedra wall, behind the Censor's seat.
  for (const dz of [-2.4, 2.4]) {
    const hang = new THREE.Mesh(new THREE.PlaneGeometry(1.3, 3.6), M.redCloth);
    const a = Math.asin(dz / 6.5);
    hang.position.set(LAYOUT.censors.x + Math.cos(a) * 6.45, 3.0, LAYOUT.censors.z + Math.sin(a) * 6.45);
    hang.rotation.y = -Math.PI / 2 - a;
    hang.receiveShadow = true;
    scene.add(hang);
    const rod = new THREE.Mesh(new THREE.CylinderGeometry(0.04, 0.04, 1.6, 6), M.bronze);
    rod.rotation.x = Math.PI / 2;
    rod.rotation.y = -a;
    rod.position.set(LAYOUT.censors.x + Math.cos(a) * 6.4, 4.85, LAYOUT.censors.z + Math.sin(a) * 6.4);
    scene.add(rod);
  }
  const censorGlow = new THREE.PointLight('#ffb766', 0, 9, 1.6);
  censorGlow.position.copy(censors.seat).add(new THREE.Vector3(-0.8, 2.2, 0));
  scene.add(censorGlow);

  // Post-processing.
  const composer = new EffectComposer(renderer);
  composer.addPass(new RenderPass(scene, camera));
  let gtao = null;
  if (Q.ao) {
    gtao = new GTAOPass(scene, camera, window.innerWidth, window.innerHeight);
    gtao.blendIntensity = 0.7;
    gtao.updateGtaoMaterial({ radius: 0.6, distanceExponent: 1.4, thickness: 1.2, scale: 1 });
    composer.addPass(gtao);
  }
  if (Q.bloom) {
    composer.addPass(new UnrealBloomPass(new THREE.Vector2(window.innerWidth, window.innerHeight), 0.45, 0.55, 2.8));
  }
  composer.addPass(new OutputPass());

  function resize() {
    camera.aspect = window.innerWidth / window.innerHeight;
    camera.updateProjectionMatrix();
    renderer.setSize(window.innerWidth, window.innerHeight);
    composer.setSize(window.innerWidth, window.innerHeight);
  }
  window.addEventListener('resize', resize);

  return {
    renderer,
    scene,
    camera,
    composer,
    sun,
    uniforms,
    materials: M,
    board,
    rack,
    desks,
    legion,
    censors,
    censorTable,
    censorGlow,
    curia,
    lamps,
    palette: PALETTE,
    quality: qualityName,
  };
}
