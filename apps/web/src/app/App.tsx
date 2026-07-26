import { useEffect, useMemo, useState } from "react";
import { api, subscribe } from "../api/client";
import type {
	Alert,
	Dashboard,
	NormalizedCall,
	SessionDetail,
} from "../domain/types";
import { DataState, MetricCard, SeverityBadge } from "../components/primitives";
import { LineChart } from "../components/LineChart";

const tagLabel: Record<string, string> = {
	system_reminder: "system reminder",
	local_command_caveat: "local command caveat",
	command_name: "command name",
	command_message: "command message",
	command_args: "command arguments",
	local_command_stdout: "command output",
};

function route() {
	return window.location.hash.replace(/^#/, "") || "/";
}
function useRoute() {
	const [value, setValue] = useState(route());
	useEffect(() => {
		const onChange = () => setValue(route());
		window.addEventListener("hashchange", onChange);
		return () => window.removeEventListener("hashchange", onChange);
	}, []);
	return value;
}
function go(path: string) {
	window.location.hash = path;
}
function formatNumber(value?: number | null) {
	// The API sends an absent count as JSON null, not undefined — a strict
	// `=== undefined` check let null through and crashed on .toLocaleString().
	// Absent is rendered as unknown, never as zero.
	return value == null ? "—" : value.toLocaleString();
}
function formatTime(value?: string) {
	return value ? new Date(value).toLocaleString() : "—";
}
function shortKey(value: string) {
	return value.length > 18 ? `${value.slice(0, 8)}…${value.slice(-6)}` : value;
}

function AppFrame({ children }: { children: React.ReactNode }) {
	return <main className="app-frame">{children}</main>;
}
function Crumbs({ items }: { items: { label: string; to?: string }[] }) {
	return (
		<nav className="crumbs" aria-label="Breadcrumb">
			{items.map((item, index) => (
				<span key={`${item.label}-${index}`}>
					{item.to ? (
						<button onClick={() => go(item.to!)}>{item.label}</button>
					) : (
						<strong>{item.label}</strong>
					)}
					{index < items.length - 1 && <i aria-hidden="true">/</i>}
				</span>
			))}
		</nav>
	);
}
function PageHeader({
	crumbs,
	title,
	description,
	actions,
	children,
}: {
	crumbs: { label: string; to?: string }[];
	title: string;
	description: string;
	actions?: React.ReactNode;
	children?: React.ReactNode;
}) {
	return (
		<>
			<header className="page-header">
				<div className="heading-copy">
					<Crumbs items={crumbs} />
					<div className="title-line">
						<span className="product-mark" aria-hidden="true">
							◌
						</span>
						<h1>{title}</h1>
					</div>
					<p>{description}</p>
				</div>
				{actions && <div className="page-actions">{actions}</div>}
			</header>
			{children}
		</>
	);
}
function Button({
	children,
	onClick,
	variant = "outline",
	disabled,
}: {
	children: React.ReactNode;
	onClick: () => void;
	variant?: "outline" | "primary" | "ghost";
	disabled?: boolean;
}) {
	return (
		<button
			className={`button ${variant}`}
			onClick={onClick}
			disabled={disabled}
		>
			{children}
		</button>
	);
}
function Empty({ children }: { children: React.ReactNode }) {
	return <div className="empty-card">{children}</div>;
}
function AlertRows({ alerts, limit }: { alerts: Alert[]; limit?: number }) {
	const visible = alerts.slice(0, limit);
	if (!visible.length)
		return <Empty>No issues detected in this capture window.</Empty>;
	return (
		<div className="table-card alert-table">
			{visible.map((alert) => (
				<button
					className="table-row"
					key={alert.id}
					onClick={() =>
						alert.sources[0]?.call_id &&
						go(`/calls/${alert.sources[0].call_id}`)
					}
				>
					<span className="alert-title">
						<SeverityBadge severity={alert.severity} />
						<span>
							<b>{alert.title}</b>
							<small>{alert.summary}</small>
						</span>
					</span>
					<span className="muted hide-mobile">
						{alert.category.replace("_", " ")}
					</span>
					<span className="muted time">{formatTime(alert.occurred_at)}</span>
				</button>
			))}
		</div>
	);
}
function SectionHeader({
	title,
	detail,
	action,
}: {
	title: string;
	detail?: string;
	action?: React.ReactNode;
}) {
	return (
		<div className="section-header">
			<div>
				<h2>{title}</h2>
				{detail && <p>{detail}</p>}
			</div>
			{action}
		</div>
	);
}

function Overview() {
	const [data, setData] = useState<Dashboard>();
	const [error, setError] = useState<string>();
	const [connected, setConnected] = useState(false);
	const load = () =>
		api
			.dashboard()
			.then((result) => {
				setData(result);
				setError(undefined);
			})
			.catch((e: Error) => setError(e.message));
	useEffect(() => {
		load();
		return subscribe(load, setConnected);
	}, []);
	if (error)
		return (
			<AppFrame>
				<DataState state="error">
					Couldn’t load local captures: {error}
				</DataState>
			</AppFrame>
		);
	if (!data)
		return (
			<AppFrame>
				<DataState state="loading">
					Loading your local agent activity…
				</DataState>
			</AppFrame>
		);
	const hasActivity = data.totals.calls > 0;
	return (
		<AppFrame>
			<PageHeader
				crumbs={[{ label: "Observatory" }]}
				title="Your agent runs, understood."
				description="Local traces for Claude Code and Codex. See the work, not just the requests."
				actions={
					<>
						<span className={`connection ${connected ? "connected" : ""}`}>
							<i />
							{connected ? "Live capture" : "Reconnecting"}
						</span>
						<Button onClick={load}>Refresh</Button>
					</>
				}
			/>
			{!hasActivity ? (
				<Empty>
					<h2>Waiting for your first run</h2>
					<p>
						Start Claude Code or Codex through the local relay. Its requests,
						tool work, context, and outcomes will appear here.
					</p>
				</Empty>
			) : (
				<>
					<section className="metric-grid">
						<MetricCard
							label="Agent runs"
							value={data.totals.sessions}
							hint="Captured conversations"
						/>
						<MetricCard
							label="Model requests"
							value={data.totals.calls}
							hint="Across all local runs"
						/>
						<MetricCard
							label="Tokens processed"
							value={formatNumber(data.totals.total_tokens)}
							hint="Input + output, exact when provided"
						/>
						<MetricCard
							label="Cache reuse"
							value={
								data.totals.cache_reuse_rate === undefined
									? undefined
									: `${Math.round(data.totals.cache_reuse_rate * 100)}%`
							}
							hint="Served from prompt cache"
						/>
					</section>
					<section className="overview-grid">
						<div>
							<SectionHeader
								title="Recent agent runs"
								detail="Continue where you left off"
								action={
									<Button variant="ghost" onClick={() => go("/sessions")}>
										View all runs →
									</Button>
								}
							/>
							<div className="table-card">
								{data.sessions.slice(0, 6).map((session) => (
									<button
										className="table-row session-row"
										key={session.key}
										onClick={() =>
											go(`/sessions/${encodeURIComponent(session.key)}`)
										}
									>
										<span className="run-cell">
											<span className="run-icon">⌁</span>
											<span>
												<b>{session.title || "Untitled agent run"}</b>
												<small>{shortKey(session.key)}</small>
											</span>
										</span>
										<span className="model-pill hide-mobile">
											{session.model || "Unknown model"}
										</span>
										<span className="muted hide-mobile">
											{session.calls_count} request
											{session.calls_count === 1 ? "" : "s"}
										</span>
										<span className="muted time">
											{formatTime(session.end || session.start)}
										</span>
									</button>
								))}
							</div>
						</div>
						<aside className="health-card">
							<p className="eyebrow">Run health</p>
							<h2>{data.totals.errors ? "Needs attention" : "Looking good"}</h2>
							<p>
								{data.totals.errors
									? `${data.totals.errors} error${data.totals.errors === 1 ? "" : "s"} need review across recent runs.`
									: "No execution failures detected in the current capture window."}
							</p>
							<div className="health-stats">
								<span>
									<b>{data.totals.errors}</b> errors
								</span>
								<span>
									<b>{data.totals.warnings}</b> signals
								</span>
							</div>
							<Button variant="outline" onClick={() => go("/alerts")}>
								Review signals
							</Button>
						</aside>
					</section>
					<section className="section-block">
						<SectionHeader
							title="Signals worth reviewing"
							detail="Derived from what your agent actually sent, received, and called."
							action={
								<Button variant="ghost" onClick={() => go("/alerts")}>
									All signals →
								</Button>
							}
						/>
						<AlertRows alerts={data.alerts} limit={5} />
					</section>
				</>
			)}
		</AppFrame>
	);
}

function Sessions() {
	const [items, setItems] = useState<Dashboard["sessions"]>();
	const [query, setQuery] = useState("");
	useEffect(() => {
		api.sessions().then((result) => setItems(result.sessions));
	}, []);
	const filtered = useMemo(
		() =>
			items?.filter((session) =>
				`${session.title} ${session.key} ${session.model}`
					.toLowerCase()
					.includes(query.toLowerCase()),
			) || [],
		[items, query],
	);
	return (
		<AppFrame>
			<PageHeader
				crumbs={[{ label: "Observatory", to: "/" }, { label: "Agent runs" }]}
				title="Agent runs"
				description="Each run groups the requests, context changes, tool work, and outcome of one conversation."
				actions={
					<label className="search">
						<span>⌕</span>
						<input
							aria-label="Search agent runs"
							placeholder="Search runs"
							value={query}
							onChange={(event) => setQuery(event.target.value)}
						/>
					</label>
				}
			/>
			{!items ? (
				<DataState state="loading">Loading agent runs…</DataState>
			) : !filtered.length ? (
				<Empty>No runs match that search.</Empty>
			) : (
				<div className="table-card runs-table">
					<div className="column-labels">
						<span>Run</span>
						<span className="hide-mobile">Model</span>
						<span className="hide-mobile">Requests</span>
						<span>Last activity</span>
					</div>
					{filtered.map((session) => (
						<button
							className="table-row session-row"
							key={session.key}
							onClick={() => go(`/sessions/${encodeURIComponent(session.key)}`)}
						>
							<span className="run-cell">
								<span className="run-icon">⌁</span>
								<span>
									<b>{session.title || "Untitled agent run"}</b>
									<small>{shortKey(session.key)}</small>
								</span>
							</span>
							<span className="model-pill hide-mobile">
								{session.model || "Unknown model"}
							</span>
							<span className="muted hide-mobile">{session.calls_count}</span>
							<span className="muted time">
								{formatTime(session.end || session.start)}
							</span>
						</button>
					))}
				</div>
			)}
		</AppFrame>
	);
}

function Session({ sessionKey }: { sessionKey: string }) {
	const [detail, setDetail] = useState<SessionDetail>();
	const [alerts, setAlerts] = useState<Alert[]>([]);
	useEffect(() => {
		Promise.all([api.session(sessionKey), api.sessionAlerts(sessionKey)]).then(
			([session, result]) => {
				setDetail(session);
				setAlerts(result.alerts);
			},
		);
	}, [sessionKey]);
	if (!detail)
		return (
			<AppFrame>
				<DataState state="loading">Opening agent run…</DataState>
			</AppFrame>
		);
	return (
		<AppFrame>
			<PageHeader
				crumbs={[
					{ label: "Observatory", to: "/" },
					{ label: "Agent runs", to: "/sessions" },
					{ label: shortKey(sessionKey) },
				]}
				title={"Agent run"}
				description={`${detail.calls.length} captured request${detail.calls.length === 1 ? "" : "s"} · ${shortKey(sessionKey)}`}
				actions={<Button onClick={() => go("/sessions")}>← All runs</Button>}
			/>
			<section className="metric-grid compact-metrics">
				<MetricCard label="Requests" value={detail.calls.length} />
				<MetricCard
					label="Context events"
					value={
						detail.calls.filter((call) => call.compaction || call.system_drift)
							.length
					}
				/>
				<MetricCard label="Signals" value={alerts.length} />
				<MetricCard
					label="Latest latency"
					value={
						detail.calls.at(-1)?.latency_ms
							? `${detail.calls.at(-1)?.latency_ms} ms`
							: undefined
					}
				/>
			</section>
			<section className="chart-grid section-block">
				<div className="chart-card">
					<LineChart
						title="Context sent to model"
						unit="tokens"
						points={detail.calls.map((call) => ({
							label: `#${call.id}`,
							value: call.context_input_tokens,
						}))}
					/>
				</div>
				<div className="chart-card">
					<LineChart
						title="Request latency"
						unit="ms"
						points={detail.calls.map((call) => ({
							label: `#${call.id}`,
							value: call.latency_ms,
						}))}
					/>
				</div>
			</section>
			<section className="section-block">
				<SectionHeader
					title="Request timeline"
					detail="Open a request to inspect the exact model exchange."
				/>
				<div className="table-card">
					<div className="column-labels call-labels">
						<span>Request</span>
						<span className="hide-mobile">Context</span>
						<span className="hide-mobile">Time to first token</span>
						<span>Latency</span>
					</div>
					{detail.calls.map((call) => (
						<button
							className="table-row call-row"
							key={call.id}
							onClick={() => go(`/calls/${call.id}`)}
						>
							<span>
								<b>Request #{call.id}</b>
								<small>
									{call.model || "Unknown model"}
									{call.compaction ? " · context compacted" : ""}
									{call.system_drift ? " · system changed" : ""}
								</small>
							</span>
							<span className="muted hide-mobile">
								{formatNumber(call.context_input_tokens)} tokens
							</span>
							<span className="muted hide-mobile">
								{call.ttft_ms ?? "—"} ms
							</span>
							<span className="muted time">{call.latency_ms ?? "—"} ms</span>
						</button>
					))}
				</div>
			</section>
			<section className="section-block">
				<SectionHeader
					title="Signals in this run"
					detail="Explanations are based on the captured request and response data."
				/>
				<AlertRows alerts={alerts} />
			</section>
		</AppFrame>
	);
}

function Conversation({ call }: { call: NormalizedCall }) {
	return (
		<section className="detail-card">
			<SectionHeader
				title="Conversation"
				detail={`${call.thread.length} turns captured`}
			/>
			{call.thread.map((turn, turnIndex) => (
				<article className="turn" key={turnIndex}>
					<div className="turn-head">
						<span className={`role ${turn.role}`}>{turn.role}</span>
						<span>
							{formatNumber(
								turn.blocks.reduce(
									(sum, block) => sum + block.approx_size.chars,
									0,
								),
							)}{" "}
							chars
						</span>
					</div>
					{turn.blocks.map((block, blockIndex) => (
						<details key={blockIndex} className="content-block">
							<summary>
								<span>
									{block.kind.replace("_", " ")}
									{block.tool_name ? ` · ${block.tool_name}` : ""}
								</span>
								<small>~{formatNumber(block.approx_size.chars)} chars</small>
							</summary>
							<pre>
								{block.content ||
									(block.content_tag
										? `[${tagLabel[block.content_tag] || block.content_tag}]`
										: "No text content captured.")}
							</pre>
						</details>
					))}
				</article>
			))}
		</section>
	);
}

function Call({ id }: { id: number }) {
	const [call, setCall] = useState<NormalizedCall>();
	const [alerts, setAlerts] = useState<Alert[]>([]);
	useEffect(() => {
		Promise.all([api.normalizedCall(id), api.callAlerts(id)]).then(
			([normalized, result]) => {
				setCall(normalized);
				setAlerts(result.alerts);
			},
		);
	}, [id]);
	if (!call)
		return (
			<AppFrame>
				<DataState state="loading">Opening request…</DataState>
			</AppFrame>
		);
	const total =
		call.usage.input !== undefined && call.usage.output !== undefined
			? call.usage.input + call.usage.output
			: undefined;
	return (
		<AppFrame>
			<PageHeader
				crumbs={[
					{ label: "Observatory", to: "/" },
					{ label: "Agent runs", to: "/sessions" },
					{ label: `Request #${id}` },
				]}
				title={`Request #${id}`}
				description={`${call.model || "Unknown model"} · ${formatTime(call.timestamps.start)}`}
				actions={
					<>
						<Button onClick={() => history.back()}>← Back</Button>
						<Button variant="primary" onClick={() => go(`/calls/${id}/raw`)}>
							Raw capture
						</Button>
					</>
				}
			/>
			<section className="metric-grid">
				<MetricCard
					label="Total tokens"
					value={formatNumber(total)}
					hint="Input + output"
				/>
				<MetricCard label="Input" value={formatNumber(call.usage.input)} />
				<MetricCard label="Output" value={formatNumber(call.usage.output)} />
				<MetricCard
					label="Cache read"
					value={formatNumber(call.usage.cache_read)}
				/>
			</section>
			<section className="call-layout section-block">
				<Conversation call={call} />
				<aside className="side-stack">
					<section className="detail-card">
						<SectionHeader title="Signals" detail={`${alerts.length} found`} />
						<AlertRows alerts={alerts} />
					</section>
					<section className="detail-card">
						<SectionHeader
							title="System prompt"
							detail={`${call.system.length} segments`}
						/>
						{call.system.length ? (
							call.system.map((segment, index) => (
								<details className="content-block" key={index}>
									<summary>
										<span>
											Segment {index + 1}
											{segment.cache_control ? " · cache point" : ""}
										</span>
										<small>
											~{formatNumber(segment.approx_size.chars)} chars
										</small>
									</summary>
									<pre>{segment.text}</pre>
								</details>
							))
						) : (
							<DataState state="unavailable">
								No system prompt was available in this capture.
							</DataState>
						)}
					</section>
				</aside>
			</section>
		</AppFrame>
	);
}

function Alerts() {
	const [alerts, setAlerts] = useState<Alert[]>();
	const [severity, setSeverity] = useState("");
	useEffect(() => {
		api
			.alerts(severity ? `?severity=${severity}` : "")
			.then((result) => setAlerts(result.alerts));
	}, [severity]);
	return (
		<AppFrame>
			<PageHeader
				crumbs={[{ label: "Observatory", to: "/" }, { label: "Signals" }]}
				title="Signals"
				description="Potential execution, context, performance, and cache issues—each tied to the source request."
				actions={
					<select
						className="select"
						aria-label="Filter signal severity"
						value={severity}
						onChange={(event) => setSeverity(event.target.value)}
					>
						<option value="">All severities</option>
						<option value="critical">Critical</option>
						<option value="error">Errors</option>
						<option value="warning">Warnings</option>
						<option value="info">Info</option>
					</select>
				}
			/>
			{!alerts ? (
				<DataState state="loading">Reviewing captured activity…</DataState>
			) : (
				<AlertRows alerts={alerts} />
			)}
		</AppFrame>
	);
}
function Raw({ id }: { id: number }) {
	const [raw, setRaw] = useState<unknown>();
	useEffect(() => {
		fetch(`/api/calls/${id}`)
			.then((response) => response.json())
			.then(setRaw);
	}, [id]);
	return (
		<AppFrame>
			<PageHeader
				crumbs={[
					{ label: "Observatory", to: "/" },
					{ label: `Request #${id}`, to: `/calls/${id}` },
					{ label: "Raw capture" },
				]}
				title="Raw capture"
				description="Source-of-truth request and response data. Use the request view for interpretation."
				actions={
					<Button onClick={() => go(`/calls/${id}`)}>← Request details</Button>
				}
			/>
			{raw ? (
				<pre className="raw-card">{JSON.stringify(raw, null, 2)}</pre>
			) : (
				<DataState state="loading">Loading source capture…</DataState>
			)}
		</AppFrame>
	);
}
export function App() {
	const current = useRoute();
	const raw = current.match(/^\/calls\/(\d+)\/raw$/);
	const call = current.match(/^\/calls\/(\d+)$/);
	const session = current.match(/^\/sessions\/(.+)$/);
	if (raw) return <Raw id={Number(raw[1])} />;
	if (call) return <Call id={Number(call[1])} />;
	if (session) return <Session sessionKey={decodeURIComponent(session[1])} />;
	if (current === "/sessions") return <Sessions />;
	if (current === "/alerts") return <Alerts />;
	return <Overview />;
}
