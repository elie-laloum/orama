/**
 * Shared primitives.
 *
 * Severity is never carried by colour alone — every badge pairs a hue with an
 * icon and a word, so the meaning survives a monochrome screen or a reader who
 * cannot distinguish the hues.
 */
import * as React from "react";
import { clsx } from "clsx";
import {
  AlertTriangle,
  CircleAlert,
  CircleHelp,
  Info,
  Loader2,
  OctagonAlert,
} from "lucide-react";

import type { Severity } from "@/api/client";
import { UNKNOWN } from "@/domain/format";

export function cn(...parts: Parameters<typeof clsx>) {
  return clsx(...parts);
}

/* ── layout ───────────────────────────────────────────────────────────── */

export function Panel({
  title,
  action,
  children,
  className,
}: {
  title?: React.ReactNode;
  action?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <section
      className={cn(
        "rounded-md border border-border bg-surface overflow-hidden",
        className,
      )}
    >
      {title && (
        <header className="flex items-center justify-between gap-3 border-b border-border px-3 h-9">
          <h2 className="text-xs font-medium uppercase tracking-wider text-muted">
            {title}
          </h2>
          {action}
        </header>
      )}
      {children}
    </section>
  );
}

/**
 * A headline number. `unavailable` renders the unknown mark in its own hue
 * rather than a zero, and says why.
 */
export function Metric({
  label,
  value,
  hint,
  unavailable,
  tone,
}: {
  label: string;
  value: React.ReactNode;
  hint?: React.ReactNode;
  unavailable?: string;
  tone?: "ok" | "warn" | "error";
}) {
  const toneClass =
    tone === "error"
      ? "text-error"
      : tone === "warn"
        ? "text-warn"
        : tone === "ok"
          ? "text-ok"
          : "text-fg";
  return (
    <div className="rounded-md border border-border bg-surface px-3 py-2.5">
      <div className="text-2xs uppercase tracking-wider text-faint">{label}</div>
      {unavailable ? (
        <div className="mt-1 flex items-baseline gap-1.5">
          <span className="text-xl text-unknown tnum">{UNKNOWN}</span>
          <span className="text-2xs text-unknown">{unavailable}</span>
        </div>
      ) : (
        <div className={cn("mt-1 text-xl tnum leading-none", toneClass)}>{value}</div>
      )}
      {hint && !unavailable && (
        <div className="mt-1 text-2xs text-faint tnum">{hint}</div>
      )}
    </div>
  );
}

/* ── status ───────────────────────────────────────────────────────────── */

const SEVERITY = {
  critical: { icon: OctagonAlert, label: "Critical", cls: "text-critical bg-critical/10 border-critical/30" },
  error: { icon: CircleAlert, label: "Error", cls: "text-error bg-error/10 border-error/30" },
  warning: { icon: AlertTriangle, label: "Warning", cls: "text-warn bg-warn/10 border-warn/30" },
  info: { icon: Info, label: "Info", cls: "text-muted bg-raised border-border" },
} as const;

export function SeverityBadge({
  severity,
  count,
}: {
  severity: Severity;
  count?: number;
}) {
  const spec = SEVERITY[severity] ?? SEVERITY.info;
  const Icon = spec.icon;
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-sm border px-1.5 py-0.5 text-2xs font-medium",
        spec.cls,
      )}
    >
      <Icon size={11} aria-hidden />
      {spec.label}
      {count != null && <span className="tnum opacity-70">{count}</span>}
    </span>
  );
}

/**
 * How much to trust a value. `unavailable` is the important one: it marks a
 * finding derived from data we do not have, so it is never read as a fact.
 */
export function ConfidenceBadge({ confidence }: { confidence: string }) {
  if (confidence === "exact") return null;
  const unavailable = confidence === "unavailable";
  return (
    <span
      className={cn(
        "inline-flex items-center gap-1 rounded-sm border px-1.5 py-0.5 text-2xs",
        unavailable
          ? "border-unknown/40 text-unknown"
          : "border-border text-faint",
      )}
      title={
        unavailable
          ? "Derived from data that was not captured"
          : "Inferred, not directly reported"
      }
    >
      <CircleHelp size={10} aria-hidden />
      {unavailable ? "unavailable" : "inferred"}
    </span>
  );
}

export function Pill({
  children,
  tone = "neutral",
  title,
}: {
  children: React.ReactNode;
  tone?: "neutral" | "accent" | "error" | "warn" | "ok" | "unknown";
  title?: string;
}) {
  const tones = {
    neutral: "border-border text-muted",
    accent: "border-accent/40 text-accent",
    error: "border-error/40 text-error",
    warn: "border-warn/40 text-warn",
    ok: "border-ok/40 text-ok",
    unknown: "border-unknown/40 text-unknown",
  } as const;
  return (
    <span
      title={title}
      className={cn(
        "inline-flex items-center rounded-sm border px-1.5 py-px text-2xs whitespace-nowrap",
        tones[tone],
      )}
    >
      {children}
    </span>
  );
}

export function AgentPill({ role, name }: { role: string | null; name: string | null }) {
  if (!name) return <span className="text-unknown">{UNKNOWN}</span>;
  const tone =
    role === "main"
      ? "accent"
      : role === "probe"
        ? "unknown"
        : role === "subagent"
          ? "ok"
          : "neutral";
  return (
    <Pill tone={tone} title={role ? `role: ${role}` : undefined}>
      {name}
    </Pill>
  );
}

/* ── states ───────────────────────────────────────────────────────────── */

export function Loading({ label = "Loading" }: { label?: string }) {
  return (
    <div
      role="status"
      className="flex items-center justify-center gap-2 py-10 text-sm text-faint"
    >
      <Loader2 size={14} className="animate-spin" aria-hidden />
      {label}
    </div>
  );
}

export function Empty({ label, hint }: { label: string; hint?: string }) {
  return (
    <div className="px-4 py-10 text-center">
      <p className="text-sm text-muted">{label}</p>
      {hint && <p className="mt-1 text-xs text-faint">{hint}</p>}
    </div>
  );
}

export function Failed({ error }: { error: unknown }) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    <div role="alert" className="px-4 py-8 text-center">
      <p className="text-sm text-error">Could not load this view</p>
      <p className="mt-1 font-mono text-2xs text-faint">{message}</p>
    </div>
  );
}

/* ── tables ───────────────────────────────────────────────────────────── */

export function Table({ children }: { children: React.ReactNode }) {
  return (
    <div className="overflow-x-auto">
      <table className="w-full border-collapse text-sm">{children}</table>
    </div>
  );
}

export function Th({
  children,
  align = "left",
  className,
}: {
  children?: React.ReactNode;
  align?: "left" | "right";
  className?: string;
}) {
  return (
    <th
      className={cn(
        "sticky top-0 z-10 h-8 bg-surface px-2.5 text-2xs font-medium uppercase tracking-wider text-faint",
        align === "right" ? "text-right" : "text-left",
        className,
      )}
    >
      {children}
    </th>
  );
}

export function Td({
  children,
  align = "left",
  className,
  title,
}: {
  children?: React.ReactNode;
  align?: "left" | "right";
  className?: string;
  title?: string;
}) {
  return (
    <td
      title={title}
      className={cn(
        "cell border-t border-border/60 text-sm",
        align === "right" ? "text-right tnum" : "text-left",
        className,
      )}
    >
      {children}
    </td>
  );
}

export function Tr({
  children,
  onClick,
  selected,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  selected?: boolean;
}) {
  return (
    <tr
      onClick={onClick}
      className={cn(
        "row",
        onClick && "cursor-pointer",
        selected ? "bg-accent-soft/40" : onClick && "hover:bg-raised",
      )}
    >
      {children}
    </tr>
  );
}
