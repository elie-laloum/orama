import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";

import { api } from "@/api/client";
import {
  Empty,
  Failed,
  Loading,
  Metric,
  Panel,
  Pill,
  SeverityBadge,
  Table,
  Td,
  Th,
  Tr,
} from "@/components/ui";
import { ago, compact, modelLabel, percent, truncate, usd } from "@/domain/format";
import { Page } from "@/features/Page";

export function Dashboard() {
  const overview = useQuery({ queryKey: ["overview"], queryFn: api.overview });
  const meta = useQuery({ queryKey: ["meta"], queryFn: api.meta });

  if (overview.isLoading) return <Loading />;
  if (overview.error) return <Failed error={overview.error} />;

  const totals = overview.data?.totals;
  const bySeverity = overview.data?.alerts_by_severity ?? [];
  const errors =
    bySeverity.find((s) => s.severity === "error")?.count ??
    0 + (bySeverity.find((s) => s.severity === "critical")?.count ?? 0);
  const coverage = meta.data?.usage_coverage;
  const partial = coverage != null && coverage < 1;

  return (
    <Page
      title="Dashboard"
      subtitle={
        totals?.first_seen
          ? `${ago(totals.last_seen)} · since ${ago(totals.first_seen)}`
          : undefined
      }
    >
      {/* Spend leads: it is the number that changes behaviour. Cache savings
          sit beside it because in agentic traffic the cache, not the prompt,
          is where the money goes. */}
      <div className="grid grid-cols-2 gap-2 lg:grid-cols-5">
        <Metric
          label="Spend"
          value={usd(totals?.cost_total_usd)}
          hint={
            partial
              ? `${percent(coverage)} of calls priced`
              : "all calls priced"
          }
          tone={partial ? "warn" : undefined}
        />
        <Metric
          label="Saved by cache"
          value={usd(totals?.cache_savings_usd)}
          hint="vs. no caching"
          tone="ok"
        />
        <Metric label="Generations" value={compact(totals?.generations)} />
        <Metric
          label="Tool calls"
          value={compact(totals?.tool_calls)}
          hint={`${compact(totals?.traces)} traces`}
        />
        <Metric
          label="Failed calls"
          value={compact(totals?.errors)}
          tone={(totals?.errors ?? 0) > 0 ? "error" : "ok"}
          hint={`${errors} error-level findings`}
        />
      </div>

      <div className="grid gap-3 lg:grid-cols-[1.2fr_1fr]">
        <Panel
          title="Recent sessions"
          action={
            <Link to="/sessions" className="text-2xs text-accent hover:underline">
              all sessions
            </Link>
          }
        >
          {overview.data?.recent_sessions.length ? (
            <Table>
              <thead>
                <tr>
                  <Th>Session</Th>
                  <Th>Model</Th>
                  <Th align="right">Calls</Th>
                  <Th align="right">Cost</Th>
                  <Th align="right">Last seen</Th>
                </tr>
              </thead>
              <tbody>
                {overview.data.recent_sessions.map((session) => (
                  <Tr key={session.session_id}>
                    <Td className="max-w-[280px]">
                      <Link
                        to={`/sessions/${encodeURIComponent(session.session_id)}`}
                        className="hover:text-accent"
                      >
                        {truncate(session.title ?? session.session_id, 52)}
                      </Link>
                    </Td>
                    <Td>
                      <Pill>{modelLabel(session.primary_model)}</Pill>
                    </Td>
                    <Td align="right">{compact(session.generation_count)}</Td>
                    <Td align="right">{usd(session.cost_total_usd)}</Td>
                    <Td align="right" className="text-faint">
                      {ago(session.ended_at ?? session.started_at)}
                    </Td>
                  </Tr>
                ))}
              </tbody>
            </Table>
          ) : (
            <Empty
              label="No sessions captured yet"
              hint="Point a coding agent at this proxy and traffic will appear here."
            />
          )}
        </Panel>

        <div className="space-y-3">
          <Panel
            title="Findings"
            action={
              <Link to="/errors" className="text-2xs text-accent hover:underline">
                all findings
              </Link>
            }
          >
            {bySeverity.length ? (
              <ul className="divide-y divide-border/60">
                {(["critical", "error", "warning", "info"] as const)
                  .map((severity) => ({
                    severity,
                    count:
                      bySeverity.find((s) => s.severity === severity)?.count ?? 0,
                  }))
                  .filter((row) => row.count > 0)
                  .map(({ severity, count }) => (
                    <li
                      key={severity}
                      className="flex items-center justify-between px-3 py-2"
                    >
                      <SeverityBadge severity={severity} />
                      <span className="tnum text-sm">{count}</span>
                    </li>
                  ))}
              </ul>
            ) : (
              <Empty label="No findings" />
            )}
          </Panel>

          <Panel
            title="Spend by model"
            action={
              <Link to="/cost" className="text-2xs text-accent hover:underline">
                breakdown
              </Link>
            }
          >
            {overview.data?.by_model.length ? (
              <Table>
                <thead>
                  <tr>
                    <Th>Model</Th>
                    <Th align="right">Calls</Th>
                    <Th align="right">Tokens</Th>
                    <Th align="right">Cost</Th>
                  </tr>
                </thead>
                <tbody>
                  {overview.data.by_model.map((row) => (
                    <Tr key={row.model}>
                      <Td>{modelLabel(row.model)}</Td>
                      <Td align="right">{compact(row.generations)}</Td>
                      <Td align="right">{compact(row.total_tokens)}</Td>
                      <Td align="right">{usd(row.cost_total_usd)}</Td>
                    </Tr>
                  ))}
                </tbody>
              </Table>
            ) : (
              <Empty label="Nothing priced yet" />
            )}
          </Panel>
        </div>
      </div>
    </Page>
  );
}
