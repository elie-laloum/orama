import * as React from "react";
import { NavLink, Outlet } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Activity,
  AlertTriangle,
  Boxes,
  DollarSign,
  LayoutDashboard,
  ListTree,
  MessagesSquare,
  ScrollText,
  Wrench,
} from "lucide-react";

import { api, subscribe } from "@/api/client";
import { cn } from "@/components/ui";
import { num, percent } from "@/domain/format";

const NAV: {
  to: string;
  label: string;
  icon: typeof LayoutDashboard;
  end?: boolean;
}[] = [
  { to: "/", label: "Dashboard", icon: LayoutDashboard, end: true },
  { to: "/traces", label: "Traces", icon: ListTree },
  { to: "/generations", label: "Generations", icon: Activity },
  { to: "/sessions", label: "Sessions", icon: MessagesSquare },
  { to: "/errors", label: "Errors", icon: AlertTriangle },
  { to: "/tools", label: "Tools", icon: Wrench },
  { to: "/harness", label: "Harness", icon: ScrollText },
  { to: "/cost", label: "Cost", icon: DollarSign },
];

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
        <div className="flex h-11 items-center gap-2 border-b border-border px-3">
          <Boxes size={15} className="text-accent" aria-hidden />
          <span className="text-sm font-semibold tracking-tight">orama</span>
          <span className="ml-auto text-2xs text-faint">local</span>
        </div>

        <ul className="flex-1 space-y-px p-1.5">
          {NAV.map(({ to, label, icon: Icon, end }) => (
            <li key={to}>
              <NavLink
                to={to}
                end={end}
                className={({ isActive }) =>
                  cn(
                    "flex h-7 items-center gap-2 rounded-sm px-2 text-sm",
                    isActive
                      ? "bg-raised text-fg"
                      : "text-muted hover:bg-raised/60 hover:text-fg",
                  )
                }
              >
                <Icon size={14} aria-hidden />
                {label}
              </NavLink>
            </li>
          ))}
        </ul>

        <div className="space-y-1.5 border-t border-border px-3 py-2.5 text-2xs text-faint">
          <Counts meta={meta} />
          <div className="flex items-center gap-1.5 pt-1">
            <span
              className={cn(
                "size-1.5 rounded-full",
                live ? "bg-ok" : "bg-unknown",
              )}
              aria-hidden
            />
            {live ? "live" : "reconnecting"}
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
