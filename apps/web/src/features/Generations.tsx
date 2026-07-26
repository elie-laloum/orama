import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router-dom";
import { X } from "lucide-react";

import { api, type Generation } from "@/api/client";
import {
  AgentPill,
  ConfidenceBadge,
  Empty,
  Failed,
  Loading,
  Panel,
  Pill,
  SeverityBadge,
  Table,
  Td,
  Th,
  Tr,
  cn,
} from "@/components/ui";
import {
  UNKNOWN,
  ago,
  clock,
  compact,
  modelLabel,
  ms,
  num,
  truncate,
  usd,
} from "@/domain/format";
import { Page } from "@/features/Page";

const FILTERS = [
  { key: "agent_role", label: "Agent", options: ["main", "subagent", "sidechain", "probe"] },
  { key: "is_error", label: "Outcome", options: ["1", "0"], labels: { "1": "failed", "0": "ok" } },
  { key: "provider", label: "Provider", options: ["anthropic", "openai"] },
] as const;

export function Generations() {
  const [params, setParams] = useSearchParams();
  const [selected, setSelected] = React.useState<string | null>(null);

  const filters: Record<string, string> = {};
  for (const { key } of FILTERS) {
    const value = params.get(key);
    if (value) filters[key] = value;
  }

  const query = useQuery({
    queryKey: ["generations", filters],
    queryFn: () => api.generations({ ...filters, limit: 200 }),
  });

  const setFilter = (key: string, value: string | null) => {
    const next = new URLSearchParams(params);
    if (value) next.set(key, value);
    else next.delete(key);
    setParams(next, { replace: true });
  };

  return (
    <Page
      title="Generations"
      subtitle={query.data ? `${query.data.generations.length} shown` : undefined}
      action={
        <div className="flex items-center gap-1.5">
          {FILTERS.map(({ key, label, options, ...rest }) => {
            const labels = (rest as { labels?: Record<string, string> }).labels;
            const active = params.get(key);
            return (
              <div key={key} className="flex items-center gap-1">
                <span className="text-2xs text-faint">{label}</span>
                <select
                  aria-label={label}
                  value={active ?? ""}
                  onChange={(event) => setFilter(key, event.target.value || null)}
                  className="h-6 rounded-sm border border-border bg-raised px-1.5 text-2xs text-fg"
                >
                  <option value="">any</option>
                  {options.map((option) => (
                    <option key={option} value={option}>
                      {labels?.[option] ?? option}
                    </option>
                  ))}
                </select>
              </div>
            );
          })}
        </div>
      }
    >
      <div
        className={cn(
          "grid gap-3",
          selected ? "lg:grid-cols-[minmax(0,1fr)_460px]" : "grid-cols-1",
        )}
      >
        <Panel>
          {query.isLoading ? (
            <Loading />
          ) : query.error ? (
            <Failed error={query.error} />
          ) : query.data?.generations.length ? (
            <Table>
              <thead>
                <tr>
                  <Th>Started</Th>
                  <Th>Agent</Th>
                  <Th>Model</Th>
                  <Th>Prompt</Th>
                  <Th align="right">TTFT</Th>
                  <Th align="right">Latency</Th>
                  <Th align="right">Context</Th>
                  <Th align="right">Out</Th>
                  <Th align="right">Cost</Th>
                  <Th align="right">Tools</Th>
                  <Th>Outcome</Th>
                </tr>
              </thead>
              <tbody>
                {query.data.generations.map((row) => (
                  <Tr
                    key={row.call_id}
                    selected={selected === key(row)}
                    onClick={() => setSelected(key(row))}
                  >
                    <Td title={clock(row.started_at)} className="text-faint">
                      {ago(row.started_at)}
                    </Td>
                    <Td>
                      <AgentPill role={row.agent_role} name={row.agent_name} />
                    </Td>
                    <Td className="text-muted">{modelLabel(row.model)}</Td>
                    <Td className="max-w-[280px]" title={row.user_prompt ?? undefined}>
                      {truncate(row.user_prompt, 48)}
                    </Td>
                    <Td align="right">{ms(row.ttft_ms)}</Td>
                    <Td align="right">{ms(row.latency_ms)}</Td>
                    <Td align="right" title="input + cache read + cache write">
                      {compact(contextOf(row))}
                    </Td>
                    <Td align="right">{compact(row.output_tokens)}</Td>
                    <Td align="right">{usd(row.cost_total_usd)}</Td>
                    <Td align="right">
                      {row.tool_call_count ? compact(row.tool_call_count) : ""}
                    </Td>
                    <Td>
                      <Outcome row={row} />
                    </Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          ) : (
            <Empty
              label="No generations match"
              hint="Clear the filters, or capture some traffic first."
            />
          )}
        </Panel>

        {selected && (
          <GenerationDetail span={selected} onClose={() => setSelected(null)} />
        )}
      </div>
    </Page>
  );
}

function key(row: Generation) {
  return row.span_id ?? String(row.call_id);
}

/** Context is the whole prompt, not the uncached remainder `input_tokens`. */
function contextOf(row: Generation) {
  if (row.input_tokens == null) return null;
  return (
    (row.input_tokens ?? 0) +
    (row.cache_read_tokens ?? 0) +
    (row.cache_creation_tokens ?? 0)
  );
}

function Outcome({ row }: { row: Generation }) {
  if (row.is_error) {
    return (
      <Pill tone="error" title={row.error_kind ?? undefined}>
        {row.error_kind ?? "failed"}
      </Pill>
    );
  }
  if (row.error_kind === "body_missing") {
    return (
      <Pill tone="unknown" title="No response was captured; usage is unknown">
        not captured
      </Pill>
    );
  }
  if (row.stop_reason === "max_tokens") return <Pill tone="warn">truncated</Pill>;
  return <span className="text-faint">{row.stop_reason ?? UNKNOWN}</span>;
}

/* ── detail pane ──────────────────────────────────────────────────────── */

const TABS = ["Conversation", "Tools", "Findings", "Raw"] as const;

function GenerationDetail({ span, onClose }: { span: string; onClose: () => void }) {
  const [tab, setTab] = React.useState<(typeof TABS)[number]>("Conversation");
  const detail = useQuery({
    queryKey: ["generation", span],
    queryFn: () => api.generation(span),
  });
  // Raw is fetched only when asked for: the bodies are hundreds of kilobytes.
  const raw = useQuery({
    queryKey: ["generation-raw", span],
    queryFn: () => api.generationRaw(span),
    enabled: tab === "Raw",
  });

  return (
    <Panel
      className="h-fit lg:sticky lg:top-3"
      title={
        <div className="flex items-center gap-2">
          <span>Generation</span>
          <code className="font-mono text-2xs text-faint">{span.slice(0, 12)}</code>
        </div>
      }
      action={
        <button
          onClick={onClose}
          aria-label="Close detail"
          className="text-faint hover:text-fg"
        >
          <X size={13} />
        </button>
      }
    >
      <div className="flex gap-px border-b border-border px-2">
        {TABS.map((name) => (
          <button
            key={name}
            onClick={() => setTab(name)}
            className={cn(
              "h-7 px-2 text-2xs",
              tab === name
                ? "border-b border-accent text-fg"
                : "text-faint hover:text-muted",
            )}
          >
            {name}
          </button>
        ))}
      </div>

      <div className="max-h-[70vh] overflow-y-auto p-3 text-sm">
        {detail.isLoading ? (
          <Loading />
        ) : detail.error ? (
          <Failed error={detail.error} />
        ) : !detail.data ? null : tab === "Conversation" ? (
          <Facts generation={detail.data.generation} />
        ) : tab === "Tools" ? (
          <ToolList tools={detail.data.tool_calls} />
        ) : tab === "Findings" ? (
          <Findings alerts={detail.data.alerts} />
        ) : raw.isLoading ? (
          <Loading />
        ) : (
          <pre className="whitespace-pre-wrap break-all font-mono text-2xs text-muted">
            {JSON.stringify(raw.data, null, 2)}
          </pre>
        )}
      </div>
    </Panel>
  );
}

function Facts({ generation }: { generation: Generation }) {
  const rows: [string, React.ReactNode][] = [
    ["Prompt", generation.user_prompt ?? UNKNOWN],
    ["Agent", `${generation.agent_name ?? UNKNOWN} (${generation.agent_role ?? "?"})`],
    ["Model", modelLabel(generation.model_resolved ?? generation.model)],
    ["Provider", `${generation.provider} · ${generation.framework ?? UNKNOWN}`],
    ["Started", clock(generation.started_at)],
    ["Time to first token", ms(generation.ttft_ms)],
    ["Latency", ms(generation.latency_ms)],
    ["Input (uncached)", num(generation.input_tokens)],
    ["Cache read", num(generation.cache_read_tokens)],
    ["Cache write", num(generation.cache_creation_tokens)],
    ["Output", num(generation.output_tokens)],
    ["Thinking", num(generation.thinking_tokens)],
    ["Cost", usd(generation.cost_total_usd)],
    ["  of which cache read", usd(generation.cost_cache_read_usd)],
    ["  of which cache write", usd(generation.cost_cache_write_usd)],
    ["Stop reason", generation.stop_reason ?? UNKNOWN],
    ["HTTP", num(generation.http_status)],
  ];
  return (
    <dl className="space-y-1">
      {rows.map(([label, value]) => (
        <div key={label} className="flex gap-3 border-b border-border/40 py-1">
          <dt className="w-44 shrink-0 whitespace-pre text-2xs text-faint">{label}</dt>
          <dd className="min-w-0 flex-1 break-words text-sm tnum">{value}</dd>
        </div>
      ))}
    </dl>
  );
}

function ToolList({ tools }: { tools: Awaited<ReturnType<typeof api.generation>>["tool_calls"] }) {
  if (!tools.length) return <Empty label="No tools called" />;
  return (
    <ul className="space-y-2">
      {tools.map((tool) => (
        <li key={tool.id} className="rounded-sm border border-border bg-raised p-2">
          <div className="flex items-center gap-2">
            <span className="font-mono text-xs">{tool.name}</span>
            {tool.is_mcp ? <Pill>mcp</Pill> : null}
            {!tool.was_declared && <Pill tone="warn">undeclared</Pill>}
            <span className="ml-auto">
              {tool.is_error ? (
                <Pill tone="error">error</Pill>
              ) : tool.status === "pending" ? (
                <Pill tone="unknown">no result yet</Pill>
              ) : (
                <Pill tone="ok">ok</Pill>
              )}
            </span>
          </div>
          {tool.input_excerpt && (
            <pre className="mt-1.5 max-h-24 overflow-auto whitespace-pre-wrap break-all font-mono text-2xs text-faint">
              {tool.input_excerpt}
            </pre>
          )}
          {tool.result_excerpt && (
            <pre className="mt-1.5 max-h-32 overflow-auto whitespace-pre-wrap break-all font-mono text-2xs text-muted">
              {tool.result_excerpt}
            </pre>
          )}
          {tool.result_chars != null && (
            <div className="mt-1 text-2xs text-faint tnum">
              {compact(tool.result_chars)} chars returned
            </div>
          )}
        </li>
      ))}
    </ul>
  );
}

/**
 * The full diagnostic, not just a title. The backend computes explanation,
 * impact and recommendation for every finding; showing only the headline would
 * throw away the part that tells you what to do.
 */
export function Findings({ alerts }: { alerts: Awaited<ReturnType<typeof api.generation>>["alerts"] }) {
  if (!alerts.length) return <Empty label="No findings" />;
  return (
    <ul className="space-y-2">
      {alerts.map((alert) => (
        <li key={alert.id} className="rounded-sm border border-border bg-raised p-2.5">
          <div className="flex flex-wrap items-center gap-1.5">
            <SeverityBadge severity={alert.severity} />
            <span className="text-sm font-medium">{alert.title}</span>
            <ConfidenceBadge confidence={alert.confidence} />
            <code className="ml-auto font-mono text-2xs text-faint">
              {alert.rule_id}
            </code>
          </div>
          <p className="mt-1.5 text-sm">{alert.summary}</p>
          <dl className="mt-2 space-y-1 text-2xs">
            <Line term="Why" text={alert.explanation} />
            <Line term="Impact" text={alert.impact} />
            <Line term="Do" text={alert.recommendation} />
          </dl>
          {alert.observed && Object.keys(alert.observed).length > 0 && (
            <div className="mt-2 flex flex-wrap gap-1">
              {Object.entries(alert.observed).map(([name, value]) => (
                <Pill key={name}>
                  {name}: {String(value ?? UNKNOWN)}
                </Pill>
              ))}
            </div>
          )}
        </li>
      ))}
    </ul>
  );
}

function Line({ term, text }: { term: string; text: string }) {
  return (
    <div className="flex gap-2">
      <dt className="w-12 shrink-0 text-faint">{term}</dt>
      <dd className="min-w-0 flex-1 text-muted">{text}</dd>
    </div>
  );
}
