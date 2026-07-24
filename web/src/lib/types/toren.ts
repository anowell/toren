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

// ── Workspace ("place") model ──────────────────────────────────────────────

/** Session pane status reported by rmux. */
export type SessionStatus = 'idle' | 'running' | 'exited';

/** How we summarize a workspace for list/dot rendering. */
export type WorkspaceDisplayStatus = 'busy' | 'ready';

/** Normalized task status used to pick an icon. Native provider status is pass-through. */
export type TaskDisplayStatus = 'open' | 'in_progress' | 'closed';

export interface SessionInfo {
	window: string;
	status: SessionStatus;
	command: string;
	/** Optional agent-provided activity string (e.g. "thinking"). */
	agent_activity?: string | null;
}

export interface CommitInfo {
	id: string;
	summary: string;
}

export interface PrInfo {
	branch: string;
	id: string;
	url: string;
	state: string;
	ci: string;
}

export interface TaskView {
	/** "source:id" link. */
	link: string;
	source: string;
	id: string;
	/** Task-source-owned, pass-through fields. */
	title?: string | null;
	status?: string | null;
	assignee?: string | null;
	url?: string | null;
	error?: string | null;
}

/** Serialized `Sets` — the collected state around a workspace. */
export interface Sets {
	sessions: SessionInfo[];
	changes: CommitInfo[];
	branches: string[];
	prs: PrInfo[];
	prs_age?: string | null;
	tasks: TaskView[];
}

/** A workspace/place as emitted by the daemon (mirrors `breq get <ws> --json`). */
export interface WorkspaceView {
	name: string;
	segment: string;
	uid?: string | null;
	path: string;
	title?: string | null;
	base?: string | null;
	parent?: string | null;
	decorated: boolean;
	vcs_tracked: boolean;
	annotations: Record<string, unknown>;
	sets: Sets;
}

export interface WorkspacesResponse {
	workspaces: WorkspaceView[];
	count: number;
	segment?: string;
}

export interface WorkspaceResponse {
	workspace: WorkspaceView;
}

export interface StartWorkspaceRequest {
	agent?: string;
	prompt?: string;
	model?: string;
	resume?: boolean;
}

export interface StartWorkspaceResponse {
	success: boolean;
	session: string;
	/** The window that was started (e.g. "agent"). */
	window?: string;
}

export interface StopWorkspaceResponse {
	success: boolean;
}

export interface ShellWorkspaceResponse {
	success: boolean;
	session: string;
	/** The name of the newly opened shell window (e.g. "shell-2"). */
	window: string;
}

// ── Segments ───────────────────────────────────────────────────────────────

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

// ── Control-channel WebSocket ──────────────────────────────────────────────

export type WsRequest =
	| { type: 'Auth'; token: string; segment?: string }
	| { type: 'Command'; request: { command: string; args: string[]; cwd?: string } }
	| { type: 'FileRead'; path: string }
	| { type: 'VcsStatus'; path: string };

export type WsResponse =
	| { type: 'AuthSuccess'; session_id: string }
	| { type: 'AuthFailure'; reason: string }
	| { type: 'CommandOutput'; output: CommandOutput }
	| { type: 'FileContent'; content: string }
	| { type: 'VcsStatus'; status: VcsStatus }
	| { type: 'Error'; message: string };

// ── Terminal WebSocket (raw pane bytes both ways; JSON only for control) ─────

export type TerminalWsRequest =
	| { type: 'data'; data: string }
	| { type: 'resize'; cols: number; rows: number }
	| { type: 'interrupt' };

export type TerminalWsResponse =
	| { type: 'status'; status: string; session: string }
	| { type: 'error'; message: string };

// ── REST auth ──────────────────────────────────────────────────────────────

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
