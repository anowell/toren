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

export type AncillaryStatus = 'idle' | 'connected' | 'executing';

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
	type: 'Reference' | 'Bead' | 'Prompt';
	original_prompt?: string;
}

export interface Assignment {
	id: string;
	ancillary_id: string;
	/** Task identifier (e.g., bead ID). Canonical field. */
	task_id?: string;
	/** @deprecated Use task_id */
	external_id?: string;
	/** @deprecated Use task_id */
	bead_id?: string;
	segment: string;
	workspace_path: string;
	source: AssignmentSource;
	status: AssignmentStatus;
	created_at: string;
	updated_at: string;
	/** Task display title. Canonical field. */
	task_title?: string;
	/** @deprecated Use task_title */
	title?: string;
	/** @deprecated Use task_title */
	bead_title?: string;
	/** Task URL */
	task_url?: string;
	/** Task source (e.g., "beads") */
	task_source?: string;
	session_id?: string;
	ancillary_num?: number;
	// Composite status signals (from API enrichment)
	agent_activity?: AgentActivity;
	has_changes?: boolean;
	/** Task status from provider */
	task_status?: BeadStatus;
	/** @deprecated Use task_status */
	bead_status?: BeadStatus;
	/** Task assignee from provider */
	task_assignee?: string;
	/** @deprecated Use task_assignee */
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

// Ancillary Terminal WebSocket (raw pane bytes both ways; JSON only for control)
export type AncillaryWsRequest =
	| { type: 'data'; data: string }
	| { type: 'resize'; cols: number; rows: number }
	| { type: 'interrupt' };

export type AncillaryWsResponse =
	| { type: 'status'; status: string; session: string }
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
	task_id?: string;
	/** @deprecated Use task_id */
	bead_id?: string;
	task_title?: string;
	task_url?: string;
	task_source?: string;
	segment: string;
}

export interface AssignmentResponse {
	assignment: Assignment;
}

export interface StartWorkRequest {
	assignment_id: string;
}
