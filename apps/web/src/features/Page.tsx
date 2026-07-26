import * as React from "react";

/** Consistent page chrome: a tight header, then content. No hero copy. */
export function Page({
  title,
  subtitle,
  action,
  children,
}: {
  title: string;
  subtitle?: React.ReactNode;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="flex min-h-full flex-col">
      <header className="flex h-11 shrink-0 items-center gap-3 border-b border-border px-4">
        <h1 className="text-sm font-semibold tracking-tight">{title}</h1>
        {subtitle && (
          <span className="truncate text-2xs text-faint">{subtitle}</span>
        )}
        {action && <div className="ml-auto flex items-center gap-2">{action}</div>}
      </header>
      <div className="flex-1 space-y-3 p-3">{children}</div>
    </div>
  );
}
