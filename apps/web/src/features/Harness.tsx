import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";

import {
  api,
  type DeclaredTool,
  type Harness as HarnessData,
  type SystemPromptSummary,
  type ToolSetSummary,
} from "@/api/client";
import {
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
import { UNKNOWN, ago, compact, percent, truncate } from "@/domain/format";
import { Page } from "@/features/Page";

const TABS = ["System prompts", "Tool sets", "Declared tools"] as const;
type Tab = (typeof TABS)[number];

/**
 * What the client wraps around every conversation.
 *
 * The other surfaces answer "what happened in this call". This one answers
 * "what was the model actually looking at" — the system prompt and the tool
 * block, which are re-sent in full on every single turn and are usually several
 * times larger than the conversation they carry.
 */
export function Harness() {
  const [tab, setTab] = React.useState<Tab>("System prompts");
  const [selected, setSelected] = React.useState<
    { kind: "system" | "tools"; hash: string } | null
  >(null);

  const query = useQuery({ queryKey: ["harness"], queryFn: api.harness });
  const data = query.data;

  return (
    <Page
      title="Harness"
      subtitle="What the client declares on every call, as opposed to what the conversation says"
      action={
        <div className="flex items-center gap-1">
          {TABS.map((name) => (
            <button
              key={name}
              onClick={() => {
                setTab(name);
                setSelected(null);
              }}
              className={cn(
                "h-6 rounded-sm border px-2 text-2xs",
                tab === name
                  ? "border-accent/50 bg-accent-soft/40 text-fg"
                  : "border-border text-muted hover:text-fg",
              )}
            >
              {name}
            </button>
          ))}
        </div>
      }
    >
      <Budget data={data} />

      <div
        className={cn(
          "grid gap-3",
          selected ? "lg:grid-cols-[minmax(0,1fr)_460px]" : "grid-cols-1",
        )}
      >
        <Panel title={tab}>
          {query.isLoading ? (
            <Loading />
          ) : query.error ? (
            <Failed error={query.error} />
          ) : !data ? null : tab === "System prompts" ? (
            <SystemPrompts
              rows={data.system_prompts}
              selected={selected?.kind === "system" ? selected.hash : null}
              onSelect={(hash) => setSelected({ kind: "system", hash })}
            />
          ) : tab === "Tool sets" ? (
            <ToolSets
              rows={data.tool_sets}
              selected={selected?.kind === "tools" ? selected.hash : null}
              onSelect={(hash) => setSelected({ kind: "tools", hash })}
            />
          ) : (
            <DeclaredTools rows={data.declared_tools} />
          )}
        </Panel>

        {selected?.kind === "system" && (
          <SystemDetail hash={selected.hash} onClose={() => setSelected(null)} />
        )}
        {selected?.kind === "tools" && (
          <ToolSetDetail hash={selected.hash} onClose={() => setSelected(null)} />
        )}
      </div>
    </Page>
  );
}

/* ── context budget ───────────────────────────────────────────────────── */

/**
 * Where the characters go across everything captured.
 *
 * Measured in characters rather than tokens on purpose: tokens are only known
 * for calls whose response was captured, and the split between system, tools
 * and history is never reported by the provider at all. Characters are exact
 * for every call, and the ratio is what matters here.
 */
function Budget({ data }: { data: HarnessData | undefined }) {
  const budget = data?.budget;
  const system = budget?.system_chars ?? 0;
  const tools = budget?.tools_chars ?? 0;
  const history = budget?.history_chars ?? 0;
  const total = system + tools + history;

  // Declared and never called: context spent on capability that went unused.
  const unused = (data?.declared_tools ?? []).filter((tool) => tool.calls === 0);
  const unusedChars = unused.reduce((sum, tool) => sum + tool.chars, 0);

  return (
    <>
      <div className="grid grid-cols-2 gap-2 lg:grid-cols-4">
        <Metric
          label="Tool declarations"
          value={percent(total > 0 ? tools / total : null)}
          hint={`${compact(tools)} chars of every request`}
          unavailable={total > 0 ? undefined : "nothing captured"}
          tone={total > 0 && tools / total > 0.5 ? "warn" : undefined}
        />
        <Metric
          label="System prompt"
          value={percent(total > 0 ? system / total : null)}
          hint={`${compact(system)} chars`}
          unavailable={total > 0 ? undefined : "nothing captured"}
        />
        <Metric
          label="Conversation"
          value={percent(total > 0 ? history / total : null)}
          hint={`${compact(history)} chars`}
          unavailable={total > 0 ? undefined : "nothing captured"}
        />
        <Metric
          label="Declared, never called"
          value={String(unused.length)}
          hint={`${compact(unusedChars)} chars per request carrying them`}
          tone={unused.length > 0 ? "warn" : "ok"}
        />
      </div>

      {total > 0 && (
        <Panel title="Where the context goes">
          <div className="p-3">
            <div className="flex h-3 overflow-hidden rounded-sm">
              <Share label="system" chars={system} total={total} className="bg-accent" />
              <Share label="tools" chars={tools} total={total} className="bg-warn" />
              <Share label="conversation" chars={history} total={total} className="bg-ok" />
            </div>
            <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-2xs text-faint">
              <Legend className="bg-accent" label="system prompt" chars={system} total={total} />
              <Legend className="bg-warn" label="tool declarations" chars={tools} total={total} />
              <Legend className="bg-ok" label="conversation" chars={history} total={total} />
            </div>
            <p className="mt-2 text-2xs text-faint">
              Every one of these is re-sent in full on every call. Only the
              conversation grows with the work; the other two are fixed overhead
              paid on each turn.
            </p>
          </div>
        </Panel>
      )}
    </>
  );
}

function Share({
  label,
  chars,
  total,
  className,
}: {
  label: string;
  chars: number;
  total: number;
  className: string;
}) {
  const share = total > 0 ? chars / total : 0;
  if (share <= 0) return null;
  return (
    <div
      className={className}
      style={{ width: `${share * 100}%` }}
      title={`${label}: ${compact(chars)} chars (${percent(share)})`}
    />
  );
}

function Legend({
  label,
  chars,
  total,
  className,
}: {
  label: string;
  chars: number;
  total: number;
  className: string;
}) {
  return (
    <span className="flex items-center gap-1.5 tnum">
      <span className={cn("size-2 rounded-xs", className)} aria-hidden />
      {label}
      <span className="text-muted">
        {percent(total > 0 ? chars / total : null)}
      </span>
    </span>
  );
}

/* ── system prompts ───────────────────────────────────────────────────── */

function SystemPrompts({
  rows,
  selected,
  onSelect,
}: {
  rows: SystemPromptSummary[];
  selected: string | null;
  onSelect: (hash: string) => void;
}) {
  if (!rows.length) {
    return (
      <Empty
        label="No system prompt captured"
        hint="Capture a call from an agent that declares one."
      />
    );
  }
  return (
    <Table>
      <thead>
        <tr>
          <Th>Opens with</Th>
          <Th>Agents</Th>
          <Th align="right">Chars</Th>
          <Th align="right">Segments</Th>
          <Th align="right">Cache pts</Th>
          <Th align="right">Calls</Th>
          <Th>Last seen</Th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <Tr
            key={row.system_hash}
            selected={selected === row.system_hash}
            onClick={() => onSelect(row.system_hash)}
          >
            <Td className="max-w-[360px]" title={row.opening ?? undefined}>
              {truncate(row.opening, 64) || (
                <span className="text-faint">{UNKNOWN}</span>
              )}
            </Td>
            <Td className="text-muted">{row.agent_roles ?? UNKNOWN}</Td>
            <Td align="right">{compact(row.total_chars)}</Td>
            <Td align="right">{row.segment_count}</Td>
            {/* Cache points are where the prompt can stop being re-billed. */}
            <Td align="right" className={row.cache_points ? "text-ok" : "text-warn"}>
              {row.cache_points}
            </Td>
            <Td align="right">{compact(row.generations)}</Td>
            <Td className="text-faint">{ago(row.last_seen)}</Td>
          </Tr>
        ))}
      </tbody>
    </Table>
  );
}

function SystemDetail({ hash, onClose }: { hash: string; onClose: () => void }) {
  const query = useQuery({
    queryKey: ["harness-system", hash],
    queryFn: () => api.harnessSystem(hash),
  });

  return (
    <Aside title="System prompt" hash={hash} onClose={onClose}>
      {query.isLoading ? (
        <Loading />
      ) : query.error ? (
        <Failed error={query.error} />
      ) : !query.data ? null : (
        <ul className="space-y-2">
          {query.data.system.segments.map((segment, index) => (
            <li
              key={index}
              className="rounded-sm border border-border bg-raised p-2"
            >
              <div className="flex items-center gap-2">
                <span className="text-2xs text-faint">segment {index + 1}</span>
                {segment.cache_control && (
                  <Pill tone="ok" title="A cache breakpoint sits after this segment">
                    cached
                  </Pill>
                )}
                <span className="ml-auto text-2xs text-faint tnum">
                  {compact(segment.chars)} chars
                </span>
              </div>
              {/* Rendered as text, never as markup: this is captured, untrusted. */}
              <pre className="mt-1.5 max-h-96 overflow-auto whitespace-pre-wrap break-words font-mono text-2xs text-muted">
                {segment.text}
              </pre>
            </li>
          ))}
        </ul>
      )}
    </Aside>
  );
}

/* ── tool sets ────────────────────────────────────────────────────────── */

function ToolSets({
  rows,
  selected,
  onSelect,
}: {
  rows: ToolSetSummary[];
  selected: string | null;
  onSelect: (hash: string) => void;
}) {
  if (!rows.length) {
    return <Empty label="No tools declared" hint="No captured call declared any." />;
  }
  return (
    <Table>
      <thead>
        <tr>
          <Th>Fingerprint</Th>
          <Th>Agents</Th>
          <Th align="right">Tools</Th>
          <Th align="right">MCP</Th>
          <Th align="right">Chars</Th>
          <Th align="right">Calls</Th>
          <Th>Last seen</Th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <Tr
            key={row.tools_hash}
            selected={selected === row.tools_hash}
            onClick={() => onSelect(row.tools_hash)}
          >
            <Td>
              <code className="font-mono text-2xs text-muted">
                {row.tools_hash.slice(0, 12)}
              </code>
            </Td>
            <Td className="text-muted">{row.agent_roles ?? UNKNOWN}</Td>
            <Td align="right">{row.tool_count}</Td>
            <Td align="right" className="text-faint">
              {row.mcp_count}
            </Td>
            <Td align="right">{compact(row.total_chars)}</Td>
            <Td align="right">{compact(row.generations)}</Td>
            <Td className="text-faint">{ago(row.last_seen)}</Td>
          </Tr>
        ))}
      </tbody>
    </Table>
  );
}

function ToolSetDetail({ hash, onClose }: { hash: string; onClose: () => void }) {
  const query = useQuery({
    queryKey: ["harness-tools", hash],
    queryFn: () => api.harnessTools(hash),
  });
  const [open, setOpen] = React.useState<string | null>(null);

  return (
    <Aside title="Tool set" hash={hash} onClose={onClose}>
      {query.isLoading ? (
        <Loading />
      ) : query.error ? (
        <Failed error={query.error} />
      ) : !query.data ? null : (
        <ul className="space-y-1.5">
          {query.data.tools.map((tool) => (
            <li key={tool.name} className="rounded-sm border border-border bg-raised">
              <button
                onClick={() => setOpen(open === tool.name ? null : tool.name)}
                className="flex w-full items-center gap-2 p-2 text-left"
              >
                <span className="truncate font-mono text-xs">{tool.name}</span>
                {tool.is_mcp ? <Pill>mcp</Pill> : null}
                {/* Weight against use: the comparison this surface exists for. */}
                {tool.calls === 0 && (
                  <Pill tone="warn" title="Declared on every call, never invoked">
                    unused
                  </Pill>
                )}
                <span className="ml-auto shrink-0 text-2xs text-faint tnum">
                  {compact(tool.chars)} ch · {tool.calls} calls
                </span>
              </button>
              {open === tool.name && (
                <div className="border-t border-border p-2">
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words font-mono text-2xs text-muted">
                    {tool.description ?? "No description declared."}
                  </pre>
                  {tool.input_schema != null && (
                    <pre className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap break-all font-mono text-2xs text-faint">
                      {JSON.stringify(tool.input_schema, null, 2)}
                    </pre>
                  )}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </Aside>
  );
}

/* ── declared tools ───────────────────────────────────────────────────── */

/**
 * Every declared tool ranked by cost against use, unused and heaviest first —
 * the order the API returns, which is the order worth acting on.
 */
function DeclaredTools({ rows }: { rows: DeclaredTool[] }) {
  if (!rows.length) return <Empty label="No tools declared" />;
  return (
    <Table>
      <thead>
        <tr>
          <Th>Tool</Th>
          <Th>Server</Th>
          <Th align="right">Chars</Th>
          <Th align="right">Calls</Th>
          <Th align="right">In sets</Th>
          <Th>Last used</Th>
        </tr>
      </thead>
      <tbody>
        {rows.map((tool) => (
          <Tr key={tool.name}>
            <Td className="max-w-[320px]">
              <span className="font-mono text-xs">{tool.name}</span>
            </Td>
            <Td className="text-faint">{tool.server ?? (tool.is_mcp ? UNKNOWN : "")}</Td>
            <Td align="right">{compact(tool.chars)}</Td>
            <Td align="right" className={tool.calls === 0 ? "text-warn" : undefined}>
              {tool.calls === 0 ? "never" : compact(tool.calls)}
            </Td>
            <Td align="right" className="text-faint">
              {tool.tool_sets}
            </Td>
            <Td className="text-faint">
              {tool.last_used ? ago(tool.last_used) : UNKNOWN}
            </Td>
          </Tr>
        ))}
      </tbody>
    </Table>
  );
}

/* ── shared detail shell ──────────────────────────────────────────────── */

function Aside({
  title,
  hash,
  onClose,
  children,
}: {
  title: string;
  hash: string;
  onClose: () => void;
  children: React.ReactNode;
}) {
  return (
    <Panel
      className="h-fit lg:sticky lg:top-3"
      title={
        <div className="flex items-center gap-2">
          <span>{title}</span>
          <code className="font-mono text-2xs text-faint">{hash.slice(0, 12)}</code>
        </div>
      }
      action={
        <button onClick={onClose} aria-label="Close detail" className="text-faint hover:text-fg">
          <X size={13} />
        </button>
      }
    >
      <div className="max-h-[70vh] overflow-y-auto p-3 text-sm">{children}</div>
    </Panel>
  );
}
