import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { ChevronLeft, CornerDownRight, Wrench } from "lucide-react";

import { api, type Generation, type ToolCall } from "@/api/client";
import {
  AgentPill,
  Empty,
  Failed,
  Loading,
  Metric,
  Panel,
  Pill,
  Table,
  Td,
  Th,
  Tr,
  cn,
} from "@/components/ui";
import { UNKNOWN, ago, clock, compact, modelLabel, ms, truncate, usd } from "@/domain/format";
import { Findings } from "@/features/Generations";
import { Page } from "@/features/Page";

/** A trace is one user turn: the calls it took, and the tools they ran. */
export function Traces() {
  const query = useQuery({
    queryKey: ["traces"],
    queryFn: () => api.traces({ limit: 200 }),
  });

  return (
    <Page
      title="Traces"
      subtitle="One trace is one user turn — every call it took to answer"
    >
      <Panel>
        {query.isLoading ? (
          <Loading />
        ) : query.error ? (
          <Failed error={query.error} />
        ) : query.data?.traces.length ? (
          <Table>
            <thead>
              <tr>
                <Th>Started</Th>
                <Th>Turn</Th>
                <Th align="right">Calls</Th>
                <Th align="right">Agents</Th>
                <Th align="right">Tools</Th>
                <Th align="right">Tokens</Th>
                <Th align="right">Cost</Th>
                <Th align="right">Errors</Th>
              </tr>
            </thead>
            <tbody>
              {query.data.traces.map((trace) => (
                <Tr key={trace.trace_id}>
                  <Td title={clock(trace.started_at)} className="text-faint">
                    <Link
                      to={`/traces/${trace.trace_id}`}
                      className="hover:text-accent"
                    >
                      {ago(trace.started_at)}
                    </Link>
                  </Td>
                  <Td className="max-w-[420px]" title={trace.user_prompt ?? undefined}>
                    <Link
                      to={`/traces/${trace.trace_id}`}
                      className="hover:text-accent"
                    >
                      {truncate(trace.user_prompt, 70)}
                    </Link>
                  </Td>
                  <Td align="right">{compact(trace.generation_count)}</Td>
                  <Td align="right">{compact(trace.agent_count)}</Td>
                  <Td align="right">{compact(trace.tool_call_count)}</Td>
                  <Td align="right">{compact(trace.total_tokens)}</Td>
                  <Td align="right">{usd(trace.cost_total_usd)}</Td>
                  <Td align="right">
                    {trace.error_count ? (
                      <span className="text-error">{trace.error_count}</span>
                    ) : (
                      ""
                    )}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        ) : (
          <Empty label="No traces yet" />
        )}
      </Panel>
    </Page>
  );
}

/**
 * The trace tree: generations in order, each with its tool spans beneath, and
 * a duration bar drawn against the trace's own span so the shape of the turn
 * is readable at a glance.
 */
export function TraceDetail() {
  const { id = "" } = useParams();
  const query = useQuery({
    queryKey: ["trace", id],
    queryFn: () => api.trace(id),
  });

  if (query.isLoading) return <Loading />;
  if (query.error) return <Failed error={query.error} />;
  const data = query.data;
  if (!data) return null;

  const generations = data.generations;
  const start = Math.min(...generations.map((g) => Date.parse(g.started_at)));
  const end = Math.max(
    ...generations.map((g) => Date.parse(g.ended_at ?? g.started_at)),
  );
  const span = Math.max(1, end - start);

  const toolsByCall = new Map<number, ToolCall[]>();
  for (const tool of data.tool_calls) {
    const list = toolsByCall.get(tool.call_id) ?? [];
    list.push(tool);
    toolsByCall.set(tool.call_id, list);
  }

  const cost = generations.reduce((sum, g) => sum + (g.cost_total_usd ?? 0), 0);
  const tokens = generations.reduce((sum, g) => sum + (g.total_tokens ?? 0), 0);

  return (
    <Page
      title="Trace"
      subtitle={truncate(generations[0]?.user_prompt, 80)}
      action={
        <Link
          to="/traces"
          className="flex items-center gap-1 text-2xs text-accent hover:underline"
        >
          <ChevronLeft size={12} /> all traces
        </Link>
      }
    >
      <div className="grid grid-cols-2 gap-2 lg:grid-cols-5">
        <Metric label="Calls" value={compact(generations.length)} />
        <Metric label="Tools" value={compact(data.tool_calls.length)} />
        <Metric label="Tokens" value={compact(tokens)} />
        <Metric label="Cost" value={usd(cost)} />
        <Metric label="Duration" value={ms(span)} />
      </div>

      <Panel title="Timeline">
        <ul className="divide-y divide-border/50">
          {generations.map((generation) => (
            <li key={generation.call_id}>
              <Row generation={generation} start={start} span={span} />
              {(toolsByCall.get(generation.call_id) ?? []).map((tool) => (
                <ToolRow key={tool.id} tool={tool} />
              ))}
            </li>
          ))}
        </ul>
      </Panel>

      {data.alerts.length > 0 && (
        <Panel title="Findings">
          <div className="p-3">
            <Findings alerts={data.alerts} />
          </div>
        </Panel>
      )}
    </Page>
  );
}

function Row({
  generation,
  start,
  span,
}: {
  generation: Generation;
  start: number;
  span: number;
}) {
  const began = Date.parse(generation.started_at) - start;
  const width = generation.latency_ms ?? 0;
  const nested = generation.depth > 0;
  return (
    <div
      className={cn(
        "flex items-center gap-2 px-3 py-1.5 hover:bg-raised/50",
        nested && "pl-8",
      )}
    >
      {nested && <CornerDownRight size={11} className="text-faint" aria-hidden />}
      <AgentPill role={generation.agent_role} name={generation.agent_name} />
      <span className="w-32 shrink-0 truncate text-2xs text-muted">
        {modelLabel(generation.model)}
      </span>

      {/* Duration bar, positioned within the trace's own window. */}
      <div className="relative h-2 min-w-0 flex-1 rounded-sm bg-raised">
        <div
          className={cn(
            "absolute h-2 rounded-sm",
            generation.is_error ? "bg-error" : "bg-accent",
          )}
          style={{
            left: `${(began / span) * 100}%`,
            width: `${Math.max(1.5, (width / span) * 100)}%`,
          }}
          title={`${ms(generation.latency_ms)} at +${ms(began)}`}
        />
      </div>

      <span className="w-14 shrink-0 text-right text-2xs tnum text-faint">
        {ms(generation.latency_ms)}
      </span>
      <span className="w-16 shrink-0 text-right text-2xs tnum">
        {usd(generation.cost_total_usd)}
      </span>
      <span className="w-12 shrink-0 text-right text-2xs tnum text-faint">
        {compact(generation.total_tokens)}
      </span>
    </div>
  );
}

function ToolRow({ tool }: { tool: ToolCall }) {
  return (
    <div className="flex items-center gap-2 py-1 pl-12 pr-3 text-2xs hover:bg-raised/40">
      <Wrench size={10} className="shrink-0 text-faint" aria-hidden />
      <span className="font-mono text-xs">{tool.name}</span>
      {tool.is_error ? (
        <Pill tone="error">error</Pill>
      ) : tool.status === "pending" ? (
        <Pill tone="unknown">no result</Pill>
      ) : null}
      <span className="min-w-0 flex-1 truncate text-faint">
        {tool.input_excerpt ?? ""}
      </span>
      <span className="shrink-0 tnum text-faint">
        {tool.result_chars != null ? `${compact(tool.result_chars)} chars` : UNKNOWN}
      </span>
    </div>
  );
}
