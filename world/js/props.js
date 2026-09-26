// Authored props: workstations, the campaign board, banners, the Censor's
// table, lamps. Props that show state expose small handles (screen, lamps,
// seal, tablets) that the director toggles; none animate on their own.

import * as THREE from '../vendor/three.module.min.js';
import { box } from './architecture.js';
import { drawAquila } from './aquila.js';
import { PALETTE, std } from './materials.js';

function mesh(g, m, { cast = true, receive = true } = {}) {
  const x = new THREE.Mesh(g, m);
  x.castShadow = cast;
  x.receiveShadow = receive;
  return x;
}

// ---------------------------------------------------------------------------
// Screens: a few shared animated canvases, one per kind of activity, so ten
// desks cost the same as one.

class ScreenFeed {
  constructor(kind) {
    this.kind = kind;
    this.canvas = document.createElement('canvas');
    this.canvas.width = 256;
    this.canvas.height = 160;
    this.ctx = this.canvas.getContext('2d');
    this.texture = new THREE.CanvasTexture(this.canvas);
    this.texture.colorSpace = THREE.SRGBColorSpace;
    this.lines = [];
    this.tick = 0;
    this.seed = 7;
    for (let i = 0; i < 14; i++) this.lines.push(this.newLine());
    this.draw();
  }

  rand() {
    this.seed = (this.seed * 16807) % 2147483647;
    return this.seed / 2147483647;
  }

  newLine() {
    const indent = Math.floor(this.rand() * 4) * 12;
    const segs = [];
    let x = indent;
    const n = 1 + Math.floor(this.rand() * 4);
    for (let i = 0; i < n; i++) {
      const w = 10 + this.rand() * 50;
      segs.push([x, w, this.rand()]);
      x += w + 6;
    }
    return segs;
  }

  draw() {
    const { ctx } = this;
    const W = 256;
    const H = 160;
    ctx.fillStyle = '#2a1a0e';
    ctx.fillRect(0, 0, W, H);
    const g = ctx.createRadialGradient(W / 2, H / 2, 10, W / 2, H / 2, 160);
    g.addColorStop(0, 'rgba(255,190,110,0.22)');
    g.addColorStop(1, 'rgba(0,0,0,0)');
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, W, H);
    if (this.kind === 'code') {
      this.lines.forEach((segs, row) => {
        for (const [x, w, hue] of segs) {
          ctx.fillStyle = hue > 0.7 ? '#ffd79a' : hue > 0.35 ? '#f0a860' : '#c9855a';
          ctx.fillRect(14 + x, 10 + row * 10.5, w, 5);
        }
      });
      // Cursor.
      if (this.tick % 2 === 0) {
        ctx.fillStyle = '#fff1d6';
        ctx.fillRect(14 + (this.lines[13].at(-1)?.[0] ?? 0) + 40, 10 + 13 * 10.5, 6, 7);
      }
    } else if (this.kind === 'read') {
      // Paragraph blocks with a highlight band moving down.
      for (let row = 0; row < 12; row++) {
        const w = row % 4 === 3 ? 120 : 220;
        ctx.fillStyle = row === this.tick % 12 ? '#ffe0ae' : '#b98556';
        ctx.fillRect(18, 12 + row * 11.5, w, 4);
      }
    } else if (this.kind === 'test') {
      for (let row = 0; row < 9; row++) {
        const done = row < this.tick % 12;
        ctx.fillStyle = done ? '#e7c46a' : '#6a4a30';
        ctx.fillRect(20, 14 + row * 15, 8, 8);
        ctx.fillStyle = done ? '#e9c898' : '#8a6040';
        ctx.fillRect(38, 16 + row * 15, 90 + ((row * 37) % 80), 4);
      }
    } else if (this.kind === 'halt') {
      ctx.fillStyle = '#b3342a';
      ctx.fillRect(0, 0, W, 8);
      ctx.fillRect(0, H - 8, W, 8);
      ctx.fillStyle = '#f3d7b0';
      ctx.font = 'bold 34px Georgia, serif';
      ctx.textAlign = 'center';
      ctx.fillText('?', W / 2, H / 2 + 12);
    } else {
      ctx.fillStyle = 'rgba(0,0,0,0.5)';
      ctx.fillRect(0, 0, W, H);
    }
    this.texture.needsUpdate = true;
  }

  step() {
    this.tick += 1;
    if (this.kind === 'code') {
      this.lines.shift();
      this.lines.push(this.newLine());
    }
    this.draw();
  }
}

export const FEEDS = {
  code: new ScreenFeed('code'),
  read: new ScreenFeed('read'),
  test: new ScreenFeed('test'),
  halt: new ScreenFeed('halt'),
  off: new ScreenFeed('off'),
};

let feedClock = 0;
export function stepFeeds(dt) {
  feedClock += dt;
  if (feedClock < 0.14) return;
  feedClock = 0;
  for (const feed of Object.values(FEEDS)) if (feed.kind !== 'off') feed.step();
}

const screenMats = new Map();
function screenMaterial(kind) {
  if (!screenMats.has(kind)) {
    const tex = FEEDS[kind].texture;
    screenMats.set(
      kind,
      new THREE.MeshStandardMaterial({
        map: tex,
        emissiveMap: tex,
        emissive: new THREE.Color('#ffffff'),
        emissiveIntensity: kind === 'off' ? 0 : 4.5,
        roughness: 0.4,
      }),
    );
  }
  return screenMats.get(kind);
}

// ---------------------------------------------------------------------------

// One Cohort workstation: desk, stool, a bronze-framed tablet-terminal, two
// reading tablets, the verification apparatus (a frame of lamps that light
// in sequence while the repository's checks run), and a red wax seal that
// stands up when the Order needs a person.
export function makeWorkstation(M) {
  const g = new THREE.Group();
  g.name = 'workstation';
  // Desk: thick top on two stone trestles.
  const top = mesh(box(2.3, 0.12, 1.05, 1.5), M.wood);
  top.position.y = 0.86;
  g.add(top);
  for (const x of [-0.9, 0.9]) {
    const leg = mesh(box(0.18, 0.8, 0.85, 1), M.stone);
    leg.position.set(x, 0.4, 0);
    g.add(leg);
  }
  // Stool behind the desk (worker sits at +z, facing -z).
  const stool = new THREE.Group();
  const seatTop = mesh(box(0.5, 0.07, 0.42, 1), M.wood);
  seatTop.position.y = 0.4;
  stool.add(seatTop);
  for (const [x, z] of [[-0.2, -0.16], [0.2, -0.16], [-0.2, 0.16], [0.2, 0.16]]) {
    const leg = mesh(box(0.05, 0.38, 0.05, 1), M.darkWood);
    leg.position.set(x, 0.19, z);
    stool.add(leg);
  }
  stool.position.set(0, 0, 0.72);
  g.add(stool);

  // Tablet-terminal: bronze frame, glowing face toward the worker.
  const frame = mesh(box(0.95, 0.66, 0.06, 1), M.bronze);
  frame.position.set(0, 1.26, -0.28);
  frame.rotation.x = -0.12;
  g.add(frame);
  const screen = mesh(new THREE.PlaneGeometry(0.82, 0.54), screenMaterial('off'), { cast: false });
  screen.position.set(0, 1.26, -0.245);
  screen.rotation.x = -0.12;
  g.add(screen);
  const stand = mesh(box(0.12, 0.34, 0.1, 1), M.bronze);
  stand.position.set(0, 1.0, -0.3);
  g.add(stand);

  // Reading tablets: wax tablets in wooden frames.
  const tablets = new THREE.Group();
  for (const [x, r] of [[-0.72, 0.25], [0.7, -0.2]]) {
    const t = mesh(box(0.42, 0.03, 0.3, 1), M.parchment);
    t.position.set(x, 0.935, 0.05);
    t.rotation.y = r;
    const rim = mesh(box(0.46, 0.025, 0.34, 1), M.darkWood);
    rim.position.set(x, 0.925, 0.05);
    rim.rotation.y = r;
    tablets.add(t, rim);
  }
  tablets.visible = false;
  g.add(tablets);

  // The Order's own tablet (what gets sealed and carried to the Censor).
  const orderTablet = mesh(box(0.36, 0.05, 0.26, 1), M.parchment);
  orderTablet.position.set(-0.35, 0.95, 0.12);
  g.add(orderTablet);

  // Verification apparatus: a small bronze arch on the desk corner with
  // five lamps that light in sequence while the repository's checks run.
  const app = new THREE.Group();
  app.position.set(0.85, 0.92, -0.25);
  const arch = mesh(new THREE.TorusGeometry(0.2, 0.025, 6, 16, Math.PI), M.bronze);
  arch.position.y = 0.02;
  app.add(arch);
  const lamps = [];
  for (let i = 0; i < 5; i++) {
    const a = Math.PI * (0.1 + (0.8 * i) / 4);
    const lm = new THREE.MeshStandardMaterial({ color: '#4b3a26', emissive: new THREE.Color(PALETTE.lamp), emissiveIntensity: 0, roughness: 0.3 });
    const l = mesh(new THREE.SphereGeometry(0.035, 8, 6), lm, { cast: false });
    l.position.set(Math.cos(a) * 0.2, 0.02 + Math.sin(a) * 0.2, 0);
    app.add(l);
    lamps.push(l);
  }
  g.add(app);

  // Needs-you seal: a red wax disc standing on the desk front.
  const seal = new THREE.Group();
  const disc = mesh(new THREE.CylinderGeometry(0.2, 0.2, 0.06, 20), std({ color: PALETTE.seal, roughness: 0.5 }));
  disc.rotation.x = Math.PI / 2;
  disc.position.set(0, 1.25, 0.56);
  seal.add(disc);
  seal.visible = false;
  g.add(seal);

  // The Cohort's standard (vexillum): a square cloth on a crossbar, high
  // enough to find from the overview. It stands only while a real Cohort
  // holds the desk; it turns seal-red with a "!" when the Order needs you.
  const standard = new THREE.Group();
  const staff = mesh(new THREE.CylinderGeometry(0.035, 0.035, 4.2, 6), M.darkWood);
  staff.position.set(-1.3, 2.1, 0.55);
  const crossbar = mesh(new THREE.CylinderGeometry(0.03, 0.03, 1.1, 6), M.bronze);
  crossbar.rotation.x = Math.PI / 2;
  crossbar.position.set(-1.3, 4.05, 0.55);
  const flagCanvas = document.createElement('canvas');
  flagCanvas.width = 128;
  flagCanvas.height = 128;
  const flagTex = new THREE.CanvasTexture(flagCanvas);
  flagTex.colorSpace = THREE.SRGBColorSpace;
  const flag = mesh(new THREE.PlaneGeometry(1.0, 1.0, 4, 4), new THREE.MeshStandardMaterial({ map: flagTex, roughness: 0.9, side: THREE.DoubleSide }), { receive: false });
  flag.position.set(-1.3, 3.5, 0.55);
  flag.rotation.y = Math.PI / 2;
  const tip = mesh(new THREE.ConeGeometry(0.06, 0.2, 6), M.gold);
  tip.position.set(-1.3, 4.3, 0.55);
  standard.add(staff, crossbar, flag, tip);
  standard.visible = false;
  g.add(standard);
  function drawFlag(numeral, halt) {
    const c = flagCanvas.getContext('2d');
    c.fillStyle = halt ? PALETTE.seal : '#e9dcbf';
    c.fillRect(0, 0, 128, 128);
    c.strokeStyle = halt ? '#f3d7b0' : PALETTE.pompeian;
    c.lineWidth = 6;
    c.strokeRect(8, 8, 112, 112);
    c.fillStyle = halt ? '#fff1dc' : PALETTE.pompeian;
    c.textAlign = 'center';
    c.font = '600 54px Georgia, serif';
    c.fillText(halt ? `${numeral}!` : numeral, 64, 84);
    flagTex.needsUpdate = true;
  }

  // Plaque on the desk front: short identity, also shown as a label.
  const plaque = mesh(box(0.9, 0.2, 0.03, 1), M.bronze);
  plaque.position.set(0, 0.72, -0.54);
  g.add(plaque);

  return {
    group: g,
    screen,
    tablets,
    orderTablet,
    lamps,
    seal,
    standard,
    flag,
    setStandard(numeral, halt) {
      if (!numeral) {
        standard.visible = false;
        return;
      }
      standard.visible = true;
      drawFlag(numeral, halt);
    },
    seatPosition: new THREE.Vector3(0, 0.02, 0.72),
    setScreen(kind) {
      screen.material = screenMaterial(kind);
    },
  };
}

// The campaign board: a bronze-framed tablet on a plinth. It shows only what
// a glance needs — title, settled count, the current Orders — and is redrawn
// from the projection whenever it changes.
export function makeCampaignBoard(M) {
  const g = new THREE.Group();
  const plinth = mesh(box(6.2, 1.1, 1.2, 2), M.marble);
  plinth.position.y = 0.55 + 0.36;
  g.add(plinth);
  const frame = mesh(box(5.6, 3.4, 0.3, 2), M.bronze);
  frame.position.y = 1.46 + 1.7;
  g.add(frame);
  const canvas = document.createElement('canvas');
  canvas.width = 1024;
  canvas.height = 600;
  const tex = new THREE.CanvasTexture(canvas);
  tex.colorSpace = THREE.SRGBColorSpace;
  tex.anisotropy = 8;
  const face = mesh(new THREE.PlaneGeometry(5.2, 3.05), new THREE.MeshStandardMaterial({ map: tex, roughness: 0.9, color: '#d8cbb0' }), { cast: false });
  face.position.set(0, 3.16, 0.16);
  g.add(face);
  // Crest: small aquila casting on top.
  const crest = mesh(box(1.4, 0.3, 0.4, 1), M.bronze);
  crest.position.set(0, 5.0, 0);
  g.add(crest);

  function draw(state) {
    const ctx = canvas.getContext('2d');
    const W = canvas.width;
    const H = canvas.height;
    ctx.fillStyle = '#efe2c2';
    ctx.fillRect(0, 0, W, H);
    ctx.fillStyle = 'rgba(120,90,50,0.07)';
    for (let i = 0; i < 60; i++) ctx.fillRect((i * 97) % W, (i * 61) % H, 140, 2);
    ctx.strokeStyle = '#8a6a3a';
    ctx.lineWidth = 6;
    ctx.strokeRect(18, 18, W - 36, H - 36);
    ctx.fillStyle = '#2a211b';
    ctx.textBaseline = 'alphabetic';
    const c = state.campaign;
    if (!c) {
      ctx.textAlign = 'center';
      ctx.font = '600 64px Georgia, "Times New Roman", serif';
      ctx.fillText('NO CAMPAIGN', W / 2, H / 2);
      ctx.font = 'italic 34px Georgia, serif';
      ctx.fillText('The Senate is at rest.', W / 2, H / 2 + 56);
      tex.needsUpdate = true;
      return;
    }
    ctx.textAlign = 'left';
    ctx.font = '600 30px Georgia, serif';
    ctx.fillStyle = '#8e2b22';
    ctx.fillText('CAMPAIGN', 60, 82);
    ctx.fillStyle = '#2a211b';
    ctx.font = '600 64px Georgia, serif';
    ctx.fillText(c.title.toUpperCase().slice(0, 26), 60, 150);
    ctx.font = '36px Georgia, serif';
    ctx.fillText(`${c.settled} of ${c.total} Orders settled`, 60, 206);
    // Progress bar made of tablets.
    const n = Math.max(c.total, 1);
    const bw = Math.min(64, (W - 140) / n - 8);
    for (let i = 0; i < n; i++) {
      ctx.fillStyle = i < c.settled ? '#b0843f' : 'rgba(42,33,27,0.14)';
      ctx.fillRect(60 + i * (bw + 8), 232, bw, 22);
    }
    ctx.font = '30px Georgia, serif';
    let y = 310;
    const orders = state.orders.filter((o) => o.state !== 'settled' && o.state !== 'cancelled').slice(0, 5);
    for (const o of orders) {
      const glyph = { working: '●', in_review: '◆', blocked: '!', delivered: '✓', failed: '✕', ready: '○', planned: '○' }[o.state] || '○';
      ctx.fillStyle = o.state === 'blocked' || o.state === 'failed' ? '#a3261f' : '#2a211b';
      ctx.fillText(`${glyph}  ${o.title.slice(0, 34)}`, 70, y);
      ctx.fillStyle = '#6b5a45';
      ctx.textAlign = 'right';
      ctx.fillText(STATE_WORD[o.state] || o.state, W - 70, y);
      ctx.textAlign = 'left';
      y += 50;
    }
    if (c.needs_you > 0) {
      ctx.fillStyle = '#a3261f';
      ctx.font = '600 32px Georgia, serif';
      ctx.fillText(`${c.needs_you} ${c.needs_you === 1 ? 'matter needs' : 'matters need'} your judgment`, 60, H - 50);
    }
    tex.needsUpdate = true;
  }
  return { group: g, draw, face };
}

export const STATE_WORD = {
  planned: 'Planned',
  ready: 'Ready',
  working: 'Working',
  in_review: 'In review',
  blocked: 'Needs you',
  delivered: 'Awaiting integration',
  settled: 'Settled',
  failed: 'Failed',
  cancelled: 'Cancelled',
};

// Banner cloth with the Aquila, moving in a light breeze (vertex shader).
export function makeBanner(M, uniforms) {
  const canvas = document.createElement('canvas');
  canvas.width = 256;
  canvas.height = 512;
  const ctx = canvas.getContext('2d');
  ctx.fillStyle = PALETTE.pompeian;
  ctx.fillRect(0, 0, 256, 512);
  ctx.fillStyle = 'rgba(0,0,0,0.12)';
  for (let y = 0; y < 512; y += 4) ctx.fillRect(0, y, 256, 1);
  ctx.strokeStyle = '#d9ad5c';
  ctx.lineWidth = 6;
  ctx.strokeRect(14, 14, 228, 484);
  drawAquila(ctx, 28, 110, 200, '#d9ad5c', PALETTE.pompeian);
  ctx.fillStyle = '#d9ad5c';
  ctx.font = '600 34px Georgia, serif';
  ctx.textAlign = 'center';
  ctx.fillText('S·P·Q·R', 128, 400);
  const tex = new THREE.CanvasTexture(canvas);
  tex.colorSpace = THREE.SRGBColorSpace;
  const mat = new THREE.MeshStandardMaterial({ map: tex, roughness: 0.9, side: THREE.DoubleSide, normalMap: M.cloth });
  mat.onBeforeCompile = (shader) => {
    shader.uniforms.uTime = uniforms.uTime;
    shader.vertexShader = shader.vertexShader
      .replace('#include <common>', '#include <common>\nuniform float uTime;')
      .replace(
        '#include <begin_vertex>',
        `#include <begin_vertex>
        float hang = (0.5 - uv.y);
        float wave = sin(uv.y * 6.0 - uTime * 1.6 + position.x * 2.0) * 0.06 * (1.0 - uv.y);
        transformed.z += wave + hang * 0.02;`,
      );
  };
  const geo = new THREE.PlaneGeometry(1.4, 2.8, 6, 16);
  const banner = mesh(geo, mat, { receive: false });
  return banner;
}

// The Censor's long table, an inbox lectern at the front and two oil lamps.
export function makeCensorTable(M) {
  const g = new THREE.Group();
  const top = mesh(box(1.2, 0.12, 3.8, 1.5), M.darkWood);
  top.position.y = 0.9;
  g.add(top);
  for (const z of [-1.6, 1.6]) {
    const leg = mesh(box(1.0, 0.84, 0.14, 1), M.darkWood);
    leg.position.set(0, 0.42, z);
    g.add(leg);
  }
  const seat = mesh(box(0.6, 0.5, 0.7, 1), M.darkWood);
  seat.position.set(0.95, 0.25, 0);
  g.add(seat);
  // Plans laid out on the table.
  const sheets = [];
  for (const [z, r] of [[-1.2, 0.1], [1.15, -0.15], [0.0, 0.03]]) {
    const s = mesh(box(0.7, 0.012, 0.9, 1), M.parchment, { cast: false });
    s.position.set(0.05, 0.97, z);
    s.rotation.y = r;
    g.add(s);
    sheets.push(s);
  }
  // The submitted tablet, shown when a review is under way.
  const submitted = mesh(box(0.36, 0.05, 0.26, 1), M.parchment);
  submitted.position.set(0.1, 0.99, 0);
  submitted.visible = false;
  g.add(submitted);
  const wax = mesh(new THREE.CylinderGeometry(0.06, 0.06, 0.03, 12), std({ color: PALETTE.seal, roughness: 0.45 }));
  wax.position.set(0.1, 1.03, 0.08);
  wax.visible = false;
  g.add(wax);
  // Lamps.
  const flames = [];
  for (const z of [-2.2, 2.2]) {
    const stand = mesh(new THREE.CylinderGeometry(0.025, 0.06, 1.3, 8), M.bronze);
    stand.position.set(0.2, 0.65, z);
    g.add(stand);
    const bowl = mesh(new THREE.CylinderGeometry(0.2, 0.1, 0.12, 10), M.bronze);
    bowl.position.set(0.2, 1.34, z);
    g.add(bowl);
    const flame = new THREE.Mesh(new THREE.SphereGeometry(0.07, 8, 6), new THREE.MeshBasicMaterial({ color: new THREE.Color('#ffd08a').multiplyScalar(5) }));
    flame.scale.y = 1.8;
    flame.position.set(0.2, 1.47, z);
    g.add(flame);
    flames.push(flame);
  }
  return { group: g, submitted, wax, flames, sheets };
}

// A rack beside the board where sealed tablets wait for integration.
export function makeTabletRack(M) {
  const g = new THREE.Group();
  const base = mesh(box(2.4, 0.9, 0.7, 1.5), M.darkWood);
  base.position.y = 0.45 + 0.36;
  g.add(base);
  const slots = [];
  for (let i = 0; i < 5; i++) {
    const t = new THREE.Group();
    const tab = mesh(box(0.36, 0.26, 0.05, 1), M.parchment);
    const wax = mesh(new THREE.CylinderGeometry(0.05, 0.05, 0.02, 10), std({ color: PALETTE.seal, roughness: 0.45 }));
    wax.rotation.x = Math.PI / 2;
    wax.position.z = 0.035;
    t.add(tab, wax);
    t.position.set(-0.9 + i * 0.45, 1.4, 0.05);
    t.rotation.x = -0.25;
    t.visible = false;
    g.add(t);
    slots.push(t);
  }
  return { group: g, slots };
}

export function makeLamp(M) {
  const g = new THREE.Group();
  const stand = mesh(new THREE.CylinderGeometry(0.03, 0.07, 1.6, 8), M.bronze);
  stand.position.y = 0.8;
  const bowl = mesh(new THREE.CylinderGeometry(0.26, 0.12, 0.16, 12), M.bronze);
  bowl.position.y = 1.66;
  const flame = new THREE.Mesh(new THREE.SphereGeometry(0.09, 8, 6), new THREE.MeshBasicMaterial({ color: new THREE.Color('#ffcf85').multiplyScalar(5) }));
  flame.scale.y = 1.8;
  flame.position.y = 1.82;
  g.add(stand, bowl, flame);
  return { group: g, flame };
}

// Scroll shelf: a wooden case of pigeonholes with rolled scrolls.
export function makeShelf(M) {
  const g = new THREE.Group();
  const back = mesh(box(3.2, 2.4, 0.5, 1.5), M.darkWood);
  back.position.set(0, 1.2, -0.1);
  g.add(back);
  const scrollGeo = new THREE.CylinderGeometry(0.07, 0.07, 0.42, 8);
  scrollGeo.rotateX(Math.PI / 2);
  const scrolls = new THREE.InstancedMesh(scrollGeo, M.parchment, 36);
  const m = new THREE.Matrix4();
  let k = 0;
  for (let r = 0; r < 4; r++) {
    for (let c = 0; c < 9; c++) {
      const skip = (r * 7 + c * 3) % 5 === 0;
      m.makeTranslation(-1.35 + c * 0.34, 0.45 + r * 0.52, 0.2);
      if (skip) m.scale(new THREE.Vector3(0, 0, 0));
      scrolls.setMatrixAt(k++, m);
    }
  }
  scrolls.castShadow = true;
  g.add(scrolls);
  for (let r = 0; r < 5; r++) {
    const plank = mesh(box(3.3, 0.06, 0.55, 1.5), M.wood);
    plank.position.set(0, 0.3 + r * 0.52, 0.1);
    g.add(plank);
  }
  return g;
}
