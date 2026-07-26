export type Severity = "critical" | "error" | "warning" | "info";
export type AlertCategory =
	| "execution"
	| "context"
	| "performance"
	| "cache"
	| "data_quality";
export interface Alert {
	id: string;
	category: AlertCategory;
	severity: Severity;
	rule_id: string;
	title: string;
	summary: string;
	occurred_at: string;
	recommendation: string;
	sources: { session_key?: string; call_id?: number; metric_ref?: string }[];
	observed: {
		label: string;
		value?: string;
		unit?: string;
		exactness: string;
	}[];
	explanation: string;
	impact: string;
	confidence: string;
}
export interface CallSummary {
	id: number;
	timestamp_start: string;
	timestamp_end?: string;
	model?: string;
	status?: number;
	has_error: boolean;
}
export interface SessionSummary {
	key: string;
	title?: string;
	calls_count: number;
	model?: string;
	start: string;
	end?: string;
	has_error: boolean;
	has_compaction: boolean;
}
export interface TimelineCall {
	id: number;
	model?: string;
	status?: number;
	start: string;
	end?: string;
	ttft_ms?: number;
	latency_ms?: number;
	context_input_tokens?: number;
	context_approx_chars: number;
	compaction: boolean;
	system_drift: boolean;
	error?: string;
}
export interface SessionDetail {
	key: string;
	calls: TimelineCall[];
	signals: {
		context_growth: {
			call_id: number;
			input_tokens?: number;
			approx_chars: number;
		}[];
	};
}
export interface NormalizedCall {
	id: number;
	model?: string;
	timestamps: { start: string; first_chunk?: string; end?: string };
	system: {
		text: string;
		approx_size: { chars: number; bytes: number };
		cache_control: boolean;
	}[];
	declared_tools: { name: string }[];
	thread: {
		role: string;
		blocks: {
			kind: string;
			content_tag?:
				| "system_reminder"
				| "local_command_caveat"
				| "command_name"
				| "command_message"
				| "command_args"
				| "local_command_stdout";
			content?: string;
			tool_name?: string;
			tool_use_id?: string;
			is_error?: boolean;
			approx_size: { chars: number; bytes: number };
		}[];
	}[];
	usage: {
		input?: number;
		output?: number;
		cache_creation?: number;
		cache_read?: number;
	};
}
export interface Dashboard {
	calls: CallSummary[];
	sessions: SessionSummary[];
	alerts: Alert[];
	totals: {
		calls: number;
		sessions: number;
		errors: number;
		warnings: number;
		total_tokens?: number;
		cache_reuse_rate?: number;
		ttft_ms?: number;
		latency_ms?: number;
	};
}
