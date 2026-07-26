import type {
	Alert,
	CallSummary,
	Dashboard,
	NormalizedCall,
	SessionDetail,
	SessionSummary,
} from "../domain/types";

async function get<T>(path: string): Promise<T> {
	const response = await fetch(path);
	if (!response.ok) throw new Error(`Request failed (${response.status})`);
	return response.json() as Promise<T>;
}

export const api = {
	dashboard: () => get<Dashboard>("/api/ui/dashboard"),
	calls: () => get<{ calls: CallSummary[] }>("/api/calls"),
	sessions: () => get<{ sessions: SessionSummary[] }>("/api/sessions"),
	callAlerts: (id: number) =>
		get<{ alerts: Alert[] }>(`/api/calls/${id}/diagnostics`),
	session: (key: string) =>
		get<SessionDetail>(`/api/sessions/${encodeURIComponent(key)}`),
	sessionAlerts: (key: string) =>
		get<{ alerts: Alert[] }>(
			`/api/sessions/${encodeURIComponent(key)}/diagnostics`,
		),
	normalizedCall: (id: number) =>
		get<NormalizedCall>(`/api/calls/${id}/normalized`),
	alerts: (filters = "") =>
		get<{ alerts: Alert[]; total: number }>(`/api/ui/alerts${filters}`),
};

export function subscribe(
	onChange: () => void,
	onStatus: (connected: boolean) => void,
): () => void {
	let closed = false;
	let source: EventSource | undefined;
	let retry = 500;
	const connect = () => {
		if (closed) return;
		source = new EventSource("/api/ui/events");
		source.onopen = () => {
			retry = 500;
			onStatus(true);
		};
		source.onmessage = onChange;
		for (const name of [
			"call.created",
			"call.completed",
			"session.changed",
			"alerts.changed",
			"dashboard.changed",
		])
			source.addEventListener(name, onChange);
		source.onerror = () => {
			onStatus(false);
			source?.close();
			window.setTimeout(connect, retry);
			retry = Math.min(retry * 2, 15_000);
		};
	};
	connect();
	return () => {
		closed = true;
		source?.close();
	};
}
