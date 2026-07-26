import { useQuery } from "@tanstack/react-query";

import { api, type ModelUsage } from "@/api/client";
import {
  Empty, Failed, Loading, Metric, Panel, Pill, Table, Td, Th, Tr,
} from "@/components/ui";
import { ago, compact, percent, usd, UNKNOWN } from "@/domain/format";
import { Page } from "@/features/Page";

/**
 * How close a model got to its own ceiling.
 *
 * Peak context is input + cache read + cache write, not `input_tokens` — on a
 * cached agentic session the uncached remainder is single digits against a
 * prompt of hundreds of thousands, so the remainder would report ~0% headroom
 * used on a session about to hit the wall.
 */
function headroom(model: ModelUsage): number | null {
  if (model.peak_context_tokens == null || !model.context_limit) return null;
  return model.peak_context_tokens / model.context_limit;
}

/** Per-million-token rate. Absent stays absent — an unpriced model is not free. */
function rate(value: number | null | undefined): string {
  return value == null ? UNKNOWN : `$${value}`;
}

export function Models() {
  const query = useQuery({ queryKey: ["models"], queryFn: api.models });

  const models = query.data?.models ?? [];
  const catalog = query.data?.catalog ?? null;
  const unpriced = models.filter((model) => model.cost_total_usd == null).length;

  return (
    <Page
      title="Models"
      subtitle="What each model costs, what it can do, and how close you came to its limits"
    >
      <div className="grid grid-cols-2 gap-2 md:grid-cols-4">
        <Metric label="Models used" value={compact(models.length)} />
        <Metric
          label="Catalogue"
          value={catalog ? compact(catalog.models) : UNKNOWN}
          hint={catalog ? `${compact(catalog.providers)} providers` : undefined}
          unavailable={catalog ? undefined : "no catalogue loaded"}
        />
        <Metric
          label="Rates from"
          value={catalog ? catalog.source : UNKNOWN}
          hint={catalog ? `checked ${ago(catalog.checked_at)}` : undefined}
          // A bundled snapshot is the fallback, not the intent: it is only in
          // force when nothing fresher could be fetched.
          tone={catalog?.source === "embedded" ? "warn" : undefined}
          unavailable={catalog ? undefined : "nothing can be priced"}
        />
        <Metric
          label="Unpriced"
          value={compact(unpriced)}
          tone={unpriced ? "warn" : undefined}
          hint={unpriced ? "spend missing from totals" : undefined}
        />
      </div>

      <Panel>
        {query.isLoading ? <Loading /> : query.error ? <Failed error={query.error} /> :
         models.length ? (
          <Table>
            <thead>
              <tr>
                <Th>Model</Th><Th>Family</Th>
                <Th align="right">Calls</Th><Th align="right">Spend</Th>
                <Th align="right">In $/M</Th><Th align="right">Out $/M</Th>
                <Th align="right">Cache read</Th><Th align="right">Cache write</Th>
                <Th align="right">Peak context</Th><Th align="right">Limit</Th>
                <Th align="right">Headroom</Th>
                <Th>Can</Th><Th align="right">Last used</Th>
              </tr>
            </thead>
            <tbody>
              {models.map((model) => {
                const used = headroom(model);
                return (
                  <Tr key={`${model.provider}:${model.model}`}>
                    <Td>
                      <span className="font-mono text-xs">{model.model}</span>
                      {!model.in_catalog ? (
                        <span className="ml-1.5" title="No entry in the model catalogue">
                          <Pill tone="unknown">unlisted</Pill>
                        </span>
                      ) : null}
                      {model.status ? (
                        <span className="ml-1.5"><Pill tone="warn">{model.status}</Pill></span>
                      ) : null}
                      {model.tiered ? (
                        <span className="ml-1.5" title="Costs more above a context size">
                          <Pill>tiered</Pill>
                        </span>
                      ) : null}
                    </Td>
                    <Td className="text-faint">{model.family ?? UNKNOWN}</Td>
                    <Td align="right">{compact(model.generations)}</Td>
                    <Td align="right">{usd(model.cost_total_usd)}</Td>
                    <Td align="right">{rate(model.rate_input)}</Td>
                    <Td align="right">{rate(model.rate_output)}</Td>
                    <Td align="right">{rate(model.rate_cache_read)}</Td>
                    <Td align="right">{rate(model.rate_cache_write)}</Td>
                    <Td align="right">{compact(model.peak_context_tokens)}</Td>
                    <Td align="right" className="text-faint">{compact(model.context_limit)}</Td>
                    <Td
                      align="right"
                      className={
                        used == null ? "text-unknown"
                        : used >= 0.9 ? "text-error"
                        : used >= 0.6 ? "text-warn"
                        : "text-faint"
                      }
                    >
                      {used == null ? UNKNOWN : percent(used, 0)}
                    </Td>
                    <Td>
                      <span className="flex gap-1">
                        {model.reasoning ? <Pill>reasoning</Pill> : null}
                        {model.tool_call ? <Pill>tools</Pill> : null}
                        {model.attachment ? <Pill>files</Pill> : null}
                      </span>
                    </Td>
                    <Td align="right" className="text-faint">{ago(model.last_seen)}</Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : <Empty label="No models captured yet" />}
      </Panel>

      <p className="px-1 text-2xs text-faint">
        Rates are published by models.dev. One-hour cache writes are billed at 2× input,
        which models.dev does not model, so that rate is derived locally. Anthropic's
        premium above 200k input tokens is not published either, so long-context Anthropic
        calls are priced at the base rate and their cost is understated.
      </p>
    </Page>
  );
}
