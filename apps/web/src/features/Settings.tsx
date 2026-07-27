import * as React from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Check,
  CircleSlash,
  Copy,
  Link2,
  Link2Off,
  Loader2,
  RotateCw,
  TriangleAlert,
} from "lucide-react";

import {
  api,
  type Connector,
  type Guide,
  type ProxyState,
  type Settings as SettingsData,
} from "@/api/client";
import { Failed, Loading, Panel, Pill, cn } from "@/components/ui";
import { UNKNOWN, ago, clock, compact } from "@/domain/format";
import { Page } from "@/features/Page";

/**
 * Where the proxy is, and what is actually pointed at it.
 *
 * Every other surface answers a question about traffic that was captured. This
 * one answers the question you have *before* there is any: is anything routed
 * through here at all? A proxy nobody configured and a proxy nobody used look
 * identical from the dashboard, so connection state is read from the harness
 * config files themselves rather than inferred from an empty capture.
 */
export function Settings() {
  const query = useQuery({ queryKey: ["settings"], queryFn: api.settings });

  return (
    <Page
      title="Settings"
      subtitle="Proxy state, and what is routed through it"
      action={
        <button
          onClick={() => query.refetch()}
          disabled={query.isFetching}
          className="flex h-6 items-center gap-1.5 rounded-sm border border-border px-2 text-2xs text-muted hover:text-fg disabled:opacity-50"
        >
          <RotateCw
            size={11}
            className={cn(query.isFetching && "animate-spin")}
            aria-hidden
          />
          Re-read
        </button>
      }
    >
      {query.isLoading ? (
        <Loading />
      ) : query.error ? (
        <Failed error={query.error} />
      ) : !query.data ? null : (
        <SettingsBody data={query.data} />
      )}
    </Page>
  );
}

function SettingsBody({ data }: { data: SettingsData }) {
  return (
    <>
      <Proxy proxy={data.proxy} />
      <Connectors connectors={data.connectors} />
      <Catalog catalog={data.proxy.catalog} />
      <Guides guides={data.guides} />
    </>
  );
}

/* ── proxy state ──────────────────────────────────────────────────────── */

function Proxy({ proxy }: { proxy: ProxyState }) {
  return (
    <Panel
      title="Proxy"
      action={
        proxy.capturing ? (
          <Pill tone="ok">capturing</Pill>
        ) : (
          <Pill tone="error" title="The database could not be opened at startup">
            relaying only
          </Pill>
        )
      }
    >
      {!proxy.capturing && (
        <p className="flex items-start gap-2 border-b border-border bg-error/5 px-3 py-2 text-2xs text-error">
          <TriangleAlert size={12} className="mt-px shrink-0" aria-hidden />
          Traffic is being forwarded but nothing is being recorded — the capture
          database could not be opened. Every screen below will stay empty.
        </p>
      )}
      <dl className="grid grid-cols-1 gap-x-6 gap-y-px p-3 sm:grid-cols-2">
        <Field label="Listening on" value={proxy.listening_on} mono />
        {proxy.bridged_on?.length > 0 && (
          // Only shown when there is one, because it is not a general fact
          // about the proxy — it is the address a WSL guest reaches it on, and
          // on most machines there is no such address.
          <Field
            label="Bridged to WSL"
            value={proxy.bridged_on.join(", ")}
            title="A harness inside WSL cannot reach the Windows loopback, so the proxy also listens here."
            mono
          />
        )}
        <Field
          label="Last capture"
          value={proxy.last_capture_at ? ago(proxy.last_capture_at) : "never"}
          title={proxy.last_capture_at ? clock(proxy.last_capture_at) : undefined}
          // "never" is a fact; unknown would be a dash. The API distinguishes
          // them and so does this.
          muted={!proxy.last_capture_at}
        />
        <Field label="Anthropic upstream" value={proxy.upstream_anthropic} mono />
        <Field label="OpenAI upstream" value={proxy.upstream_openai} mono />
        <Field label="ChatGPT upstream" value={proxy.upstream_chatgpt} mono />
        <Field label="Capture database" value={proxy.db_path} mono />
        <Field
          label="Database size"
          value={proxy.db_bytes == null ? UNKNOWN : `${compact(proxy.db_bytes)}B`}
        />
        <Field label="Running since" value={ago(proxy.started_at)} title={clock(proxy.started_at)} />
        <Field
          label="Versions"
          value={`parser ${proxy.parser_version} · pricing ${proxy.pricing_version} · policy ${proxy.policy_version}`}
        />
      </dl>
    </Panel>
  );
}

/**
 * Where the prices come from, and how to move them.
 *
 * Rates are not maintained in this repo — they are a cached copy of what
 * models.dev publishes. That makes their age a fact worth showing: a stale
 * catalogue prices calls at rates nobody is charging any more.
 */
function Catalog({ catalog }: { catalog: ProxyState["catalog"] }) {
  const client = useQueryClient();
  const [outcome, setOutcome] = React.useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: () => api.refreshCatalog(),
    onSuccess: (result) => {
      setOutcome(
        result.changed
          ? `Updated to ${result.catalog?.models ?? 0} models. Existing captures are being re-priced.`
          : "Already current — the published rates have not changed.",
      );
      client.invalidateQueries();
    },
    onError: () => setOutcome(null),
  });

  return (
    <Panel
      title="Model catalogue"
      action={
        catalog?.source === "embedded" ? (
          <Pill tone="warn" title="Nothing fresher could be loaded">
            bundled copy
          </Pill>
        ) : catalog ? (
          <Pill tone="ok">{catalog.source}</Pill>
        ) : (
          <Pill tone="error" title="No rates are loaded, so nothing can be priced">
            unavailable
          </Pill>
        )
      }
    >
      {!catalog && (
        <p className="flex items-start gap-2 border-b border-border bg-error/5 px-3 py-2 text-2xs text-error">
          <TriangleAlert size={12} className="mt-px shrink-0" aria-hidden />
          No catalogue could be loaded, so no call can be priced. Every cost on
          every screen will read {UNKNOWN} until this is fixed.
        </p>
      )}
      <dl className="grid grid-cols-1 gap-x-6 gap-y-px p-3 sm:grid-cols-2">
        <Field label="Source" value="models.dev" mono />
        <Field label="Models" value={catalog ? compact(catalog.models) : UNKNOWN} />
        <Field label="Providers" value={catalog ? compact(catalog.providers) : UNKNOWN} />
        <Field label="Snapshot" value={catalog ? catalog.digest.slice(0, 12) : UNKNOWN} mono />
        <Field
          label="Downloaded"
          value={catalog ? ago(catalog.fetched_at) : UNKNOWN}
          title={catalog ? clock(catalog.fetched_at) : undefined}
        />
        <Field
          label="Last checked"
          value={catalog ? ago(catalog.checked_at) : UNKNOWN}
          title={catalog ? clock(catalog.checked_at) : undefined}
        />
      </dl>
      <div className="flex items-center gap-3 border-t border-border px-3 py-2">
        <button
          type="button"
          disabled={mutation.isPending}
          onClick={() => {
            setOutcome(null);
            mutation.mutate();
          }}
          className="flex h-7 items-center gap-1.5 rounded-sm border border-border px-2 text-xs text-fg hover:bg-raised disabled:opacity-50"
        >
          {mutation.isPending ? (
            <Loader2 size={12} className="animate-spin" aria-hidden />
          ) : (
            <RotateCw size={12} aria-hidden />
          )}
          Check for new rates
        </button>
        {mutation.error ? (
          <span className="text-2xs text-error">{String(mutation.error)}</span>
        ) : outcome ? (
          <span className="text-2xs text-ok">{outcome}</span>
        ) : (
          <span className="text-2xs text-faint">
            Checked automatically once a day.
          </span>
        )}
      </div>
    </Panel>
  );
}

function Field({
  label,
  value,
  mono,
  muted,
  title,
}: {
  label: string;
  value: string;
  mono?: boolean;
  muted?: boolean;
  title?: string;
}) {
  return (
    <div className="flex items-baseline justify-between gap-3 border-b border-border/40 py-1.5 last:border-0">
      <dt className="shrink-0 text-2xs uppercase tracking-wider text-faint">
        {label}
      </dt>
      <dd
        title={title ?? value}
        className={cn(
          "truncate text-xs",
          mono && "font-mono",
          muted ? "text-unknown" : "text-muted",
        )}
      >
        {value}
      </dd>
    </div>
  );
}

/* ── connectors ───────────────────────────────────────────────────────── */

function Connectors({ connectors }: { connectors: Connector[] }) {
  return (
    <Panel title="Connectors">
      <p className="border-b border-border px-3 py-2 text-2xs text-faint">
        Edits the harness's own config file in place. Your existing credentials
        are forwarded untouched, and auth headers are redacted before anything
        is written to disk.
      </p>
      <ul>
        {connectors.map((connector) => (
          <ConnectorRow key={connector.id} connector={connector} />
        ))}
      </ul>
    </Panel>
  );
}

function ConnectorRow({ connector }: { connector: Connector }) {
  const client = useQueryClient();
  const [outcome, setOutcome] = React.useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: (action: "connect" | "disconnect") =>
      action === "connect" ? api.connect(connector.id) : api.disconnect(connector.id),
    onSuccess: (result) => {
      setOutcome(
        result.already
          ? "Already in that state — nothing was changed."
          : result.backup_path
            ? `Saved. Original copied to ${result.backup_path}`
            : "Saved.",
      );
      client.invalidateQueries({ queryKey: ["settings"] });
    },
    onError: () => setOutcome(null),
  });

  // What connecting writes depends on Codex's auth mode and on which side of a
  // WSL boundary the harness sits, so the server reports it outright. This used
  // to be reconstructed here from the connector id, which stopped being
  // sufficient the moment one harness could exist at more than one place.
  const expected = connector.expected_base_url;
  // A base URL that is neither ours nor absent belongs to someone else, and
  // connecting would silently take it over. Say so rather than just doing it.
  const foreign =
    !connector.connected && connector.base_url != null && connector.base_url !== "";

  return (
    <li className="border-b border-border last:border-0">
      <div className="flex items-start gap-3 px-3 py-2.5">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">{connector.label}</span>
            {connector.site === "wsl" && (
              <Pill
                tone="unknown"
                title="This harness lives inside a WSL distribution, and is reached across that boundary."
              >
                wsl
              </Pill>
            )}
            {connector.error ? (
              <Pill tone="error">unreadable</Pill>
            ) : connector.connected ? (
              <Pill tone="ok">connected</Pill>
            ) : foreign ? (
              <Pill tone="warn">points elsewhere</Pill>
            ) : (
              <Pill tone="unknown">not connected</Pill>
            )}
            {connector.connected && connector.restart_required && (
              <span className="text-2xs text-faint">restart to take effect</span>
            )}
          </div>

          <p className="mt-1 truncate font-mono text-2xs text-faint" title={connector.config_path}>
            {connector.config_path}
            {!connector.config_exists && " (will be created)"}
          </p>

          {connector.error ? (
            <p className="mt-1 text-2xs text-error">{connector.error}</p>
          ) : foreign ? (
            <p className="mt-1 text-2xs text-warn">
              Currently points at{" "}
              <span className="font-mono">{connector.base_url}</span>. Connecting
              replaces that, and disconnecting puts it back.
            </p>
          ) : (
            <p className="mt-1 text-2xs text-faint">
              {connector.connected ? (
                <>
                  Routed through <span className="font-mono">{expected}</span>
                </>
              ) : (
                connector.effect
              )}
            </p>
          )}

          {connector.caveat && !connector.error && (
            <p className="mt-1 flex items-start gap-1.5 text-2xs text-warn">
              <TriangleAlert size={11} className="mt-px shrink-0" aria-hidden />
              {connector.caveat}
            </p>
          )}

          {mutation.error && (
            <p className="mt-1 text-2xs text-error">
              {mutation.error instanceof Error
                ? mutation.error.message
                : String(mutation.error)}
            </p>
          )}
          {outcome && !mutation.error && (
            <p className="mt-1 text-2xs text-ok">{outcome}</p>
          )}
        </div>

        <button
          // No expected base URL means there is no address this harness could
          // reach us on, so connecting could only write a config that fails
          // every request. Disconnecting stays available: something may still
          // need putting back.
          disabled={
            mutation.isPending ||
            !!connector.error ||
            (!connector.connected && expected == null)
          }
          title={
            !connector.connected && expected == null
              ? "There is no address this harness could reach the proxy on."
              : undefined
          }
          onClick={() => {
            setOutcome(null);
            mutation.mutate(connector.connected ? "disconnect" : "connect");
          }}
          className={cn(
            "flex h-7 shrink-0 items-center gap-1.5 rounded-sm border px-2.5 text-2xs",
            connector.connected
              ? "border-border text-muted hover:text-fg"
              : "border-accent/50 bg-accent-soft/40 text-fg hover:bg-accent-soft/60",
            "disabled:cursor-not-allowed disabled:opacity-50",
          )}
        >
          {mutation.isPending ? (
            <Loader2 size={11} className="animate-spin" aria-hidden />
          ) : connector.error ? (
            <CircleSlash size={11} aria-hidden />
          ) : connector.connected ? (
            <Link2Off size={11} aria-hidden />
          ) : (
            <Link2 size={11} aria-hidden />
          )}
          {connector.connected ? "Disconnect" : "Connect"}
        </button>
      </div>
    </li>
  );
}

/* ── guides ───────────────────────────────────────────────────────────── */

/**
 * Clients with no config file at a known path. There is nothing to click, so
 * the honest thing is to hand over the exact line to paste.
 */
function Guides({ guides }: { guides: Guide[] }) {
  const [open, setOpen] = React.useState<string>(guides[0]?.id ?? "");

  return (
    <Panel
      title="Connect anything else"
      action={
        <div className="flex items-center gap-1">
          {guides.map((guide) => (
            <button
              key={guide.id}
              onClick={() => setOpen(guide.id)}
              className={cn(
                "h-6 rounded-sm border px-2 text-2xs",
                open === guide.id
                  ? "border-accent/50 bg-accent-soft/40 text-fg"
                  : "border-border text-muted hover:text-fg",
              )}
            >
              {guide.label}
            </button>
          ))}
        </div>
      }
    >
      {guides
        .filter((guide) => guide.id === open)
        .map((guide) => (
          <div key={guide.id} className="space-y-2 p-3">
            <p className="text-xs text-muted">{guide.summary}</p>
            {guide.snippets.map((snippet) => (
              <Snippet
                key={snippet.label}
                label={snippet.label}
                code={snippet.code}
              />
            ))}
            <p className="text-2xs text-faint">{guide.note}</p>
          </div>
        ))}
    </Panel>
  );
}

function Snippet({ label, code }: { label: string; code: string }) {
  const [copied, setCopied] = React.useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard access can be refused; the text is on screen and selectable,
      // so there is nothing to recover from and nothing worth alarming about.
    }
  };

  return (
    <div className="overflow-hidden rounded-sm border border-border">
      <div className="flex h-7 items-center justify-between gap-2 border-b border-border bg-raised/40 px-2">
        <span className="text-2xs uppercase tracking-wider text-faint">
          {label}
        </span>
        <button
          onClick={copy}
          className="flex items-center gap-1 text-2xs text-muted hover:text-fg"
        >
          {copied ? (
            <Check size={11} className="text-ok" aria-hidden />
          ) : (
            <Copy size={11} aria-hidden />
          )}
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      <pre className="overflow-x-auto px-2.5 py-2 font-mono text-2xs leading-relaxed text-muted">
        {code}
      </pre>
    </div>
  );
}
