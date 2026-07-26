import * as React from "react";
import { useQuery } from "@tanstack/react-query";

import { api } from "@/api/client";
import {
  Empty, Failed, Loading, Metric, Panel, Table, Td, Th, Tr, cn,
} from "@/components/ui";
import { compact, modelLabel, percent, usd } from "@/domain/format";
import { Page } from "@/features/Page";

const GROUPS = ["model", "agent", "session", "day", "provider"] as const;

export function Cost() {
  const [groupBy, setGroupBy] = React.useState<(typeof GROUPS)[number]>("model");
  const query = useQuery({
    queryKey: ["cost", groupBy],
    queryFn: () => api.cost(groupBy),
  });

  const buckets = query.data?.buckets ?? [];
  const total = buckets.reduce((sum, b) => sum + (b.cost_total_usd ?? 0), 0);
  const saved = buckets.reduce((sum, b) => sum + (b.cache_savings_usd ?? 0), 0);
  const write = buckets.reduce((sum, b) => sum + (b.cost_cache_write_usd ?? 0), 0);
  const read = buckets.reduce((sum, b) => sum + (b.cost_cache_read_usd ?? 0), 0);
  const cacheShare = total > 0 ? (write + read) / total : null;

  return (
    <Page
      title="Cost"
      subtitle="Cache is priced as a multiple of input: 5m write ×1.25, 1h write ×2, read ×0.1"
      action={
        <div className="flex items-center gap-1">
          <span className="text-2xs text-faint">group by</span>
          {GROUPS.map((group) => (
            <button
              key={group}
              onClick={() => setGroupBy(group)}
              className={cn(
                "h-6 rounded-sm border px-2 text-2xs",
                groupBy === group
                  ? "border-accent/50 bg-accent-soft/40 text-fg"
                  : "border-border text-muted hover:text-fg",
              )}
            >
              {group}
            </button>
          ))}
        </div>
      }
    >
      <div className="grid grid-cols-2 gap-2 lg:grid-cols-4">
        <Metric label="Total spend" value={usd(total)} />
        <Metric label="Saved by cache" value={usd(saved)} tone="ok" hint="vs. no caching" />
        <Metric label="Cache write" value={usd(write)} hint="1h costs 2× the base rate" />
        <Metric
          label="Cache share of spend"
          value={percent(cacheShare)}
          hint="write + read"
          unavailable={cacheShare == null ? "nothing priced" : undefined}
        />
      </div>

      <Panel title={`By ${groupBy}`}>
        {query.isLoading ? <Loading /> : query.error ? <Failed error={query.error} /> :
         buckets.length ? (
          <Table>
            <thead>
              <tr>
                <Th>{groupBy}</Th><Th align="right">Calls</Th>
                <Th align="right">Input</Th><Th align="right">Output</Th>
                <Th align="right">Cache read</Th><Th align="right">Cache write</Th>
                <Th align="right">In $</Th><Th align="right">Out $</Th>
                <Th align="right">Cache $</Th><Th align="right">Total</Th>
                <Th align="right">Priced</Th>
              </tr>
            </thead>
            <tbody>
              {buckets.map((bucket) => {
                const partial = bucket.priced_share != null && bucket.priced_share < 1;
                return (
                  <Tr key={bucket.bucket}>
                    <Td className="max-w-[280px]">{modelLabel(bucket.bucket)}</Td>
                    <Td align="right">{compact(bucket.generations)}</Td>
                    <Td align="right">{compact(bucket.input_tokens)}</Td>
                    <Td align="right">{compact(bucket.output_tokens)}</Td>
                    <Td align="right">{compact(bucket.cache_read_tokens)}</Td>
                    <Td align="right">{compact(bucket.cache_creation_tokens)}</Td>
                    <Td align="right">{usd(bucket.cost_input_usd)}</Td>
                    <Td align="right">{usd(bucket.cost_output_usd)}</Td>
                    <Td align="right">
                      {usd((bucket.cost_cache_write_usd ?? 0) + (bucket.cost_cache_read_usd ?? 0))}
                    </Td>
                    <Td align="right" className="font-medium">{usd(bucket.cost_total_usd)}</Td>
                    {/* Partial coverage is shown, never rounded away into the total. */}
                    <Td align="right" className={partial ? "text-warn" : "text-faint"}>
                      {percent(bucket.priced_share)}
                    </Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : <Empty label="Nothing priced yet" />}
      </Panel>
    </Page>
  );
}
