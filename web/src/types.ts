export type EventKind =
  | 'session_created'
  | 'turn_started'
  | 'thought'
  | 'message'
  | 'tool_call'
  | 'tool_update'
  | 'plan'
  | 'turn_ended'
  | 'error'

export interface UiEvent {
  seq: number
  label: string
  ts_ms: number
  session_id: string | null
  kind: EventKind
  text?: string
  tool_id?: string
  title?: string
  tool_kind?: string
  status?: string
  stop_reason?: string
  message?: string
  entries?: number
}

export interface Agent {
  label: string
  connected: boolean
  session_id: string | null
  turn_started_ms: number | null
  last_activity_ms: number | null
  turns: number
  tool_calls: number
}

export type ServerMessage =
  | { type: 'snapshot'; agents: Agent[]; events: UiEvent[] }
  | ({ type: 'event' } & UiEvent)
  | ({ type: 'agent' } & Agent)

/**
 * A feed row. Consecutive thinking/message chunks are merged into one block —
 * the wire delivers them token by token, which is unreadable one row per chunk.
 */
export interface Block {
  id: number
  label: string
  kind: EventKind
  ts_ms: number
  session_id: string | null
  text: string
  /** tool_call/tool_update only */
  toolId?: string
  toolKind?: string
  status?: string
  /** number of chunks merged, for a subtle density hint */
  chunks: number
}
