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

export interface Ancillary {
	id: string;
	segment: string;
	status: 'Idle' | 'Thinking' | 'Executing';
	last_active: string;
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
