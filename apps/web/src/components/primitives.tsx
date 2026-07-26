import type { PropsWithChildren } from "react";
import type { Alert, Severity } from "../domain/types";

export function DataState({
	state,
	children,
}: PropsWithChildren<{
	state: "loading" | "empty" | "error" | "unavailable";
}>) {
	return (
		<p
			role={state === "error" ? "alert" : "status"}
			className={`state ${state}`}
		>
			{children}
		</p>
	);
}

export function SeverityBadge({ severity }: { severity: Severity }) {
	const icon =
		severity === "error" || severity === "critical"
			? "●"
			: severity === "warning"
				? "▲"
				: "●";
	return (
		<span className={`badge ${severity}`}>
			<span aria-hidden="true">{icon}</span> {severity}
		</span>
	);
}

export function MetricCard({
	label,
	value,
	unavailable,
	hint,
}: {
	label: string;
	value?: string | number;
	unavailable?: boolean;
	hint?: string;
}) {
	const isUnavailable = unavailable || value === undefined;
	return (
		<section className="metric">
			<p>{label}</p>
			<strong className={isUnavailable ? "unavailable" : ""}>
				{isUnavailable ? "—" : value}
			</strong>
			{hint && <small>{hint}</small>}
		</section>
	);
}

export function DiagnosticPanel({ alert }: { alert: Alert }) {
	const source = alert.sources[0];
	return (
		<article className="diagnostic">
			<header>
				<SeverityBadge severity={alert.severity} /> <code>{alert.rule_id}</code>
			</header>
			<h3>{alert.title}</h3>
			<p>{alert.summary}</p>
			<dl>
				{alert.observed.map((item) => (
					<div key={item.label}>
						<dt>{item.label}</dt>
						<dd>
							{item.value ?? "Unavailable"} {item.unit}
						</dd>
					</div>
				))}
			</dl>
			<h4>Why</h4>
			<p>{alert.explanation}</p>
			<h4>Impact</h4>
			<p>{alert.impact}</p>
			<h4>Recommended action</h4>
			<p>{alert.recommendation}</p>
			{source?.call_id && (
				<button
					type="button"
					className="source-link"
					onClick={() => {
						window.location.hash = `/calls/${source.call_id}`;
					}}
				>
					Inspect request {source.call_id}
				</button>
			)}
		</article>
	);
}
