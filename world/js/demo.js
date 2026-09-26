// Deterministic demonstration campaigns. They use the exact shape the live
// projection serves (`GET /api/world`, schema 1) so the world cannot tell
// them apart; they exist to test whether the spatial language reads before
// real state is wired, and to capture repeatable screenshots.

const T0 = Date.parse('2026-09-26T10:00:00Z');
const minutesAgo = (m) => new Date(T0 - m * 60000).toISOString();

function order(id, cohort, title, state, extra = {}) {
  return {
    id,
    cohort,
    title,
    state,
    activity: null,
    worker: null,
    since: null,
    reason: null,
    ...extra,
  };
}

const deepseek = { provider: 'deepseek', model: 'deepseek-v4' };
const codex = { provider: 'codex', model: 'gpt-5.5' };
const claude = { provider: 'claude', model: 'fable' };

function campaign(orders, extra = {}) {
  const settled = orders.filter((o) => o.state === 'settled').length;
  const active = orders.filter((o) => ['working', 'in_review', 'blocked'].includes(o.state)).length;
  const needs = orders.filter((o) => ['blocked', 'failed', 'delivered'].includes(o.state)).length;
  return {
    id: 'demo-living-characters',
    title: 'Living Characters',
    goal: 'Give every character a persistent memory and a decision policy that reacts to world events.',
    state: 'active',
    settled,
    total: orders.length,
    active,
    needs_you: needs,
    ...extra,
  };
}

function world(orders, { consul, censor, attention = [], campaignExtra } = {}) {
  return {
    schema: 1,
    source: 'demo',
    at: new Date(T0).toISOString(),
    campaign: campaign(orders, campaignExtra),
    consul,
    orders,
    censor: censor || { state: 'idle', order: null, reviewer: null },
    attention,
    curia: { state: 'closed' },
  };
}

const settledPair = [
  order('o-persist', 3, 'Save format', 'settled'),
  order('o-journal', 4, 'Event journal', 'settled'),
];

export const SCENARIOS = {
  // Nothing is running. The Senate is quiet, and that is the point.
  quiet: () =>
    world(
      [
        ...settledPair,
        order('o-memory', 1, 'Persistent memory', 'settled'),
        order('o-queue', 2, 'Event queue', 'settled'),
        order('o-policy', 5, 'Decision policy', 'planned'),
      ],
      {
        consul: {
          state: 'quiet',
          summary: ['Nothing is running.', '4 of 5 Orders are settled.', 'Decision policy is planned and waits for your word to start.'],
          lead_turns: 3,
        },
      },
    ),

  // One Cohort at work.
  one: () =>
    world(
      [
        ...settledPair,
        order('o-memory', 1, 'Persistent memory', 'working', { activity: 'coding', worker: deepseek, since: minutesAgo(6) }),
        order('o-queue', 2, 'Event queue', 'ready'),
        order('o-policy', 5, 'Decision policy', 'planned'),
      ],
      {
        consul: {
          state: 'quiet',
          summary: ['Cohort I is writing Persistent memory.', 'Event queue is ready to start.', 'Nothing needs your judgment.'],
          lead_turns: 3,
        },
      },
    ),

  // The brief's reference state: A working, B under independent review.
  review: () =>
    world(
      [
        ...settledPair,
        order('o-memory', 1, 'Persistent memory', 'working', { activity: 'coding', worker: deepseek, since: minutesAgo(6) }),
        order('o-queue', 2, 'Event queue', 'in_review', { activity: 'reviewing', worker: codex, since: minutesAgo(21) }),
        order('o-policy', 5, 'Decision policy', 'planned'),
      ],
      {
        consul: {
          state: 'quiet',
          summary: ['Cohort I is writing Persistent memory.', 'The Censor is reviewing Event queue.', 'Nothing needs your judgment.'],
          lead_turns: 3,
        },
        censor: { state: 'reviewing', order: 'o-queue', reviewer: claude },
      },
    ),

  // Tests running on one desk, a stop on another.
  blocked: () =>
    world(
      [
        ...settledPair,
        order('o-memory', 1, 'Persistent memory', 'working', { activity: 'testing', worker: deepseek, since: minutesAgo(14) }),
        order('o-queue', 2, 'Event queue', 'blocked', {
          activity: 'coding',
          worker: codex,
          since: minutesAgo(9),
          reason: 'Asks permission to run `cargo add tokio`.',
        }),
        order('o-policy', 5, 'Decision policy', 'planned'),
      ],
      {
        consul: {
          state: 'awaiting_you',
          summary: ['Event queue has stopped: it asks permission to run `cargo add tokio`.', 'Cohort I is running the checks for Persistent memory.'],
          lead_turns: 3,
        },
        attention: [{ kind: 'blocked', order: 'o-queue', text: 'Asks permission to run `cargo add tokio`.' }],
      },
    ),

  // B has delivered and waits to be integrated; A still working.
  complete: () =>
    world(
      [
        ...settledPair,
        order('o-memory', 1, 'Persistent memory', 'working', { activity: 'reading', worker: deepseek, since: minutesAgo(2) }),
        order('o-queue', 2, 'Event queue', 'delivered', { worker: codex, since: minutesAgo(1) }),
        order('o-policy', 5, 'Decision policy', 'planned'),
      ],
      {
        consul: {
          state: 'awaiting_you',
          summary: ['Event queue passed review and waits to be integrated.', 'Cohort I is reading the repository for Persistent memory.'],
          lead_turns: 3,
        },
        attention: [{ kind: 'awaiting_integration', order: 'o-queue', text: 'Delivered; integrate to settle it.' }],
      },
    ),

  // No campaign at all.
  empty: () => ({
    schema: 1,
    source: 'demo',
    at: new Date(T0).toISOString(),
    campaign: null,
    consul: { state: 'quiet', summary: ['There is no campaign yet.', 'Start one from the terminal with `polycode mission create`.'], lead_turns: 0 },
    orders: [],
    censor: { state: 'idle', order: null, reviewer: null },
    attention: [],
    curia: { state: 'closed' },
  }),
};

// "story" walks through the states in order, so transitions can be watched:
// a Cohort arrives, works, tests, hands its tablet to the Censor, the tablet
// is sealed and waits for integration, then it is settled.
const STORY = ['quiet', 'one', 'review', 'blocked', 'complete', 'quiet'];
export function storyAt(seconds, stepSeconds = 12) {
  const i = Math.floor(seconds / stepSeconds) % STORY.length;
  return SCENARIOS[STORY[i]]();
}

// Details a panel shows for an Order, in the same shape as `/api/order/:id`.
export function demoOrderDetail(state, id) {
  const o = state.orders.find((x) => x.id === id);
  if (!o) return null;
  return {
    id: o.id,
    title: o.title,
    state: o.state,
    goal: {
      'o-memory': 'Characters remember what happened to them across sessions.',
      'o-queue': 'World events are queued and delivered to characters in order.',
    }[o.id] || 'Demonstration Order.',
    acceptance: ['Behaviour covered by tests', 'No change to the save format'],
    stages: [
      { kind: 'implementation', label: 'Implementation', status: o.state === 'working' ? 'running' : 'completed', provider: o.worker?.provider ?? null },
      { kind: 'verify', label: 'Verification', status: o.activity === 'testing' ? 'running' : o.state === 'working' ? 'pending' : 'completed', provider: null },
      { kind: 'independent_review', label: 'Independent review', status: o.state === 'in_review' ? 'running' : o.state === 'delivered' ? 'completed' : 'pending', provider: 'claude' },
    ],
    review: o.state === 'delivered' ? { status: 'completed', bottom_line: 'Approve. The queue preserves order under concurrent producers; one naming nit, not blocking.' } : null,
    verification: o.state === 'delivered' ? { status: 'completed', bottom_line: 'cargo test: 412 passed.' } : null,
    reason: o.reason,
    demo: true,
  };
}
