/**
 * Typed access to the v2 API.
 *
 * Rows come back keyed by column name, so these types mirror the derived
 * tables. Every count is `number | null`: the API reports absent data as null
 * rather than zero, and the UI preserves that distinction all the way to the
 * screen.
 */

const BASE = "/api/v2";

export type Nullable = number | null;

export interface Meta {
  calls: number;
  generations: number;
  tool_calls: number;
  sessions: number;
  traces: number;
  alerts: number;
  derive_failures: number;
  usage_coverage: Nullable;
  cost_coverage: Nullable;
  parser_version: string;
  pricing_version: string;
  policy_version: string;
}

export interface Generation {
  call_id: number;
  span_id: string | null;
  trace_id: string | null;
  session_id: string | null;
  parent_span_id: string | null;
  depth: number;
  provider: string;
  framework: string | null;
  agent_name: string | null;
  agent_role: string | null;
  model: string | null;
  model_resolved: string | null;
  started_at: string;
  ended_at: string | null;
  ttft_ms: Nullable;
  latency_ms: Nullable;
  input_tokens: Nullable;
  output_tokens: Nullable;
  total_tokens: Nullable;
  cache_read_tokens: Nullable;
  cache_creation_tokens: Nullable;
  thinking_tokens: Nullable;
  cost_total_usd: Nullable;
  cost_cache_read_usd: Nullable;
  cost_cache_write_usd: Nullable;
  http_status: Nullable;
  stop_reason: string | null;
  is_error: number;
  error_kind: string | null;
  tool_call_count: number;
  tools_called: string[] | null;
  user_prompt: string | null;
  context_tokens?: Nullable;
}

export interface ToolCall {
  id: number;
  call_id: number;
  seq: number;
  tool_use_id: string | null;
  name: string;
  server: string | null;
  is_mcp: number;
  was_declared: number;
  input_chars: Nullable;
  input_excerpt: string | null;
  result_chars: Nullable;
  result_excerpt: string | null;
  is_error: Nullable;
  status: string;
  emitted_at: string | null;
  duration_ms: Nullable;
}

export type Severity = "critical" | "error" | "warning" | "info";

export interface Alert {
  id: number;
  rule_id: string;
  category: string;
  severity: Severity;
  confidence: "exact" | "inferred" | "unavailable";
  scope_kind: string;
  scope_id: string;
  call_id: Nullable;
  span_id: string | null;
  trace_id: string | null;
  session_id: string | null;
  title: string;
  summary: string;
  explanation: string;
  impact: string;
  recommendation: string;
  observed: Record<string, unknown> | null;
  metric_value: Nullable;
  threshold: Nullable;
  metric_unit: string | null;
  occurred_at: string;
  occurrences: number;
}

export interface Session {
  session_id: string;
  title: string | null;
  project_name: string | null;
  git_branch: string | null;
  primary_model: string | null;
  models: string[] | null;
  started_at: string;
  ended_at: string | null;
  generation_count: number;
  tool_call_count: Nullable;
  error_count: Nullable;
  input_tokens: Nullable;
  output_tokens: Nullable;
  cache_read_tokens: Nullable;
  cost_total_usd: Nullable;
  cache_savings_usd: Nullable;
  usage_coverage: Nullable;
}

export interface Trace {
  trace_id: string;
  session_id: string | null;
  started_at: string;
  ended_at: string | null;
  generation_count: number;
  tool_call_count: Nullable;
  agent_count: number;
  error_count: Nullable;
  total_tokens: Nullable;
  cost_total_usd: Nullable;
  max_depth: number;
  user_prompt: string | null;
}

export interface Overview {
  totals: {
    generations: number;
    sessions: number;
    traces: number;
    errors: Nullable;
    input_tokens: Nullable;
    output_tokens: Nullable;
    cache_read_tokens: Nullable;
    cache_creation_tokens: Nullable;
    thinking_tokens: Nullable;
    cost_total_usd: Nullable;
    cache_savings_usd: Nullable;
    tool_calls: Nullable;
    first_seen: string | null;
    last_seen: string | null;
  } | null;
  alerts_by_severity: { severity: Severity; count: number }[];
  by_model: {
    model: string;
    generations: number;
    cost_total_usd: Nullable;
    total_tokens: Nullable;
  }[];
  recent_sessions: Session[];
}

export interface ErrorGroup {
  rule_id: string;
  category: string;
  severity: Severity;
  confidence: string;
  title: string;
  explanation: string;
  impact: string;
  recommendation: string;
  occurrences: number;
  sessions: number;
  first_seen: string;
  last_seen: string;
  sample_summary: string;
}

export interface ToolStat {
  name: string;
  server: string | null;
  is_mcp: number;
  calls: number;
  errors: number;
  error_rate: Nullable;
  pending: Nullable;
  undeclared: Nullable;
  avg_result_chars: Nullable;
  max_result_chars: Nullable;
  sessions: number;
  last_used: string | null;
}

export interface CostBucket {
  bucket: string;
  generations: number;
  input_tokens: Nullable;
  output_tokens: Nullable;
  cache_read_tokens: Nullable;
  cache_creation_tokens: Nullable;
  cost_input_usd: Nullable;
  cost_output_usd: Nullable;
  cost_cache_write_usd: Nullable;
  cost_cache_read_usd: Nullable;
  cost_total_usd: Nullable;
  cache_savings_usd: Nullable;
  priced_share: Nullable;
}

/* ── harness ──────────────────────────────────────────────────────────── */

export interface SystemSegment {
  text: string;
  chars: number;
  cache_control: boolean;
}

/** One distinct system prompt, with how widely it was used. */
export interface SystemPromptSummary {
  system_hash: string;
  total_chars: number;
  segment_count: number;
  cache_points: number;
  generations: number;
  sessions: number;
  first_seen: string | null;
  last_seen: string | null;
  agent_roles: string | null;
  latest_span: string | null;
  /** First line of actual prompt text, past the billing header. */
  opening: string | null;
}

export interface ToolSetSummary {
  tools_hash: string;
  tool_count: number;
  total_chars: number;
  mcp_count: number;
  generations: number;
  sessions: number;
  first_seen: string | null;
  last_seen: string | null;
  agent_roles: string | null;
}

/** A tool declaration, and whether anything ever called it. */
export interface DeclaredTool {
  name: string;
  server: string | null;
  is_mcp: number;
  chars: number;
  tool_sets: number;
  calls: number;
  last_used: string | null;
}

export interface ToolSchema {
  seq: number;
  name: string;
  server: string | null;
  is_mcp: number;
  description: string | null;
  input_schema: unknown;
  chars: number;
  calls: number;
}

export interface Harness {
  budget: {
    system_chars: Nullable;
    tools_chars: Nullable;
    history_chars: Nullable;
    generations: number;
    with_tools: Nullable;
    max_tools_chars: Nullable;
    max_history_chars: Nullable;
  } | null;
  system_prompts: SystemPromptSummary[];
  tool_sets: ToolSetSummary[];
  declared_tools: DeclaredTool[];
}

export type BlockKind =
  | "text"
  | "thinking"
  | "tool_use"
  | "tool_result"
  | "image"
  | "other";

export interface ContextBlock {
  kind: BlockKind;
  /** Set when the harness injected this rather than the user writing it. */
  content_tag: string | null;
  chars: number;
  tool_name: string | null;
  is_error: boolean | null;
  preview: string | null;
  truncated: boolean;
}

export interface ContextTurn {
  index: number;
  role: "user" | "assistant" | "tool" | "system" | "other";
  origin: "history" | "new";
  chars: number;
  blocks: ContextBlock[];
}

export interface CallContext {
  shape: Record<string, unknown> & {
    system_hash: string | null;
    tools_hash: string | null;
    system_segments_count: Nullable;
    system_cache_points: Nullable;
    tools_declared_count: Nullable;
    messages_count: Nullable;
    compaction_requested: number;
    input_tokens: Nullable;
    cache_read_tokens: Nullable;
    cache_creation_tokens: Nullable;
  };
  /** The three sections are disjoint and sum to the whole request. */
  composition: {
    system_chars: number;
    tools_chars: number;
    history_chars: number;
    total_chars: number;
  };
  system: {
    segments: SystemSegment[];
    total_chars: number;
    segment_count: number;
    cache_points: number;
  } | null;
  tools: {
    name: string;
    server: string | null;
    is_mcp: number;
    chars: number;
    description_head: string | null;
  }[];
  thread: {
    turns: ContextTurn[];
    by_kind: { kind: BlockKind; count: number; chars: number }[];
  };
}

/* ── settings ─────────────────────────────────────────────────────────── */

export interface ProxyState {
  listening_on: string;
  base_url: string;
  openai_base_url: string;
  upstream_anthropic: string;
  upstream_openai: string;
  /** Codex on a ChatGPT subscription: a different backend, not a different path. */
  upstream_chatgpt: string;
  db_path: string;
  db_bytes: Nullable;
  /** False when the database could not be opened: relaying, but not recording. */
  capturing: boolean;
  started_at: string;
  /** Null means nothing has ever been captured, not "captured at time zero". */
  last_capture_at: string | null;
  parser_version: string;
  pricing_version: string;
  policy_version: string;
}

/** A harness Orama can configure by editing its own config file. */
export interface Connector {
  id: string;
  label: string;
  config_path: string;
  config_exists: boolean;
  /** Points at this proxy specifically, not merely at some proxy. */
  connected: boolean;
  /** Where it currently points; null means straight to the provider. */
  base_url: string | null;
  /** We wrote the current value, so disconnecting can restore what was there. */
  managed: boolean;
  effect: string;
  /** A prerequisite the connector cannot meet for you. Shown before the click. */
  caveat: string | null;
  restart_required: boolean;
  error: string | null;
}

export interface GuideSnippet {
  language: string;
  label: string;
  code: string;
}

/** A client with no config file to write — instructions instead of a button. */
export interface Guide {
  id: string;
  label: string;
  summary: string;
  env: { name: string; value: string }[];
  snippets: GuideSnippet[];
  note: string;
}

export interface Settings {
  proxy: ProxyState;
  connectors: Connector[];
  guides: Guide[];
}

export interface ConnectOutcome {
  harness: string;
  config_path: string;
  backup_path: string | null;
  /** The config already said what we were about to write. */
  already: boolean;
  status: Connector;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

async function get<T>(path: string): Promise<T> {
  const response = await fetch(`${BASE}${path}`, {
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    // The API explains its rejections — an unknown filter names the accepted
    // set — so surface that rather than a bare status code.
    let detail = response.statusText;
    try {
      detail = ((await response.json()) as { error?: string }).error ?? detail;
    } catch {
      /* non-JSON body; the status text stands */
    }
    throw new ApiError(detail, response.status);
  }
  return (await response.json()) as T;
}

/**
 * The one place this client writes.
 *
 * Kept deliberately separate from `get`: everything else here reads the
 * capture database, and these two routes edit a harness's own config file.
 */
async function post<T>(path: string): Promise<T> {
  const response = await fetch(`${BASE}${path}`, {
    method: "POST",
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    let detail = response.statusText;
    try {
      detail = ((await response.json()) as { error?: string }).error ?? detail;
    } catch {
      /* non-JSON body; the status text stands */
    }
    throw new ApiError(detail, response.status);
  }
  return (await response.json()) as T;
}

function qs(params: Record<string, string | number | undefined>) {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") search.set(key, String(value));
  }
  const text = search.toString();
  return text ? `?${text}` : "";
}

type Params = Record<string, string | number | undefined>;

export const api = {
  meta: () => get<Meta>("/meta"),
  overview: () => get<Overview>("/overview"),

  generations: (params: Params = {}) =>
    get<{ generations: Generation[] }>(`/generations${qs(params)}`),
  generation: (span: string) =>
    get<{ generation: Generation; tool_calls: ToolCall[]; alerts: Alert[] }>(
      `/generations/${encodeURIComponent(span)}`,
    ),
  generationRaw: (span: string) =>
    get<Record<string, unknown>>(`/generations/${encodeURIComponent(span)}/raw`),
  generationContext: (span: string) =>
    get<CallContext>(`/generations/${encodeURIComponent(span)}/context`),

  harness: () => get<Harness>("/harness"),
  harnessSystem: (hash: string) =>
    get<{
      system: {
        system_hash: string;
        segments: SystemSegment[];
        total_chars: number;
        segment_count: number;
        cache_points: number;
      };
      usage: Record<string, unknown> | null;
    }>(`/harness/system/${encodeURIComponent(hash)}`),
  harnessTools: (hash: string) =>
    get<{
      tool_set: ToolSetSummary;
      tools: ToolSchema[];
      usage: Record<string, unknown> | null;
    }>(`/harness/tools/${encodeURIComponent(hash)}`),

  traces: (params: Params = {}) => get<{ traces: Trace[] }>(`/traces${qs(params)}`),
  trace: (id: string) =>
    get<{
      trace_id: string;
      generations: Generation[];
      tool_calls: ToolCall[];
      alerts: Alert[];
    }>(`/traces/${encodeURIComponent(id)}`),

  sessions: (params: Params = {}) =>
    get<{ sessions: Session[] }>(`/sessions${qs(params)}`),
  session: (id: string) =>
    get<{
      session: Session;
      agents: Record<string, unknown>[];
      timeline: Generation[];
      alerts: Alert[];
    }>(`/sessions/${encodeURIComponent(id)}`),

  alerts: (params: Params = {}) =>
    get<{ alerts: Alert[]; facets: Record<string, unknown>[] }>(`/alerts${qs(params)}`),
  errors: () => get<{ groups: ErrorGroup[] }>("/errors"),

  tools: () => get<{ tools: ToolStat[] }>("/tools"),
  tool: (name: string) =>
    get<{ calls: ToolCall[] }>(`/tools/${encodeURIComponent(name)}`),

  cost: (groupBy: string) =>
    get<{ buckets: CostBucket[] }>(`/cost${qs({ group_by: groupBy })}`),

  settings: () => get<Settings>("/settings"),
  connect: (id: string) =>
    post<ConnectOutcome>(`/connectors/${encodeURIComponent(id)}/connect`),
  disconnect: (id: string) =>
    post<ConnectOutcome>(`/connectors/${encodeURIComponent(id)}/disconnect`),
};

/**
 * Subscribe to live capture events.
 *
 * The stream carries notifications, not state, so an event simply invalidates
 * the cache and the UI re-reads. A `resync` is handled the same way — there is
 * nothing to reconcile, only something to re-fetch.
 */
export function subscribe(onChange: () => void, onStatus: (live: boolean) => void) {
  let source: EventSource | null = null;
  let delay = 500;
  let stopped = false;

  const connect = () => {
    if (stopped) return;
    source = new EventSource(`${BASE}/events`);
    source.onopen = () => {
      delay = 500;
      onStatus(true);
    };
    for (const name of ["capture.started", "generation.derived", "derive.failed", "resync"]) {
      source.addEventListener(name, () => onChange());
    }
    source.onerror = () => {
      onStatus(false);
      source?.close();
      if (stopped) return;
      setTimeout(connect, delay);
      delay = Math.min(delay * 2, 15_000);
    };
  };

  connect();
  return () => {
    stopped = true;
    source?.close();
  };
}
