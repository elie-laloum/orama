import * as React from "react";
import { NavLink, Outlet } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  AlertTriangle,
  DollarSign,
  Layers,
  LayoutDashboard,
  ListTree,
  MessagesSquare,
  ScrollText,
  SlidersHorizontal,
  Wrench,
} from "lucide-react";

import { api, subscribe } from "@/api/client";
import { cn } from "@/components/ui";
import { num, percent } from "@/domain/format";

type NavItem = {
  to: string;
  label: string;
  icon: typeof LayoutDashboard;
  end?: boolean;
};

const NAV: NavItem[] = [
  { to: "/", label: "Dashboard", icon: LayoutDashboard, end: true },
  { to: "/traces", label: "Traces", icon: ListTree },
  { to: "/generations", label: "Generations", icon: Activity },
  { to: "/sessions", label: "Sessions", icon: MessagesSquare },
  { to: "/errors", label: "Errors", icon: AlertTriangle },
  { to: "/tools", label: "Tools", icon: Wrench },
  { to: "/harness", label: "Harness", icon: ScrollText },
  { to: "/cost", label: "Cost", icon: DollarSign },
  { to: "/models", label: "Models", icon: Layers },
];

const SETTINGS: NavItem = {
  to: "/settings",
  label: "Settings",
  icon: SlidersHorizontal,
};

const linkClass = ({ isActive }: { isActive: boolean }) =>
  cn(
    "flex h-7 items-center gap-2 rounded-sm px-2 text-sm",
    isActive ? "bg-raised text-fg" : "text-muted hover:bg-raised/60 hover:text-fg",
  );

export function AppShell() {
  const client = useQueryClient();
  const [live, setLive] = React.useState(false);
  const { data: meta } = useQuery({ queryKey: ["meta"], queryFn: api.meta });

  // The stream carries notifications, not state: any event simply invalidates
  // the cache and the visible queries re-fetch.
  React.useEffect(
    () => subscribe(() => client.invalidateQueries(), setLive),
    [client],
  );

  return (
    <div className="flex h-full">
      <nav className="flex w-[196px] shrink-0 flex-col border-r border-border bg-surface">
        {/* The wordmark and its logo used to sit here, repeating the window's
            own title and icon. What the corner is worth is the one fact no
            title bar carries: whether traffic is flowing through this proxy
            right now — and the switch that decides it. */}
        <div className="flex h-11 items-center border-b border-border px-3">
          <ProxyToggle />
          <span className="ml-auto text-2xs text-faint">local</span>
        </div>

        <ul className="flex-1 space-y-px p-1.5">
          {NAV.map(({ to, label, icon: Icon, end }) => (
            <li key={to}>
              <NavLink to={to} end={end} className={linkClass}>
                <Icon size={14} aria-hidden />
                {label}
              </NavLink>
            </li>
          ))}
        </ul>

        {/* Pinned below the analytics surfaces: it configures the proxy rather
            than reporting on it, and it is where a first-run user starts. */}
        <div className="p-1.5">
          <NavLink to={SETTINGS.to} className={linkClass}>
            <SETTINGS.icon size={14} aria-hidden />
            {SETTINGS.label}
            <ConnectionDot />
          </NavLink>
        </div>

        <div className="space-y-1.5 border-t border-border px-3 py-2.5 text-2xs text-faint">
          <Counts meta={meta} />
          {/* The event stream, not the proxy. Worded as "stream" since the
              badge above owns "live": two indicators both saying live, meaning
              different things, is how you learn to trust neither. */}
          <div className="flex items-center gap-1.5 pt-1">
            <span
              className={cn(
                "size-1.5 rounded-full",
                live ? "bg-ok" : "bg-unknown",
              )}
              aria-hidden
            />
            {live ? "stream" : "reconnecting"}
          </div>
        </div>
      </nav>

      <main className="min-w-0 flex-1 overflow-y-auto">
        <Outlet />
      </main>
    </div>
  );
}

/**
 * Whether the relay is forwarding, and the switch that decides it.
 *
 * Stopping keeps the port and the dashboard — only forwarding stops, and every
 * proxied request is refused with a 503 until it is started again. That is a
 * deliberate break rather than a quiet one: a harness pointed here has no other
 * way to learn that the thing in its path went away.
 *
 * The state is never persisted, so this reads as running on every launch. A
 * proxy that came back refusing traffic because of a click from a previous
 * session would be indistinguishable from a broken install.
 */
function ProxyToggle() {
  const client = useQueryClient();
  const { data } = useQuery({ queryKey: ["settings"], queryFn: api.settings });
  const toggle = useMutation({
    mutationFn: api.setProxyRunning,
    onSuccess: () => client.invalidateQueries({ queryKey: ["settings"] }),
  });

  // While the round trip is in flight the badge shows where it is going, not
  // where it has been — the click is the only feedback there is otherwise.
  const running = toggle.isPending ? toggle.variables : data?.proxy.running;

  if (running == null) {
    return (
      <span className="flex items-center gap-2 text-sm text-faint">
        <span className="size-2 rounded-full bg-unknown" aria-hidden />
        checking
      </span>
    );
  }

  return (
    <button
      type="button"
      onClick={() => toggle.mutate(!running)}
      disabled={toggle.isPending}
      aria-pressed={running}
      title={
        toggle.error
          ? String(toggle.error)
          : running
            ? "Relaying and capturing. Click to stop — connected harnesses will start failing."
            : "Not relaying: every request through this proxy is refused. Click to start."
      }
      className={cn(
        "group -ml-1 flex items-center gap-2 rounded-sm px-1 py-1 text-sm font-medium",
        "hover:bg-raised disabled:opacity-60",
        running ? "text-ok" : "text-error",
      )}
    >
      <span className="relative flex size-2 shrink-0" aria-hidden>
        <span
          className={cn(
            "absolute inline-flex size-full animate-ping rounded-full opacity-60",
            running ? "bg-ok" : "bg-error",
          )}
        />
        <span
          className={cn(
            "relative inline-flex size-2 rounded-full",
            running ? "bg-ok" : "bg-error",
          )}
        />
      </span>
      {running ? "live" : "stopped"}
      <span className="text-2xs text-faint opacity-0 group-hover:opacity-100">
        {running ? "stop" : "start"}
      </span>
    </button>
  );
}

/**
 * A quiet marker for "nothing is routed through this proxy".
 *
 * Worth surfacing in the nav because the failure it warns about is silent: an
 * unconfigured proxy produces an empty dashboard, which reads as "no traffic
 * yet" rather than "the harness never knew about you".
 */
function ConnectionDot() {
  const { data } = useQuery({ queryKey: ["settings"], queryFn: api.settings });
  if (!data || data.connectors.some((connector) => connector.connected)) {
    return null;
  }
  return (
    <span
      title="No harness is pointed at this proxy"
      className="ml-auto size-1.5 rounded-full bg-warn"
      aria-hidden
    />
  );
}

/**
 * Corpus size and how much of it is measured. Coverage is shown next to the
 * counts rather than buried, because every total on every screen is only as
 * complete as this number.
 */
function Counts({ meta }: { meta: Awaited<ReturnType<typeof api.meta>> | undefined }) {
  if (!meta) return null;
  const gaps = meta.usage_coverage != null && meta.usage_coverage < 1;
  return (
    <>
      <div className="flex justify-between tnum">
        <span>generations</span>
        <span className="text-muted">{num(meta.generations)}</span>
      </div>
      <div className="flex justify-between tnum">
        <span>traces</span>
        <span className="text-muted">{num(meta.traces)}</span>
      </div>
      <div
        className="flex justify-between tnum"
        title={
          gaps
            ? "Some captures have no usage data, so totals are partial"
            : "Every capture has usage data"
        }
      >
        <span>coverage</span>
        <span className={gaps ? "text-warn" : "text-ok"}>
          {percent(meta.usage_coverage)}
        </span>
      </div>
      {meta.derive_failures > 0 && (
        <div className="flex justify-between tnum text-error">
          <span>derive failures</span>
          <span>{num(meta.derive_failures)}</span>
        </div>
      )}
    </>
  );
}
