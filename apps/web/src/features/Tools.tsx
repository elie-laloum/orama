import { useQuery } from "@tanstack/react-query";

import { api } from "@/api/client";
import {
  Empty, Failed, Loading, Panel, Pill, Table, Td, Th, Tr,
} from "@/components/ui";
import { ago, compact, percent } from "@/domain/format";
import { Page } from "@/features/Page";

export function Tools() {
  const query = useQuery({ queryKey: ["tools"], queryFn: api.tools });

  return (
    <Page title="Tools" subtitle="What the agents actually ran, and how often it failed">
      <Panel>
        {query.isLoading ? <Loading /> : query.error ? <Failed error={query.error} /> :
         query.data?.tools.length ? (
          <Table>
            <thead>
              <tr>
                <Th>Tool</Th><Th align="right">Calls</Th><Th align="right">Errors</Th>
                <Th align="right">Failure rate</Th><Th align="right">Unresolved</Th>
                <Th align="right">Avg result</Th><Th align="right">Largest</Th>
                <Th align="right">Sessions</Th><Th align="right">Last used</Th>
              </tr>
            </thead>
            <tbody>
              {query.data.tools.map((tool) => {
                const rate = tool.error_rate ?? 0;
                return (
                  <Tr key={`${tool.name}:${tool.server ?? ""}`}>
                    <Td>
                      <span className="font-mono text-xs">{tool.name}</span>
                      {tool.is_mcp ? <span className="ml-1.5"><Pill>mcp</Pill></span> : null}
                      {tool.undeclared ? (
                        <span className="ml-1.5"><Pill tone="warn">undeclared</Pill></span>
                      ) : null}
                    </Td>
                    <Td align="right">{compact(tool.calls)}</Td>
                    <Td align="right" className={tool.errors ? "text-error" : undefined}>
                      {tool.errors ? compact(tool.errors) : ""}
                    </Td>
                    <Td align="right" className={rate >= 0.3 ? "text-error" : rate > 0 ? "text-warn" : "text-faint"}>
                      {percent(tool.error_rate, 1)}
                    </Td>
                    {/* A tool_use with no matching result: the turn ended before it came back. */}
                    <Td align="right" className={tool.pending ? "text-warn" : "text-faint"}>
                      {tool.pending ? compact(tool.pending) : ""}
                    </Td>
                    <Td align="right">{compact(tool.avg_result_chars)}</Td>
                    <Td align="right">{compact(tool.max_result_chars)}</Td>
                    <Td align="right">{compact(tool.sessions)}</Td>
                    <Td align="right" className="text-faint">{ago(tool.last_used)}</Td>
                  </Tr>
                );
              })}
            </tbody>
          </Table>
        ) : <Empty label="No tool calls captured yet" />}
      </Panel>
    </Page>
  );
}
