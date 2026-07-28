import { signal } from '@preact/signals'
import type { Agent, Block, ServerMessage, UiEvent } from './types'

export const agents = signal<Agent[]>([])
export const blocks = signal<Block[]>([])
export const connected = signal(false)
export const selected = signal<string | null>(null)

const MAX_BLOCKS = 1500
/** Chunks further apart than this start a new block rather than merging. */
const MERGE_WINDOW_MS = 30_000

let nextId = 1

function mergeable(kind: string): boolean {
  return kind === 'thought' || kind === 'message'
}

function eventText(e: UiEvent): string {
  switch (e.kind) {
    case 'thought':
    case 'message':
    case 'turn_started':
      return e.text ?? ''
    case 'tool_call':
      return e.title || e.tool_id || ''
    case 'tool_update':
      return e.tool_id ?? ''
    case 'turn_ended':
      return e.stop_reason ?? ''
    case 'session_created':
      return e.session_id ?? ''
    case 'plan':
      return `${e.entries ?? 0} entries`
    case 'error':
      return e.message ?? ''
    default:
      return ''
  }
}

/** How far back to look for the tool call a `tool_update` belongs to. */
const TOOL_LOOKBACK = 200

/** Fold one event into the block list, merging streamed chunks. */
export function pushEvent(e: UiEvent, list: Block[]): Block[] {
  const text = eventText(e)

  // A tool_update is a status transition on an existing call, not a new line.
  // Agents emit dozens per call; rendering each one buries everything else.
  if (e.kind === 'tool_update' && e.tool_id) {
    const from = Math.max(0, list.length - TOOL_LOOKBACK)
    for (let i = list.length - 1; i >= from; i--) {
      const b = list[i]
      if (b.kind === 'tool_call' && b.toolId === e.tool_id && b.label === e.label) {
        const updated: Block = {
          ...b,
          // The opening tool_call often knows only the tool name; the command
          // itself arrives here once the agent has streamed its arguments.
          text: e.title && e.title !== b.text ? e.title : b.text,
          status: e.status ?? b.status,
          output: e.output ? (b.output ?? '') + e.output : b.output,
          exitCode: e.exit_code ?? b.exitCode,
          ts_ms: e.ts_ms,
          chunks: b.chunks + 1
        }
        return [...list.slice(0, i), updated, ...list.slice(i + 1)]
      }
    }
    // No matching call in view (history trimmed): drop it rather than emit noise.
    return list
  }

  const last = list[list.length - 1]

  if (
    last &&
    mergeable(e.kind) &&
    last.kind === e.kind &&
    last.label === e.label &&
    last.session_id === e.session_id &&
    e.ts_ms - last.ts_ms < MERGE_WINDOW_MS
  ) {
    const merged: Block = {
      ...last,
      text: last.text + text,
      ts_ms: e.ts_ms,
      chunks: last.chunks + 1
    }
    return [...list.slice(0, -1), merged]
  }

  const block: Block = {
    id: nextId++,
    label: e.label,
    kind: e.kind,
    ts_ms: e.ts_ms,
    session_id: e.session_id,
    text,
    toolId: e.tool_id,
    toolKind: e.tool_kind,
    status: e.status,
    chunks: 1
  }

  const next = [...list, block]
  return next.length > MAX_BLOCKS ? next.slice(next.length - MAX_BLOCKS) : next
}

function upsertAgent(a: Agent) {
  const rest = agents.value.filter(x => x.label !== a.label)
  agents.value = [...rest, a].sort((x, y) => x.label.localeCompare(y.label))
}

export function connect() {
  const host = import.meta.env.DEV ? `${location.hostname}:9111` : location.host
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  const ws = new WebSocket(`${proto}://${host}/ws`)

  ws.onopen = () => (connected.value = true)
  ws.onclose = () => {
    connected.value = false
    setTimeout(connect, 1500)
  }
  ws.onmessage = ev => {
    const msg: ServerMessage = JSON.parse(ev.data)
    if (msg.type === 'snapshot') {
      agents.value = [...msg.agents].sort((a, b) => a.label.localeCompare(b.label))
      let list: Block[] = []
      for (const e of msg.events) list = pushEvent(e, list)
      blocks.value = list
    } else if (msg.type === 'event') {
      blocks.value = pushEvent(msg as UiEvent, blocks.value)
    } else if (msg.type === 'agent') {
      upsertAgent(msg as Agent)
    }
  }
}
