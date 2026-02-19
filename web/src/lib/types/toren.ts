export interface CommandOutput {
	type: 'Stdout' | 'Stderr' | 'Exit' | 'Error';
	line?: string;
	code?: number;
	message?: string;
}

export interface FileContent {
	content: string;
}

export interface VcsStatus {
	vcs_type: 'Git' | 'Jj' | 'None';
	branch?: string;
	modified: string[];
	added: string[];
	deleted: string[];
}

export type AncillaryStatus =
	| 'idle'
	| 'starting'
	| 'working'
	| 'awaiting_input'
	| 'completed'
	| 'failed'
	| 'connected'
	| 'executing'
	| 'disconnected';

export type AncillaryDisplayStatus = 'busy' | 'ready';

export type BeadDisplayStatus = 'open' | 'in_progress' | 'closed';

export type BeadStatus = 'open' | 'in_progress' | 'closed';

export type AgentActivity = 'busy' | 'idle';

export interface Ancillary {
	id: string;
	segment: string;
	status: AncillaryStatus;
	last_active: string;
}

export type AssignmentStatus = 'active';

export interface AssignmentSource {
	type: 'Bead' | 'Prompt';
	original_prompt?: string;
}

export interface Assignment {
	id: string;
	ancillary_id: string;
	bead_id: string;
	segment: string;
	workspace_path: string;
	source: AssignmentSource;
	status: AssignmentStatus;
	created_at: string;
	updated_at: string;
	bead_title?: string;
	session_id?: string;
	ancillary_num?: number;
	// Composite status signals (from API enrichment)
	agent_activity?: AgentActivity;
	has_changes?: boolean;
	bead_status?: BeadStatus;
	bead_assignee?: string;
}

export interface Segment {
	name: string;
	path: string;
	source: 'glob' | 'path' | 'root';
}

export interface SegmentsResponse {
	segments: Segment[];
	roots: string[];
	count: number;
}

// WebSocket Request Types
export type WsRequest =
	| { type: 'Auth'; token: string; ancillary_id?: string; segment?: string }
	| { type: 'Command'; request: { command: string; args: string[]; cwd?: string } }
	| { type: 'FileRead'; path: string }
	| { type: 'VcsStatus'; path: string };

// WebSocket Response Types
export type WsResponse =
	| { type: 'AuthSuccess'; session_id: string }
	| { type: 'AuthFailure'; reason: string }
	| { type: 'CommandOutput'; output: CommandOutput }
	| { type: 'FileContent'; content: string }
	| { type: 'VcsStatus'; status: VcsStatus }
	| { type: 'Error'; message: string };

// Work Event Types (from ancillary WebSocket)
export interface WorkEvent {
	seq: number;
	timestamp: string;
	op: WorkOp;
}

export type WorkOp =
	| { type: 'assistant_message'; content: string }
	| { type: 'user_message'; content: string; client_id: string }
	| { type: 'thinking_start' }
	| { type: 'thinking_end' }
	| { type: 'tool_call'; id: string; name: string; input: unknown }
	| { type: 'tool_result'; id: string; output: unknown; is_error: boolean }
	| { type: 'file_read'; path: string }
	| { type: 'file_write'; path: string }
	| { type: 'command_start'; command: string; args: string[] }
	| { type: 'command_output'; stdout?: string; stderr?: string }
	| { type: 'command_exit'; code: number }
	| { type: 'assignment_started'; bead_id: string }
	| { type: 'assignment_completed' }
	| { type: 'assignment_failed'; error: string }
	| { type: 'status_change'; status: string }
	| { type: 'client_connected'; client_id: string }
	| { type: 'client_disconnected'; client_id: string };

// Ancillary WebSocket Request Types
export type AncillaryWsRequest = { type: 'message'; content: string } | { type: 'interrupt' };

// Ancillary WebSocket Response Types
export type AncillaryWsResponse =
	| { type: 'event'; event: WorkEvent }
	| { type: 'replay_complete'; current_seq: number }
	| { type: 'status'; status: string; ancillary_id: string }
	| { type: 'error'; message: string };

// REST API Types
export interface PairRequest {
	pairing_token: string;
}

export interface PairResponse {
	session_token: string;
	session_id: string;
}

export interface HealthResponse {
	status: string;
	version: string;
}

export interface CreateAssignmentRequest {
	prompt?: string;
	bead_id?: string;
	title?: string;
	segment: string;
}

export interface AssignmentResponse {
	assignment: Assignment;
}

export interface StartWorkRequest {
	assignment_id: string;
}
