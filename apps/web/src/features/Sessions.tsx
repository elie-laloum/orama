import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "react-router-dom";
import { ChevronLeft } from "lucide-react";

import { api } from "@/api/client";
import {
  AgentPill, Empty, Failed, Loading, Metric, Panel, Pill,
  Table, Td, Th, Tr,
} from "@/components/ui";
import { UNKNOWN, ago, clock, compact, modelLabel, ms, percent, truncate, usd } from "@/domain/format";
import { Findings } from "@/features/Generations";
import { Page } from "@/features/Page";

export function Sessions() {
  const query = useQuery({ queryKey: ["sessions"], queryFn: () => api.sessions({ limit: 200 }) });
  return (
    <Page title="Sessions" subtitle="One session is one CLI run">
      <Panel>
        {query.isLoading ? <Loading /> : query.error ? <Failed error={query.error} /> :
         query.data?.sessions.length ? (
          <Table>
            <thead>
              <tr>
                <Th>Session</Th><Th>Model</Th><Th align="right">Calls</Th>
                <Th align="right">Tools</Th><Th align="right">Cost</Th>
                <Th align="right">Saved</Th><Th align="right">Coverage</Th>
                <Th align="right">Last seen</Th>
              </tr>
            </thead>
            <tbody>
              {query.data.sessions.map((session) => {
                const partial = session.usage_coverage != null && session.usage_coverage < 1;
                return (
                  <Tr key={session.session_id}>
                    <Td className="max-w-[360px]">
                      <Link to={`/sessions/${encodeURIComponent(session.session_id)}`} className="hover:text-accent">
                        {truncate(session.title ?? session.session_id, 60)}
                      </Link>
                    </Td>
                    <Td><Pill>{modelLabel(session.primary_model)}</Pill></Td>
                    <Td align="right">{compact(session.generation_count)}</Td>
                    <Td align="right">{compact(session.tool_call_count)}</Td>
                    <Td align="right">{usd(session.cost_total_usd)}</Td>
                    <Td align="right" className="text-ok">{usd(session.cache_savings_usd)}</Td>
                    {/* Coverage sits beside the totals: they are only as complete as it is. */}
                    <Td align="right" className={partial ? "text-warn" : "text-faint"}>
                      {percent(session.usage_coverage)}
                    </Td>
                    <Td align="right" className="text-faint">
                      {ago(session.ended_at ?? session.started_at)}
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : <Empty label="No sessions captured yet" />}
      </Panel>
    </Page>
  );
}

export function SessionDetail() {
  const { id = "" } = useParams();
  const query = useQuery({ queryKey: ["session", id], queryFn: () => api.session(id) });

  if (query.isLoading) return <Loading />;
  if (query.error) return <Failed error={query.error} />;
  const data = query.data;
  if (!data) return null;

  const { session, timeline, agents, alerts } = data;
  const peak = Math.max(1, ...timeline.map((row) => row.context_tokens ?? 0));

  return (
    <Page
      title={truncate(session.title ?? session.session_id, 64)}
      subtitle={`${clock(session.started_at)} · ${session.git_branch ?? UNKNOWN}`}
      action={
        <Link to="/sessions" className="flex items-center gap-1 text-2xs text-accent hover:underline">
          <ChevronLeft size={12} /> all sessions
        </Link>
      }
    >
      <div className="grid grid-cols-2 gap-2 lg:grid-cols-5">
        <Metric label="Calls" value={compact(session.generation_count)} />
        <Metric label="Tools" value={compact(session.tool_call_count)} />
        <Metric label="Cost" value={usd(session.cost_total_usd)} />
        <Metric label="Saved by cache" value={usd(session.cache_savings_usd)} tone="ok" />
        <Metric
          label="Errors"
          value={compact(session.error_count)}
          tone={(session.error_count ?? 0) > 0 ? "error" : "ok"}
        />
      </div>

      <div className="grid gap-3 lg:grid-cols-[1fr_320px]">
        <Panel title="Context over the session">
          {/* A bar per call, scaled to the session's peak. Context growth and
              compaction are both visible as shape rather than as a number. */}
          <div className="flex h-24 items-end gap-px overflow-x-auto p-3">
            {timeline.map((row) => (
              <div
                key={row.call_id}
                title={`${compact(row.context_tokens)} tokens · ${ms(row.latency_ms)} · ${usd(row.cost_total_usd)}`}
                style={{ height: `${Math.max(2, ((row.context_tokens ?? 0) / peak) * 100)}%` }}
                className={`w-1.5 shrink-0 rounded-t-sm ${
                  row.is_error ? "bg-error" : row.agent_role === "main" ? "bg-accent" : "bg-border-strong"
                }`}
              />
            ))}
          </div>
          <Table>
            <thead>
              <tr>
                <Th>Time</Th><Th>Agent</Th><Th>Model</Th>
                <Th align="right">Context</Th><Th align="right">TTFT</Th>
                <Th align="right">Latency</Th><Th align="right">Cost</Th>
              </tr>
            </thead>
            <tbody>
              {timeline.map((row) => (
                <Tr key={row.call_id}>
                  <Td className="text-faint" title={clock(row.started_at)}>{ago(row.started_at)}</Td>
                  <Td><AgentPill role={row.agent_role} name={row.agent_name} /></Td>
                  <Td className="text-muted">{modelLabel(row.model)}</Td>
                  <Td align="right">{compact(row.context_tokens)}</Td>
                  <Td align="right">{ms(row.ttft_ms)}</Td>
                  <Td align="right">{ms(row.latency_ms)}</Td>
                  <Td align="right">{usd(row.cost_total_usd)}</Td>
                </Tr>
              ))}
            </tbody>
          </Table>
        </Panel>

        <div className="space-y-3">
          <Panel title="Agents">
            <Table>
              <thead>
                <tr><Th>Agent</Th><Th align="right">Calls</Th><Th align="right">Cost</Th></tr>
              </thead>
              <tbody>
                {agents.map((agent) => (
                  <Tr key={String(agent.agent_id)}>
                    <Td>
                      <AgentPill
                        role={agent.agent_role as string | null}
                        name={agent.agent_name as string | null}
                      />
                    </Td>
                    <Td align="right">{compact(agent.generation_count as number)}</Td>
                    <Td align="right">{usd(agent.cost_total_usd as number | null)}</Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          </Panel>

          <Panel title={`Findings (${alerts.length})`}>
            <div className="max-h-[50vh] overflow-y-auto p-2.5">
              <Findings alerts={alerts} />
            </div>
          </Panel>
        </div>
      </div>
    </Page>
  );
}
