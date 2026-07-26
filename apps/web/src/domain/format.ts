/**
 * Presentation rules.
 *
 * One rule governs this file: **absent is not zero**. The API distinguishes a
 * counter that is genuinely zero from one that was never captured, and every
 * formatter here preserves that — `null` renders as an em dash, never as `0`,
 * `$0.00`, or `0 ms`. Showing a call we failed to capture as free, or a session
 * we never measured as instant, would be a confident wrong answer.
 */

/** The mark for "we do not know", distinct from any real value. */
export const UNKNOWN = "—";

export function num(value: number | null | undefined): string {
  return value == null ? UNKNOWN : value.toLocaleString();
}

/** Compact counts for dense table cells: 1.2k, 970k, 4.1M. */
export function compact(value: number | null | undefined): string {
  if (value == null) return UNKNOWN;
  const abs = Math.abs(value);
  if (abs >= 1e9) return `${(value / 1e9).toFixed(1)}B`;
  if (abs >= 1e6) return `${(value / 1e6).toFixed(abs >= 1e7 ? 0 : 1)}M`;
  if (abs >= 1e4) return `${Math.round(value / 1e3)}k`;
  if (abs >= 1e3) return `${(value / 1e3).toFixed(1)}k`;
  return value.toLocaleString();
}

/**
 * Money, at a precision that matches the magnitude. Sub-cent amounts are real
 * in this domain — a cache read can cost a hundredth of a cent — so rounding
 * everything to two places would show a long tail of genuine spend as $0.00.
 */
export function usd(value: number | null | undefined): string {
  if (value == null) return UNKNOWN;
  if (value === 0) return "$0";
  const abs = Math.abs(value);
  if (abs < 0.01) return `$${value.toFixed(4)}`;
  if (abs < 1) return `$${value.toFixed(3)}`;
  if (abs < 1000) return `$${value.toFixed(2)}`;
  return `$${Math.round(value).toLocaleString()}`;
}

export function ms(value: number | null | undefined): string {
  if (value == null) return UNKNOWN;
  if (value < 1000) return `${Math.round(value)}ms`;
  if (value < 60_000) return `${(value / 1000).toFixed(1)}s`;
  const minutes = Math.floor(value / 60_000);
  const seconds = Math.round((value % 60_000) / 1000);
  return `${minutes}m ${seconds}s`;
}

export function percent(value: number | null | undefined, digits = 0): string {
  return value == null ? UNKNOWN : `${(value * 100).toFixed(digits)}%`;
}

export function ago(iso: string | null | undefined): string {
  if (!iso) return UNKNOWN;
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return UNKNOWN;
  const seconds = Math.max(0, (Date.now() - then) / 1000);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h ago`;
  const days = Math.floor(seconds / 86_400);
  return days < 30 ? `${days}d ago` : new Date(then).toLocaleDateString();
}

export function clock(iso: string | null | undefined): string {
  if (!iso) return UNKNOWN;
  const at = new Date(iso);
  return Number.isNaN(at.getTime()) ? UNKNOWN : at.toLocaleString();
}

/** Shorten an opaque id for display while keeping both ends recognisable. */
export function shortId(value: string | null | undefined, keep = 8): string {
  if (!value) return UNKNOWN;
  return value.length > keep + 4 ? value.slice(0, keep) : value;
}

/** Drop a model's date suffix; the family is what a reader compares. */
export function modelLabel(value: string | null | undefined): string {
  if (!value) return UNKNOWN;
  return value.replace(/-\d{8}$/, "");
}

export function truncate(value: string | null | undefined, max = 90): string {
  if (!value) return UNKNOWN;
  const flat = value.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}
